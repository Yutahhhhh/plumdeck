#include "audio_clock.h"
#include "ids.h"
#include <algorithm>
#include <chrono>
#include <cmath>
#include <vector>

namespace junction {

qint64 monotonicNanos() {
    using namespace std::chrono;
    return duration_cast<nanoseconds>(steady_clock::now().time_since_epoch()).count();
}

void MediaTimeline::start(qint64 hostMonotonicNanos, quint64 originFrame) {
    if (started_) return;
    t0Nanos_ = hostMonotonicNanos;
    originFrame_ = originFrame;
    started_ = true;
}

void MediaTimeline::adopt(qint64 t0Nanos, quint64 originFrame) {
    t0Nanos_ = t0Nanos;
    originFrame_ = originFrame;
    started_ = true;
}

quint64 MediaTimeline::frameAt(qint64 hostMonotonicNanos) const {
    if (!started_ || hostMonotonicNanos <= t0Nanos_) return originFrame_;
    // Split into whole seconds and a remainder before scaling so a long set
    // does not lose frames to double rounding: at 48 kHz a naive
    // nanos * 48000 / 1e9 in double drifts once the session passes a few hours.
    const quint64 elapsed = static_cast<quint64>(hostMonotonicNanos - t0Nanos_);
    const quint64 seconds = elapsed / 1000000000ULL;
    const quint64 remainder = elapsed % 1000000000ULL;
    return originFrame_ + seconds * kWireSampleRate + (remainder * kWireSampleRate) / 1000000000ULL;
}

qint64 MediaTimeline::nanosAt(quint64 mediaFrame) const {
    if (!started_ || mediaFrame <= originFrame_) return t0Nanos_;
    const quint64 frames = mediaFrame - originFrame_;
    const quint64 seconds = frames / kWireSampleRate;
    const quint64 remainder = frames % kWireSampleRate;
    return t0Nanos_ + static_cast<qint64>(seconds * 1000000000ULL + (remainder * 1000000000ULL) / kWireSampleRate);
}

bool ClockEstimator::add(const ClockProbe& probe) {
    const qint64 rtt = (probe.t4 - probe.t1) - (probe.t3 - probe.t2);
    // A negative RTT means the remote timestamps are inconsistent; a multi
    // second RTT is not a usable synchronisation sample either way.
    if (rtt < 0 || rtt > 5000000000LL) return false;
    if (probe.t4 < probe.t1 || probe.t3 < probe.t2) return false;
    const qint64 offset = ((probe.t2 - probe.t1) + (probe.t3 - probe.t4)) / 2;
    const qint64 localMid = probe.t1 + (probe.t4 - probe.t1) / 2;
    if (samples_.empty()) anchorLocal_ = localMid;
    samples_.push_back({localMid, offset, rtt});
    while (samples_.size() > window_) samples_.pop_front();
    refit();
    return true;
}

void ClockEstimator::refit() {
    if (samples_.empty()) return;
    bestRttNanos_ = samples_.front().rtt;
    for (const Sample& sample : samples_) bestRttNanos_ = std::min(bestRttNanos_, sample.rtt);

    // Keep only the rounds whose RTT is close to the best observed one. A
    // packet that waited in a queue carries that wait straight into its offset
    // estimate, and the median of a congested window is still biased.
    const qint64 cutoff = bestRttNanos_ + std::max<qint64>(bestRttNanos_ / 2, 500000LL);
    std::vector<const Sample*> good;
    good.reserve(samples_.size());
    for (const Sample& sample : samples_) if (sample.rtt <= cutoff) good.push_back(&sample);
    if (good.empty()) return;

    offsetNanos_ = good.back()->offset;
    if (good.size() < kMinSamples) { drift_ = 1.0; anchorLocal_ = good.back()->localMid; return; }

    // Least squares of offset against local time. The slope is the fractional
    // rate difference between the two crystals; a = 1 + slope.
    double sumX = 0, sumY = 0, sumXX = 0, sumXY = 0;
    const double base = static_cast<double>(good.front()->localMid);
    for (const Sample* sample : good) {
        const double x = static_cast<double>(sample->localMid) - base;
        const double y = static_cast<double>(sample->offset);
        sumX += x; sumY += y; sumXX += x * x; sumXY += x * y;
    }
    const double count = static_cast<double>(good.size());
    const double denominator = count * sumXX - sumX * sumX;
    double slope = 0.0;
    if (std::abs(denominator) > 1.0) slope = (count * sumXY - sumX * sumY) / denominator;
    // +/- 1000 ppm covers any real oscillator with margin. Beyond that the fit
    // is describing packet noise, so hold at unity rather than chase it.
    slope = std::clamp(slope, -1.0e-3, 1.0e-3);
    drift_ = 1.0 + slope;
    anchorLocal_ = good.back()->localMid;
    offsetNanos_ = good.back()->offset;
}

qint64 ClockEstimator::toHostNanos(qint64 localNanos) const {
    if (samples_.empty()) return localNanos;
    const double delta = static_cast<double>(localNanos - anchorLocal_) * drift_;
    return anchorLocal_ + static_cast<qint64>(delta) + offsetNanos_;
}

qint64 ClockEstimator::toLocalNanos(qint64 hostNanos) const {
    if (samples_.empty()) return hostNanos;
    const double delta = static_cast<double>(hostNanos - offsetNanos_ - anchorLocal_) / drift_;
    return anchorLocal_ + static_cast<qint64>(delta);
}

void ClockEstimator::reset() {
    samples_.clear();
    drift_ = 1.0;
    offsetNanos_ = 0;
    bestRttNanos_ = 0;
    anchorLocal_ = 0;
}

void AudioClockAnchor::observe(quint64 deviceFrames, qint64 monotonicNs) {
    if (observations_ == 0) {
        firstFrames_ = deviceFrames;
        firstNanos_ = monotonicNs;
        measuredRateHz_ = nominalRateHz_;
    }
    lastFrames_ = deviceFrames;
    lastNanos_ = monotonicNs;
    ++observations_;
    const qint64 elapsed = lastNanos_ - firstNanos_;
    // A short window measures scheduling jitter, not the crystal. One second is
    // enough for a coarse rate and the estimate keeps tightening after that.
    if (elapsed >= 1000000000LL && lastFrames_ > firstFrames_)
        measuredRateHz_ = static_cast<double>(lastFrames_ - firstFrames_) * 1.0e9 / static_cast<double>(elapsed);
}

double AudioClockAnchor::driftPpm() const {
    if (nominalRateHz_ == 0 || measuredRateHz_ <= 0.0) return 0.0;
    return (measuredRateHz_ / static_cast<double>(nominalRateHz_) - 1.0) * 1.0e6;
}

void AudioClockAnchor::reset() {
    observations_ = 0;
    firstFrames_ = lastFrames_ = 0;
    firstNanos_ = lastNanos_ = 0;
    measuredRateHz_ = 0.0;
}

double AsrcController::update(double fillFrames) {
    double error = fillFrames - config_.targetFillFrames;
    if (std::abs(error) <= config_.deadbandFrames) error = 0.0;
    else error -= (error > 0 ? config_.deadbandFrames : -config_.deadbandFrames);
    integrator_ += error;
    // Clamp the integrator to the span it could ever need, so a long stall does
    // not wind it up into a correction that takes minutes to unwind.
    const double integratorLimit = config_.maxRatioDeviation / std::max(config_.integralGain, 1.0e-15);
    integrator_ = std::clamp(integrator_, -integratorLimit, integratorLimit);
    // Negative feedback: a ring above target must be drained, which means
    // consuming *more* input per output frame, which is a ratio below 1.0.
    const double correction = config_.proportionalGain * error + config_.integralGain * integrator_;
    ratio_ = std::clamp(1.0 - correction, 1.0 - config_.maxRatioDeviation, 1.0 + config_.maxRatioDeviation);
    return ratio_;
}

void AsrcController::reset() {
    ratio_ = 1.0;
    integrator_ = 0.0;
}

} // namespace junction
