#pragma once
#include "deck_telemetry.h"
#include "scratch_prediction.h"

#include <algorithm>
#include <atomic>
#include <array>
#include <chrono>
#include <QJsonArray>
#include <QJsonObject>
#include <cmath>
#include "control/controlobject.h"
#include "engine/channels/enginedeck.h"
#include "engine/enginebuffer.h"

// Main-thread commands cross through a fixed SPSC trajectory ring.
// The callback consumes at most 64 records and never waits or allocates.
// Overflow aborts the affected generation; a fresh touch may start a new one.
class ScratchDeck final : public EngineDeck {
    struct Request { unsigned sequence,generation;bool enabled,finish,keepalive;double samples,sampleRate,at; quint64 loadGeneration,inputSequence; };
    deckclock::Ring<Request,1024> requests_;
    std::atomic<unsigned> overflowGeneration_{0};
    unsigned blockedGeneration_=0;
    quint64 appliedNativeInput_=0;
public:
    ScratchDeck(const ChannelHandleAndGroup& handleGroup, UserSettingsPointer settings,
            EngineMixer* mixer, EffectsManager* effects, ChannelOrientation orientation)
            : EngineDeck(handleGroup, settings, mixer, effects, orientation, true),
              enable_(ControlObject::getControl(ConfigKey(handleGroup.name(), "scratch2_enable"))),
              position_(ControlObject::getControl(ConfigKey(handleGroup.name(), "scratch2"))),
              play_(ControlObject::getControl(ConfigKey(handleGroup.name(), "play"))),
              tempo_(ControlObject::getControl(ConfigKey(handleGroup.name(), "rate_ratio"))) {
        // These two controls are private to the headless scratch adapter and
        // only polled by RateControl. Disable Qt signal dispatch before audio
        // starts, then use setAndConfirm (atomic value write, no behavior lookup
        // or signal allocations/locks) in the callback. No UI/MIDI subscribers.
        ControlDoublePrivate::getControl(ConfigKey(handleGroup.name(), "scratch2"))->blockSignals(true);
        ControlDoublePrivate::getControl(ConfigKey(handleGroup.name(), "scratch2_enable"))->blockSignals(true);
    }

    void requestScratch(bool enabled, bool begin, double samples, double sourceSampleRate, bool finish = true, double capturedNativeUs = 0, bool keepalive = false, quint64 inputSequence = 0) {
        if (begin) generation_.fetch_add(1);
        const unsigned sequence = sequence_.fetch_add(2) + 2;
        const Request request{sequence, generation_.load(), enabled, finish, keepalive, samples, sourceSampleRate,
            capturedNativeUs > 0 ? capturedNativeUs / 1000.0 : monotonicMs(), clockGeneration_.load(),inputSequence};
        if (!requests_.push(request, enabled && !begin ? 64 : 0)) overflowGeneration_.store(request.generation);
    }

    bool scratching() const { return observedScratching_.load(); }

    QJsonObject audioDiagnostics() const {
        return {{"buffers", static_cast<double>(diagnosticBuffers_.load())},
                {"silentMovingBuffers", static_cast<double>(diagnosticSilent_.load())},
                {"positionMs", diagnosticPosition_.load()},
                {"speed", diagnosticSpeed_.load()}, {"preFaderPeak", diagnosticPeak_.load()},
                {"inputAgeMs", diagnosticAge_.load()}, {"scratching", scratching()}};
    }

    // Optional SPSC diagnostic ring. The callback never waits for its reader.
    QJsonObject timingTrace() {
        QJsonArray rows;
        auto read = traceRead_.load();
        const auto end = traceWrite_.load(std::memory_order_acquire);
        while (read != end) {
            const auto& row = trace_[read % trace_.size()];
            rows.append(QJsonObject{{"audioMs", row.audioMs}, {"positionMs", row.positionMs},
                {"speed", row.speed}, {"held", row.held}, {"scratching", row.scratching},
                {"targetMs", row.targetMs}, {"callbackMs", row.callbackMs}, {"commandedSpeed", row.commandedSpeed},
                {"requestAgeMs", row.requestAgeMs}});
            ++read;
        }
        traceRead_.store(read, std::memory_order_release);
        return {{"rows", rows}, {"dropped", static_cast<double>(traceDropped_.load())}};
    }

    void setLoadGeneration(quint64 generation) { clockGeneration_.store(generation); }
    QJsonObject clockPoints() {
        QJsonArray points; deckclock::Point p;
        while (clockPoints_.pop(p)) points.append(QJsonObject{
            {"schema", 2}, {"sequence", double(p.sequence)}, {"audioConfigEpoch", double(p.config)},
            {"outputFrameEnd", double(p.outputEnd)}, {"outputFrames", int(p.frames)},
            {"outputSampleRateHz", p.outputRate}, {"sourceFrameStart", p.start}, {"sourceFrameEnd", p.end},
            {"sourceSampleRateHz", p.sourceRate}, {"nativeMonoUs", p.nativeUs},
            {"loadGeneration", double(p.generation)}, {"trajectoryEpoch", double(p.trajectory)},
            {"velocityRatioMean", p.velocity}, {"velocityRatioEnd", QJsonValue::Null},
            {"transportPlaying", p.playing}, {"scratching", p.scratching},
            {"appliedInputSeq", double(p.applied)}, {"inputOrigin",p.nativeInput?"native-midi":"control"},
            {"timestampReference", "deck-postprocess-observed"},
            {"discontinuity", p.boundary ? QJsonValue(p.boundary == 2 ? "loop-wrap" : "seek") : QJsonValue(QJsonValue::Null)},
            {"interpolationSafe", p.boundary == 0}});
        return {{"points", points}, {"dropped", int(clockPoints_.dropped())}};
    }
    void process(CSAMPLE* output, const int bufferSize) override {
        clockBoundary_.kind = 0;
        deckclock::currentBoundary = &clockBoundary_;
        bool grabbed = false;
        const auto overflow = overflowGeneration_.exchange(0);
        if (overflow) { blockedGeneration_ = overflow; enabled_ = false; baselineValid_ = false; releaseRamp_ = false; }
        if(appliedLoadGeneration_!=clockGeneration_.load(std::memory_order_acquire)){enabled_=false;baselineValid_=false;releaseRamp_=false;}
        Request request;
        for (unsigned consumed = 0; consumed < 64 && requests_.pop(request); ++consumed) {
            if(request.loadGeneration!=clockGeneration_.load(std::memory_order_acquire))continue;
            if (blockedGeneration_ && request.generation <= blockedGeneration_ && request.enabled) continue;
            const auto sequence = request.sequence;
            const auto generation = request.generation;
            const auto enabled = request.enabled;
            const auto samples = request.samples;
            const auto sampleRate = request.sampleRate;
            const auto finish = request.finish;
            const auto requestedAt = request.at;
            const auto keepalive = request.keepalive;
            {
                consumedSequence_ = sequence;
                inputAgeMs_ = 0;
                if (generation != currentGeneration_) {
                    grabbed = true;
                    currentGeneration_ = generation;
                    baselineValid_ = false;
                    releaseRamp_ = false;
                    catchUp_ = 0;
                    motionAgeMs_ = 0;
                    velocityFrames_ = 0;
                    inputIntervalMs_ = 0;
                    samples_ = samples;
                    motionAtMs_ = requestedAt;
                }
                enabled_ = enabled;
                if (keepalive) {
                    // Contact lease refresh is not an observed stop.
                } else if (samples != samples_) {
                    // 入力が届いた実時刻の差で手の速度を測る。ハートビートは
                    // 位置を変えないので間隔に混ぜない。異常に長い間隔（曲の
                    // 頭出し待ちなど）は速度ではないので推定を捨てる。
                    const double interval = requestedAt - motionAtMs_;
                    if (interval > 120) { velocityFrames_ = 0; inputIntervalMs_ = 0; }
                    else if (interval >= 1) {
                        // 間隔は伸びに即応し、縮むときだけゆっくり戻す。UI が
                        // 詰まって急に間隔が伸びたとき、谷間の途中で外挿が
                        // 切れて音が痩せるのを防ぐ。
                        inputIntervalMs_ = std::max(interval, inputIntervalMs_ + .15 * (interval - inputIntervalMs_));
                        const double measured = (samples - samples_) / 2.0 / interval;
                        // A direction change is intentional, not timing noise.
                        // Averaging opposite velocities delays every cut.
                        if (measured * velocityFrames_ < 0) velocityFrames_ = measured;
                        else velocityFrames_ += .5 * (measured - velocityFrames_);
                    }
                    motionAtMs_ = requestedAt;
                    motionAgeMs_ = 0;
                } else if (baselineValid_) {
                    // 位置が変わらない入力が届いた＝手は止まっている。外挿を
                    // 素早く畳む。入力そのものが途切れている場合（UI が詰まった
                    // だけかもしれない）とは区別し、そちらは繋ぎ続ける。
                    velocityFrames_ *= .35;
                }
                samples_ = samples;
                sourceSampleRate_ = sampleRate;
                appliedLoadGeneration_ = request.loadGeneration;
                appliedNativeInput_ = request.inputSequence;
                finish_ = finish;
                requestAtMs_ = requestedAt;
            }
        }
        const double outputRate = std::max(1.0, m_sampleRate.get());
        const double callbackMs = bufferSize * 500.0 / outputRate;
        inputAgeMs_ += callbackMs;
        motionAgeMs_ += callbackMs;
        // Also expire in audio time: stdout backpressure can stall the Qt
        // watchdog, but must never leave a clientless platter held forever.
        if (inputAgeMs_ >= 1500) enabled_ = false;
        // RateControl's scratch2 path has no position-controller sampling delay
        // or throw. Close the position loop once per callback; Mixxx's linear
        // scaler interpolates rates within the buffer, preserving PCM continuity.
        const double frames = getEngineBuffer()->getExactPlayPos().value();
        const double framesPerBuffer = callbackMs * sourceSampleRate_ / 1000.0;
        if (!enabled_ && baselineValid_ && finish_) {
            // Complete the final pointer displacement with rate interpolation,
            // never a seek or a held platter. Account for BOTH the correction
            // buffer and the following ramp to transport speed. The linear
            // scaler uses two half-buffer ramps when changing direction.
            const double normal = play_->get() > 0 ? tempo_->get() : 0;
            const double previous = getEngineBuffer()->getSpeed();
            const double distance = (baselineFrames_ + samples_ / 2.0 - frames) / framesPerBuffer + 2 * normal;
            // Do not select an almost-zero intermediate rate while playing:
            // the reverse-to-forward scaler would spend half the next buffer
            // effectively stopped. A small minimum trades a small position error
            // for continuous audible motion through the release.
            const double minimum = normal > 0 ? .25 : .000001;
            double best = 0, bestError = 1e30;
            for (const double sign : {-1.0, 1.0}) {
                const double first = previous * sign < 0 ? .25 : .5;
                const double second = normal * sign < 0 ? .25 : .5;
                const double candidate = std::clamp((distance - first * previous - second * normal) / (first + second),
                    sign < 0 ? -16.0 : minimum, sign < 0 ? -minimum : 16.0);
                const double error = std::abs(first * (previous + candidate) + second * (candidate + normal) - distance);
                if (error < bestError) { bestError = error; best = candidate; }
            }
            // 予測で先に出た距離が 1 バッファの権限（±16 倍）を超えると、
            // 着地レートが上限に張り付いて戻し切れない。そこは上限のまま次の
            // コールバックへ持ち越し、権限内に入ってから通常の着地計算に渡す。
            // 30ms の復帰予算に収めるため持ち越しは 1 バッファまで。
            if (bestError > .5 && catchUp_ < 1) {
                ++catchUp_;
                position_->setAndConfirm(distance > 0 ? 16.0 : -16.0);
                enable_->setAndConfirm(1);
            } else {
                catchUp_ = 0;
                position_->setAndConfirm(best);
                enable_->setAndConfirm(1);
                releaseRamp_ = true;
                baselineValid_ = false;
            }
        } else if (!enabled_) {
            // Finish on the SAME linear scaler before handing back to keylock.
            // Otherwise a keylock scaler switch bypasses the final rate ramp.
            // Paused transport also needs this callback to ramp down to zero.
            const bool finalRamp = releaseRamp_ && finish_;
            position_->setAndConfirm(finalRamp && play_->get() > 0 ? tempo_->get() : 0);
            enable_->setAndConfirm(finalRamp ? 1 : 0);
            releaseRamp_ = false;
            baselineValid_ = false;
        } else {
            releaseRamp_ = false;
            if (!baselineValid_) {
                baselineFrames_ = frames;
                baselineValid_ = true;
            }
            // 入力は数十msおきの階段なので、その値をそのまま目標にすると、
            // パケット到着でレートが跳ね上がり、次の到着まで減衰する鋸歯に
            // なる（実測で速度が ±20〜60% 揺れ、低音の輪郭が濁った）。手の
            // 速度でパケットの谷間を外挿して目標を連続にし、同じ速度を送り
            // 項にも足して追従遅れを消す。入力が途切れたら外挿を 0 へ戻し、
            // 手を止めたのに滑り続けないようにする。時間軸はコールバック数
            // ではなく実時刻で測る。5.805ms 刻みでは入力間隔と位相が合わず、
            // 埋めたはずの谷間に段差が残る。
            const double window = std::clamp(inputIntervalMs_, 8.0, 60.0);
            const double age = std::max(0.0, monotonicMs() - motionAtMs_);
            const bool stoppedInput = age >= 3 * window;
            if (stoppedInput) velocityFrames_ = 0;
            const auto prediction = scratch::predict(velocityFrames_, age, window);
            const double lead = prediction.position;
            // The scaler ramps to this command over the output block. Supply
            // the trajectory's end velocity, not its already elapsed start
            // velocity, or the return segment gains another buffer of lag.
            const double feed = scratch::predict(velocityFrames_, age + callbackMs, window).velocity * 1000.0 / sourceSampleRate_;
            const double error = baselineFrames_ + samples_ / 2.0 + lead - frames;
            // Near-zero reversals in the resampler otherwise form an audible
            // ~1ms limit cycle (especially 48k sources on a 44.1k output).
            // Apply the silence deadband only after motion has stopped. During
            // fine continuous movement a 1ms deadband gates most callbacks to
            // zero, turning a slow scratch into an audible train of impulses.
            // Keep the loop damped during motion too, avoiding a gain step as
            // MIDI packets age. Heartbeats do not reset actual motion age.
            // 入力が 30ms 以上途切れただけでは黙らせない。UI スレッドが詰まって
            // 位置更新が遅れると、手は回っているのにレートが 0 に落ちて音が
            // 切れる。手が実際に止まっている（推定速度がほぼ 0）ときだけ黙る。
            const bool settled = motionAgeMs_ > 30 && std::abs(error) < sourceSampleRate_ * .001
                && std::abs(velocityFrames_) * 1000 < sourceSampleRate_ * .02;
            // Feed-forward follows the hand; position feedback only removes
            // accumulated drift. A strong per-buffer correction amplifies
            // packet/OS timing jitter into audible pitch modulation.
            const double positionGain = 1 - std::pow(1 - (stoppedInput ? .35 : .08), callbackMs / (256000.0 / 44100));
            const double target = std::clamp(feed + positionGain * error / framesPerBuffer, -16.0, 16.0);
            const double previous = getEngineBuffer()->getSpeed();
            const double smoothingMs = stoppedInput || target * previous < 0 ? 2.0 : 10.0;
            const double blend = callbackMs / (smoothingMs + callbackMs);
            position_->setAndConfirm(settled || (grabbed && samples_ == 0) ? 0 : previous + blend * (target - previous));
            enable_->setAndConfirm(1);
        }
        beforeFrames_ = getEngineBuffer()->getExactPlayPos().value();
        requestAgeMs_ = traceEnabled_ ? monotonicMs() - requestAtMs_ : 0;
        EngineDeck::process(output, bufferSize);
        deckclock::currentBoundary = nullptr;
        const bool scratching = getEngineBuffer()->getScratching();
        observedScratching_.store(scratching);
        if (scratching) {
            // Meter only, no audio samples leave the callback. The counters
            // catch short dropouts that a main-thread snapshot can miss.
            double peak = 0;
            for (int i = 0; i < bufferSize; ++i) peak = std::max(peak, std::abs(static_cast<double>(output[i])));
            const double speed = getEngineBuffer()->getSpeed();
            diagnosticPeak_.store(peak);
            diagnosticSpeed_.store(speed);
            diagnosticPosition_.store(getEngineBuffer()->getExactPlayPos().value() * 1000.0 / sourceSampleRate_);
            diagnosticAge_.store(inputAgeMs_);
            if (std::abs(speed) > .1 && peak < 1e-7) diagnosticSilent_.fetch_add(1);
            diagnosticBuffers_.fetch_add(1);
        }
    }

    void postProcess(const int bufferSize) override {
        EngineDeck::postProcess(bufferSize);
        const auto exact = getEngineBuffer()->getExactPlayPos();
        const double callbackMs = bufferSize * 500.0 / std::max(1.0, m_sampleRate.get());
        const bool scratching = getEngineBuffer()->getScratching();
        traceAudioMs_ += callbackMs;
        if (exact.isValid() && sourceSampleRate_ > 0) {
            const double mean = clockBoundary_.kind ? 0 : (exact.value() - beforeFrames_) * 1000.0 / sourceSampleRate_ / callbackMs;
            clockPoints_.push({++clockSequence_, deckclock::audioConfigEpoch, deckclock::outputFrameEnd,
                appliedLoadGeneration_, clockBoundary_.epoch, appliedNativeInput_ ? appliedNativeInput_ : consumedSequence_ / 2,
                beforeFrames_, exact.value(), sourceSampleRate_, m_sampleRate.get(), deckclock::monotonicUs(), mean,
                unsigned(bufferSize / 2), clockBoundary_.kind, play_->get() > 0, scratching, appliedNativeInput_ != 0});
        }
        if (traceEnabled_) {
            const auto write = traceWrite_.load();
            if (write - traceRead_.load(std::memory_order_acquire) < trace_.size()) {
                const auto pos = getEngineBuffer()->getExactPlayPos();
                trace_[write % trace_.size()] = {traceAudioMs_,
                    pos.isValid() ? pos.value() * 1000.0 / sourceSampleRate_ : -1,
                    pos.isValid() ? (pos.value() - beforeFrames_) * 1000.0 / sourceSampleRate_ / callbackMs : 0, callbackMs,
                    getEngineBuffer()->getSpeed(),
                    (baselineFrames_ + samples_ / 2.0) * 1000.0 / sourceSampleRate_, requestAgeMs_, enabled_, scratching};
                traceWrite_.store(write + 1, std::memory_order_release);
            } else traceDropped_.fetch_add(1);
        }
    }

private:
    deckclock::Ring<deckclock::Point, 256> clockPoints_;
    deckclock::Boundary clockBoundary_;
    std::atomic<quint64> clockGeneration_{0};
    quint64 clockSequence_ = 0, appliedLoadGeneration_ = 0;
    static double monotonicMs() { return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now().time_since_epoch()).count(); }
    struct TraceRow { double audioMs, positionMs, speed, callbackMs, commandedSpeed, targetMs, requestAgeMs; bool held, scratching; };
    const bool traceEnabled_ = qEnvironmentVariableIntValue("PLUMDECK_MIXXX_TIMING_TRACE") == 1;
    std::array<TraceRow, 8192> trace_{};
    std::atomic<unsigned> traceWrite_{0}, traceRead_{0}, traceDropped_{0};
    double traceAudioMs_ = 0, beforeFrames_ = 0, requestAtMs_ = 0, requestAgeMs_ = 0;
    static_assert(std::atomic<double>::is_always_lock_free);
    static_assert(std::atomic<unsigned>::is_always_lock_free);
    static_assert(std::atomic<bool>::is_always_lock_free);
    ControlObject* const enable_;
    ControlObject* const position_;
    ControlObject* const play_;
    ControlObject* const tempo_;
    std::atomic<unsigned> sequence_{0}, generation_{0};
    std::atomic<bool> observedScratching_{false};
    std::atomic<unsigned> diagnosticBuffers_{0}, diagnosticSilent_{0};
    std::atomic<double> diagnosticPeak_{0}, diagnosticSpeed_{0}, diagnosticPosition_{0}, diagnosticAge_{0};

    unsigned currentGeneration_ = 0, consumedSequence_ = 0;
    bool enabled_ = false, baselineValid_ = false, finish_ = true, releaseRamp_ = false;
    double samples_ = 0, sourceSampleRate_ = 44100, inputAgeMs_ = 0, motionAgeMs_ = 0, baselineFrames_ = 0;
    // ソースフレーム/ms で表した手の速度、その推定に使った入力間隔、直近の
    // 位置入力が届いた時刻。catchUp_ は解放時に権限を持ち越したバッファ数。
    double velocityFrames_ = 0, inputIntervalMs_ = 0, motionAtMs_ = 0;
    int catchUp_ = 0;
};
