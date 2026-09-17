#include "deck_telemetry.h"
#include "host.h"
#include <QDir>
#include <QJsonArray>
#include <QJsonDocument>
#include <QSet>
#include <QVector>
#include <QUuid>
#include <cmath>
#include <cstdio>
#include <limits>
#include <optional>

namespace {
constexpr double kMaxSafeId = 9007199254740991.0;
bool integer(const QJsonValue& value) {
    return value.isDouble() && value.toDouble() >= 0 && value.toDouble() <= kMaxSafeId && std::floor(value.toDouble()) == value.toDouble();
}
bool validGrid(const QJsonValue& bpm, const QJsonValue& offset, const QJsonValue& meter, double duration) {
    return bpm.isDouble() && std::isfinite(bpm.toDouble()) && bpm.toDouble() >= 20 && bpm.toDouble() <= 300 &&
            offset.isDouble() && std::isfinite(offset.toDouble()) && offset.toDouble() >= 0 && offset.toDouble() < duration - 0.000001 &&
            integer(meter) && meter.toDouble() >= 1 && meter.toDouble() <= 16;
}
bool validMeter(const QJsonValue& meter) {
    return integer(meter) && meter.toDouble() >= 1 && meter.toDouble() <= 16;
}
bool parseBeatTimes(const QJsonValue& value, double duration, QVector<double>* values = nullptr) {
    if (!value.isArray()) return false;
    const auto array = value.toArray();
    if (array.size() < 2 || array.size() > 100000) return false;
    QVector<double> parsed;
    if (values) parsed.reserve(array.size());
    double previous = -1.0;
    for (const auto& item : array) {
        if (!item.isDouble()) return false;
        const double time = item.toDouble();
        if (!std::isfinite(time) || time < 0 || time <= previous || time >= duration) return false;
        previous = time;
        if (values) parsed.append(time);
    }
    if (values) *values = std::move(parsed);
    return true;
}
bool validBeatNumbers(const QJsonValue& value, int expectedSize, int beatsPerBar) {
    if (!value.isArray()) return false;
    const auto array = value.toArray();
    if (array.size() != expectedSize) return false;
    for (const auto& item : array) if (!integer(item) || item.toDouble() < 1 || item.toDouble() > beatsPerBar) return false;
    return true;
}
bool validOptionalBpm(const QJsonValue& value) {
    return value.isUndefined() || value.isNull() ||
            (value.isDouble() && std::isfinite(value.toDouble()) && value.toDouble() >= 20 && value.toDouble() <= 300);
}
bool validOptionalOffset(const QJsonValue& value, double duration) {
    return value.isUndefined() ||
            (value.isDouble() && std::isfinite(value.toDouble()) && value.toDouble() >= 0 && value.toDouble() < duration);
}
std::optional<QVector<double>> descriptorBeatTimes(const QJsonObject& descriptor, double duration) {
    if (!descriptor.contains("beatTimesMs")) return std::nullopt;
    QVector<double> values;
    if (!parseBeatTimes(descriptor["beatTimesMs"], duration, &values)) return std::nullopt;
    return values;
}
QJsonValue effectiveBpmValue(double bpm) {
    return std::isfinite(bpm) && bpm > 0 ? QJsonValue(bpm) : QJsonValue(QJsonValue::Null);
}
bool validCues(const QJsonValue& value, double duration) {
    if (!value.isArray() || (value.toArray().size() != 8 && value.toArray().size() != 16)) return false;
    for (const auto cue : value.toArray()) if (!cue.isNull() && (!cue.isDouble() || !std::isfinite(cue.toDouble()) || cue.toDouble() < 0 || cue.toDouble() >= duration)) return false;
    return true;
}
// The decoded duration is authoritative and is routinely shorter than the
// catalog duration a saved grid was validated against, because decoders drop
// the encoder delay and padding of MP3/AAC files. Beats behind the decoded end
// can never be reached, so keep the usable prefix instead of refusing the
// whole grid. Callers must have checked the shape first: this only cuts the
// tail off an already strictly increasing list. False means fewer than two
// beats survive, which is a real mismatch rather than a trimmed tail.
bool trimBeatsToDuration(QJsonValue& times, QJsonValue& numbers, double duration) {
    auto beats = times.toArray();
    int keep = 0;
    while (keep < beats.size() && beats[keep].toDouble() < duration) ++keep;
    if (keep == beats.size()) return true;
    if (keep < 2) return false;
    while (beats.size() > keep) beats.removeLast();
    times = beats;
    if (numbers.isArray()) {
        auto bars = numbers.toArray();
        while (bars.size() > keep) bars.removeLast();
        numbers = bars;
    }
    return true;
}
// A saved cue behind the decoded end is unreachable for the same reason. Empty
// that slot only on this deck; the stored cue stays in the library untouched.
QJsonArray cuesWithinDuration(const QJsonValue& value, double duration) {
    auto cues = value.toArray();
    for (int slot = 0; slot < cues.size(); ++slot) if (!cues[slot].isNull() && cues[slot].toDouble() >= duration) cues[slot] = QJsonValue::Null;
    return cues;
}
const QSet<QString> samplerOps = {"sampler.bank","sampler.state", "sampler.load", "sampler.eject", "sampler.play", "sampler.stop", "sampler.stopAll", "sampler.gain", "sampler.pfl"};
const QSet<QString> known = {"deck.load", "deck.unload", "deck.play", "deck.pause", "deck.seek", "deck.tempo.set", "deck.keylock.set", "deck.sync.set", "deck.beatgrid.set", "deck.hotcue.set", "deck.hotcue.jump", "deck.hotcue.clear", "deck.loop.set", "deck.loop.enable", "mixer.channel.gain", "mixer.channel.eq", "mixer.channel.pfl", "mixer.crossfader", "mixer.master.gain", "audio.devices.list", "audio.config.get", "audio.config.set", "meters.subscribe", "recording.start", "recording.stop", "recording.directory.set", "recording.format.set"};
const QSet<QString> transport = {"deck.load", "deck.unload", "deck.play", "deck.pause", "deck.seek", "deck.pitchbend", "deck.scratch"};
const QSet<QString> deckControls = {"deck.key.shift", "deck.key.sync", "deck.key.reset", "deck.slip.set", "deck.reverse.set", "deck.slipReverse.set","deck.tempo.set", "deck.keylock.set", "deck.sync.set", "deck.beatgrid.set", "deck.hotcue.set", "deck.hotcue.jump", "deck.hotcue.clear", "deck.loop.set", "deck.loop.enable", "deck.loop.beats", "deck.beatjump", "deck.quantize.set"};
// Builtin Mixxx effects usable as performance pads. Each name is loaded as
// org.mixxx.effects.<name>; keep this in sync with PAD_EFFECTS on the client.
const QSet<QString> padEffects = {"echo", "reverb", "flanger", "phaser", "filter", "bitcrusher", "distortion", "autopan", "tremolo", "moogladder4filter"};
const QSet<QString> mixerOps = {"mixer.colorfx.set","mixer.beatfx.set","mixer.channel.orientation", "mixer.channel.pfl", "mixer.channel.gain", "mixer.crossfader", "mixer.master.gain", "mixer.channel.eq", "mixer.eq.set", "mixer.filter.set", "mixer.trim.set", "mixer.fx.set"};
const QString deckNames[] = {"A", "B", "C", "D"};
int deckIndex(const QString& name) {
    for (int index = 0; index < 4; ++index) if (deckNames[index] == name) return index;
    return -1;
}
}

Host::Host(std::unique_ptr<PlaybackBackend> backend) : backend_(std::move(backend)), engineId_(QUuid::createUuid().toString(QUuid::WithoutBraces)) {
    junction_ = std::make_unique<junction::Runtime>(backend_.get(), this);
    backend_->attachJunction(junction_.get());
    performanceInput_=std::make_unique<PerformanceInput>(backend_.get(),[this](int i){return slots_[i].state;},[this]{return backend_->available()&&!junction_->active()&&!sessionId_.isEmpty();},this);
    clock_.start();
    for (int index = 0; index < 4; ++index) resetDeck(index);
    // Junction: this computer's loaded decks, for J metadata, READY status
    // and the OUTGOING tail. Never a path.
    junction_->localDeckTracks = [this] {
        QJsonArray rows;
        const auto mixer = backend_->mixer();
        const auto channels = mixer["channels"].toObject();
        const double crossfader = mixer["crossfader"].toDouble();
        for (int index = 0; index < 4; ++index) {
            const auto& slot = slots_[index];
            const auto track = slot.state["track"].toObject();
            if (track.isEmpty() || slot.descriptor["path"].toString().isEmpty() || slot.state["status"] == "loading") continue;
            const auto channel = channels[deckNames[index]].toObject();
            const double orientation = channel["orientation"].toDouble();
            const double crossGain = orientation < 0.5 ? (1.0 - crossfader) * 0.5
                    : orientation > 1.5 ? (1.0 + crossfader) * 0.5 : 1.0;
            const double audibility = qMax(0.0, channel["gain"].toDouble() * channel["trim"].toDouble(1.0) * crossGain);
            rows.append(QJsonObject{{"deck", deckNames[index]}, {"title", track["title"].toString()}, {"artist", track["artist"].toString()},
                {"durationMs", track["durationMs"].toDouble()}, {"bpm", track["bpm"].toDouble()},
                {"positionMs", backend_->positionMs(index)}, {"rate", backend_->playbackRate(index)}, {"audibility", audibility},
                {"playing", backend_->playing(index)},
                {"firstBeatMs", track.contains("beatgridOffsetMs") ? track["beatgridOffsetMs"] : QJsonValue(QJsonValue::Null)}, {"pfl", channel["pfl"].toBool()}});
        }
        return rows;
    };
    backend_->loaded = [this](int index, quint64 generation, QJsonObject metadata, QString error) {
        // Even immediate decoder failures are delivered after the accepted reply.
        QTimer::singleShot(0, this, [this, index, generation, metadata, error] { completed(index, generation, metadata, error); });
    };
    backend_->recordingChanged = [this](QJsonObject) {
        QTimer::singleShot(0, this, [this] { ++rev_; event("recording.state", recordingState()); });
    };
    connect(&timer_, &QTimer::timeout, this, [this] { sample(); });
    timer_.start(20);
}
void Host::resetDeck(int index) {
    releaseScratch(index, false);
    ++slots_[index].reverseGesture;
    slots_[index].state = emptyDeck(); slots_[index].state["deck"] = deckNames[index];
    slots_[index].state["scratching"] = false;
    slots_[index].transportTouched = false;
    slots_[index].state["loadGeneration"] = double(slots_[index].generation);
    slots_[index].state["available"] = backend_->implementation() == "mixxx";
}
/** Mirror the engine's sync leader onto every deck, and announce the decks that
 *  changed. The leader is engine-wide state, so a deck other than the one being
 *  commanded can lose or gain it. `commanded` is emitted by the caller. */
void Host::publishSyncLeader(int commanded) {
    const int leader = backend_->syncLeader();
    for (int index = 0; index < 4; ++index) {
        const QJsonValue value = leader < 0 ? QJsonValue(QJsonValue::Null) : QJsonValue(deckNames[index]);
        const QJsonValue next = index == leader ? value : QJsonValue(QJsonValue::Null);
        if (slots_[index].state["syncLeader"] == next) continue;
        slots_[index].state["syncLeader"] = next;
        if (index != commanded) event("deck.state", slots_[index].state);
    }
}
void Host::releaseScratch(int index, bool finish) {
    auto& slot = slots_[index];
    if (!slot.scratchGesture.isEmpty()) {
        backend_->scratch(index, finish ? "end" : "abort", slot.scratchLastPositionMs);
    }
    slot.scratchGesture.clear();
    slot.scratchLastPositionMs = 0;
}
QJsonObject Host::emptyDeck() {
    return {{"deck", "A"}, {"status", "empty"}, {"track", QJsonValue::Null}, {"positionMs", 0}, {"positionFrames", 0}, {"rate", 1.0}, {"keylock", false}, {"syncEnabled", false}, {"syncLeader", QJsonValue::Null}, {"effectiveBpm", QJsonValue::Null}, {"hotCues", QJsonArray{QJsonValue::Null, QJsonValue::Null, QJsonValue::Null, QJsonValue::Null, QJsonValue::Null, QJsonValue::Null, QJsonValue::Null, QJsonValue::Null}}, {"loopRegion", QJsonValue::Null}, {"lastError", QJsonValue::Null}, {"loadId", QJsonValue::Null}};
}
QJsonObject Host::info() const {
    QJsonArray capabilities;
    if (backend_->available()) capabilities = {"sampler", "mixer.beatfx", "mixer.colorfx", "deck.key", "deck.slip", "deck.reverse", "deck.load.async", "deck.transport", "deck.scratch", "deck.pitchbend", "deck.tempo", "deck.keylock", "deck.sync", "deck.beatgrid", "deck.hotcue", "deck.loop", "deck.beatjump", "deck.quantize", "mixer.eq", "mixer.filter", "mixer.trim", "mixer.fx", "mixer.gain", "mixer.crossfader", "recording", "audio.microphone", "audio.microphone.ducking"};
    if (backend_->audio()["pflApplied"].toBool()) capabilities.append("mixer.pfl");
    if (backend_->available()) { capabilities.append("mixer.beatfx.release"); capabilities.append("deck.clock.v2"); capabilities.append("performance.midi.v2"); capabilities.append("waveform.tiles.v2"); }
    return {{"name", backend_->implementation() == "mixxx" ? "plumdeck-mixxx-engine-host" : "plumdeck-mixxx-host-unavailable"}, {"version", "0.3.0"}, {"implementation", backend_->implementation()}, {"simulated", false}, {"deterministic", false}, {"audioAvailable", backend_->available()}, {"audioProblem", backend_->problem()}, {"decks", QJsonArray{"A", "B", "C", "D"}}, {"capabilities", capabilities}, {"upstreamCommit", "3ebac449e7e5fe2a0186596657696e87ce8b0e56"}};
}
QJsonObject Host::envelope(const QString& kind) const {
    return {{"protocol", 1}, {"kind", kind}, {"engineId", engineId_}, {"rev", static_cast<qint64>(rev_)}, {"engineTimeMs", static_cast<double>(clock_.elapsed())}};
}
void Host::send(QJsonObject message) const {
    const QByteArray bytes = QJsonDocument(message).toJson(QJsonDocument::Compact) + '\n';
    if (std::fwrite(bytes.constData(), 1, bytes.size(), stdout) != static_cast<size_t>(bytes.size()) || std::fflush(stdout) != 0) std::exit(1);
}
Host::~Host() {
    // Stop while the backend is alive, then keep Runtime storage in place
    // until backend destruction has joined the audio callback.
    if (junction_) junction_->detachBackend();
    backend_.reset();
    junction_.reset();
}
void Host::result(const QJsonObject& cmd, const QJsonObject& data) {
    auto message = envelope("result");
    message.insert("id", cmd["id"]); message.insert("op", cmd["op"]); message.insert("sessionId", sessionId_); message.insert("data", data); send(message);
}
void Host::error(const QJsonObject& cmd, const QString& code, const QString& text) {
    auto message = envelope("error");
    if (integer(cmd["id"])) message.insert("id", cmd["id"]);
    if (cmd["op"].isString()) message.insert("op", cmd["op"]);
    message.insert("error", QJsonObject{{"code", code}, {"message", text}, {"retryable", code == "track_not_ready" || code == "internal"}}); send(message);
}
void Host::malformed(const QString& message) { error({}, "malformed_message", message); }
void Host::event(const QString& name, const QJsonObject& data) {
    auto message = envelope("event"); message.insert("event", name); message.insert("seq", static_cast<qint64>(++seq_)); message.insert("data", data); send(message);
}
QJsonObject Host::snapshot() {
    return {{"rev", static_cast<qint64>(rev_)}, {"seq", static_cast<qint64>(seq_)}, {"engineId", engineId_}, {"sessionId", sessionId_}, {"engineTimeMs", static_cast<double>(clock_.elapsed())}, {"engine", info()}, {"decks", QJsonObject{{"A", slots_[0].state}, {"B", slots_[1].state}, {"C", slots_[2].state}, {"D", slots_[3].state}}}, {"mixer", backend_->mixer()}, {"audio", backend_->audio()}, {"recording", recordingState()}, {"meters", QJsonObject{{"enabled", false}, {"intervalMs", 100}, {"simulated", false}}}};
}

void Host::sampleRecordingTimeline() {
    const auto recording = backend_->recording();
    const bool active = recording["active"].toBool();
    const qint64 frame = static_cast<qint64>(recording["frameCount"].toDouble());
    const QString key = recording["path"].toString() + QLatin1Char('|') + recording["startedAt"].toString();
    if (active && key != recordingTimelineKey_) {
        recordingTimelineKey_ = key;
        recordingTimeline_ = {};
        recordingOpenSegments_.fill(-1);
        recordingTimelineDropped_ = 0;
    }
    const auto mixer = backend_->mixer();
    const auto channels = mixer["channels"].toObject();
    const double crossfader = mixer["crossfader"].toDouble();
    for (int index = 0; index < 4; ++index) {
        const auto channel = channels[deckNames[index]].toObject();
        const double orientation = channel["orientation"].toDouble();
        const double crossGain = orientation < 0.5 ? (1.0 - crossfader) * 0.5
                : orientation > 1.5 ? (1.0 + crossfader) * 0.5 : 1.0;
        const bool contributing = active && slots_[index].state["status"] == "playing" &&
                channel["gain"].toDouble() * channel["trim"].toDouble(1.0) * crossGain > 0.0001 &&
                slots_[index].state["track"].isObject();
        int& open = recordingOpenSegments_[index];
        if (open >= 0 && (!contributing || recordingTimeline_[open].toObject()["loadGeneration"].toDouble() != static_cast<double>(slots_[index].generation))) {
            auto segment = recordingTimeline_[open].toObject();
            segment["endFrame"] = frame;
            recordingTimeline_[open] = segment;
            open = -1;
        }
        if (contributing && open < 0) {
            if (recordingTimeline_.size() >= 10000) { ++recordingTimelineDropped_; continue; }
            const auto track = slots_[index].state["track"].toObject();
            QJsonObject segment{{"eventKey", QString("%1:%2:%3:%4").arg(key, deckNames[index]).arg(slots_[index].generation).arg(recordingTimeline_.size())},
                    {"deck", deckNames[index]}, {"loadGeneration", static_cast<qint64>(slots_[index].generation)},
                    {"trackId", track["localTrackId"].isDouble() ? track["localTrackId"] : track["trackId"]},
                    {"title", track["title"]}, {"artist", track["artist"]},
                    {"startFrame", frame}, {"endFrame", QJsonValue::Null}, {"source", "engine_observed"}};
            recordingTimeline_.append(segment);
            open = recordingTimeline_.size() - 1;
        } else if (!contributing && open >= 0) {
            auto segment = recordingTimeline_[open].toObject();
            segment["endFrame"] = frame;
            recordingTimeline_[open] = segment;
            open = -1;
        }
    }
}

QJsonObject Host::recordingState() {
    sampleRecordingTimeline();
    auto state = backend_->recording();
    state["timeline"] = recordingTimeline_;
    state["timelineDroppedEvents"] = static_cast<int>(recordingTimelineDropped_);
    // The recorder frame count is exact; contribution edges are sampled.
    state["timelineQuality"] = recordingTimelineDropped_ ? "incomplete" : "engine_sampled";
    return state;
}
void Host::line(const QByteArray& bytes) {
    if (bytes.trimmed().isEmpty()) return;
    QJsonParseError parse;
    auto document = QJsonDocument::fromJson(bytes, &parse);
    if (parse.error != QJsonParseError::NoError || !document.isObject()) { malformed("Expected one JSON command object"); return; }
    auto cmd = document.object();
    const auto op = cmd["op"].toString();
    if (!integer(cmd["id"]) || !cmd["op"].isString() || (!cmd.value("kind").isUndefined() && cmd.value("kind") != "command")) { error(cmd, "malformed_message", "Invalid command envelope or unsafe integer id"); return; }
    if (!cmd.value("protocol").isUndefined() && cmd.value("protocol") != 1) { error(cmd, "protocol_version_unsupported", "Only protocol 1 is supported"); return; }
    const auto id = static_cast<quint64>(cmd["id"].toDouble());
    if (op == "session.hello") {
        if (junction_->active() && !sessionId_.isEmpty()) {
            lastId_ = id;
            auto hello = envelope("hello"); hello.insert("id",cmd["id"]); hello.insert("sessionId",sessionId_); hello.insert("engine",info()); hello.insert("protocolVersions",QJsonObject{{"min",1},{"max",1}}); send(hello); return;
        }
        for (int index = 0; index < 4; ++index) { releaseScratch(index, false); if(backend_->available()) backend_->performanceControl(index,"reverseroll",0); ++slots_[index].reverseGesture; }
        // A new renderer cannot still own a held pad from the old connection.
        const bool effectsReleased = backend_->resetFx();
        if (backend_->available()) backend_->samplerCommand("sampler.stopAll", {});
        if (effectsReleased) ++rev_;
        performanceInput_->reset();
        const auto previous = sessionId_;
        sessionId_ = QUuid::createUuid().toString(QUuid::WithoutBraces); lastId_ = id;
        auto hello = envelope("hello"); hello.insert("id", cmd["id"]); hello.insert("sessionId", sessionId_); hello.insert("engine", info()); hello.insert("protocolVersions", QJsonObject{{"min", 1}, {"max", 1}}); send(hello);
        if (!previous.isEmpty()) event("session.invalidated", {{"sessionId", previous}, {"reason", "superseded"}});
        if (effectsReleased) event("mixer.state", backend_->mixer());
        backend_->start();
        return;
    }
    if (sessionId_.isEmpty()) { error(cmd, "session_required", "Call session.hello first"); return; }
    if (cmd["engineId"] != engineId_) { error(cmd, "engine_mismatch", "Wrong engine instance"); return; }
    if (cmd["sessionId"] != sessionId_) { error(cmd, "session_mismatch", "Expired or missing session"); return; }
    if (id <= lastId_) { error(cmd, "stale_command_id", "Command id must increase"); return; }
    lastId_ = id;
    if (op.startsWith("junction.")) {
        QString failure; const auto data = junction_->command(op.mid(9), cmd["params"].toObject(), &failure);
        if (!failure.isEmpty()) error(cmd,"junction_rejected",failure); else result(cmd,data);
        return;
    }
    if (junction_->active()) {
        const auto failure = junction_->authorize(op,cmd["params"].toObject());
        if (!failure.isEmpty()) { error(cmd,"junction_rejected",failure); return; }
    }
    if (op.startsWith("waveform.")) {
        result(cmd, backend_->waveformCommand(op, cmd["params"].toObject())); return;
    }
    if (op == "performance.endpoint") { result(cmd,performanceInput_->endpoint());return; }
    if (op == "engine.audioHealth") {
        QJsonArray durations;double value;while(deckclock::callbackDurations.pop(value))durations.append(value);
        result(cmd,{{"callbackDurationsUs",durations},{"lateCallbacks",int(deckclock::lateCallbacks.load())},{"xruns",int(deckclock::xruns.load())},{"dropped",int(deckclock::callbackDurations.dropped())}});return;
    }
    if (op == "engine.clock.probe") {
        const auto received=deckclock::monotonicUs();
        result(cmd, {{"engineEpoch",engineId_},{"receivedNativeUs",received},{"sentNativeUs",deckclock::monotonicUs()}}); return;
    }
    if (op == "engine.ping") { result(cmd, {{"pong", true}}); return; }
    if (op == "state.snapshot") { result(cmd, snapshot()); return; }
    if (!samplerOps.contains(op) && !known.contains(op) && !deckControls.contains(op) && !mixerOps.contains(op) && op != "deck.scratch" && op != "deck.pitchbend" && op != "deck.timing.trace") { error(cmd, "unknown_op", "Unknown operation"); return; }
    if (op == "mixer.colorfx.set") {
        if (!backend_->available()) { error(cmd, "unsupported_operation", backend_->problem()); return; }
        const auto p = cmd["params"].toObject(); const auto deck = deckIndex(p["deck"].toString()); const auto value = p["amount"];
        if (deck < 0 || !value.isDouble() || !std::isfinite(value.toDouble()) || std::abs(value.toDouble()) > 1) { error(cmd, "invalid_params", "Expected deck and color amount -1..1"); return; }
        const auto failure = backend_->colorFx(deck,p["effect"].toString(),value.toDouble());
        if (!failure.isEmpty()) { error(cmd, "invalid_params", failure); return; }
        ++rev_; result(cmd, backend_->mixer()); event("mixer.state", backend_->mixer()); return;
    }
    if (op == "mixer.beatfx.set") {
        if (!backend_->available()) { error(cmd, "unsupported_operation", backend_->problem()); return; }
        if (!cmd["params"].isObject()) { error(cmd, "invalid_params", "Expected beat FX parameters"); return; }
        const auto failure = backend_->beatFx(cmd["params"].toObject());
        if (!failure.isEmpty()) { error(cmd, "invalid_params", failure); return; }
        ++rev_; result(cmd, backend_->mixer()); event("mixer.state", backend_->mixer()); return;
    }
    if (samplerOps.contains(op)) {
        if (!backend_->available()) { error(cmd, "unsupported_operation", backend_->problem()); return; }
        if (!cmd["params"].isObject()) { error(cmd, "invalid_params", "Expected sampler parameters"); return; }
        if (op == "sampler.pfl" && !backend_->audio()["pflApplied"].toBool()) { error(cmd, "unsupported_operation", "Choose an output with a headphone cue bus"); return; }
        const auto failure = backend_->samplerCommand(op, cmd["params"].toObject());
        if (!failure.isEmpty()) { error(cmd, "invalid_params", failure); return; }
        if (op != "sampler.state") ++rev_;
        result(cmd, backend_->samplerState()); return;
    }
    // Setup queries must work even when the selected output could not open.
    if (op == "audio.devices.list") { result(cmd, backend_->audioDevices()); return; }
    if (op == "audio.config.get") { result(cmd, backend_->audio()); return; }
    if (op == "audio.config.set") {
        if (!backend_->available()) { error(cmd, "unsupported_operation", backend_->problem()); return; }
        const auto params = cmd["params"].toObject();
        if (params.size() == 1 && params["outputRouting"].isObject()) {
            const auto route = params["outputRouting"].toObject();
            if (route.size() != 2 || !route.contains("masterChannels") || !route.contains("pflChannels")) {
                error(cmd, "invalid_params", "Expected masterChannels and pflChannels"); return;
            }
            const auto failure = backend_->configureOutputRouting(route);
            if (!failure.isEmpty()) { error(cmd, "invalid_params", failure); return; }
            ++rev_; result(cmd, backend_->audio()); event("audio.config", backend_->audio()); return;
        }
        if (params.size() != 1 || !params["microphone"].isObject()) { error(cmd, "invalid_params", "Expected {microphone:{...}}; apply output changes by restarting from Audio Settings"); return; }
        const auto mic = params["microphone"].toObject();
        const QSet<QString> fields = {"deviceId", "channel", "enabled", "gain", "duckingEnabled", "duckingStrength"};
        for (auto it = mic.begin(); it != mic.end(); ++it) {
            if (!fields.contains(it.key())) { error(cmd, "invalid_params", "Unknown microphone setting: " + it.key()); return; }
            const auto value = it.value();
            bool valid = false;
            if (it.key() == "deviceId") valid = value.isNull() || (value.isString() && !value.toString().isEmpty() && value.toString().size() <= 512);
            else if (it.key() == "enabled" || it.key() == "duckingEnabled") valid = value.isBool();
            else if (it.key() == "channel") valid = integer(value) && value.toDouble() <= 255;
            else valid = value.isDouble() && std::isfinite(value.toDouble()) && value.toDouble() >= 0 && value.toDouble() <= (it.key() == "gain" ? 4.0 : 1.0);
            if (!valid) { error(cmd, "invalid_params", "Invalid microphone setting: " + it.key()); return; }
        }
        const auto failure = backend_->configureMicrophone(mic);
        if (!failure.isEmpty()) { error(cmd, "invalid_params", failure); return; }
        ++rev_; const auto state = backend_->audio(); result(cmd, state); event("audio.config", state); return;
    }
    if (op == "recording.directory.set") {
        if (!backend_->available()) { error(cmd, "unsupported_operation", backend_->problem()); return; }
        if (!cmd["params"].isObject()) { error(cmd, "invalid_params", "params must be an object"); return; }
        const auto directory = cmd["params"].toObject()["directory"].toString();
        if (directory.isEmpty() || directory.size() > 1024 || !directory.startsWith('/')) { error(cmd, "invalid_params", "directory must be an absolute path"); return; }
        const auto resolved = backend_->recordingDirectory(directory);
        if (resolved.isEmpty()) { error(cmd, "unsupported_operation", "Recording is unavailable on this host"); return; }
        ++rev_; const auto state = recordingState(); result(cmd, state); event("recording.state", state); return;
    }
    if (op == "recording.format.set") {
        if (!backend_->available()) { error(cmd, "unsupported_operation", backend_->problem()); return; }
        if (!cmd["params"].isObject()) { error(cmd, "invalid_params", "params must be an object"); return; }
        const auto format = cmd["params"].toObject()["format"].toString();
        if (backend_->recordingFormat(format).isEmpty()) { error(cmd, "invalid_params", "This host cannot write that recording format"); return; }
        ++rev_; const auto state = recordingState(); result(cmd, state); event("recording.state", state); return;
    }
    const bool recordingOp = op == "recording.start" || op == "recording.stop";
    if (!transport.contains(op) && !deckControls.contains(op) && !mixerOps.contains(op) && !recordingOp && op != "deck.timing.trace") { error(cmd, "unsupported_operation", "This host does not implement the requested controller operation"); return; }
    if (!cmd["params"].isObject()) { error(cmd, "invalid_params", "params must be an object"); return; }
    auto params = cmd["params"].toObject();
    if (!backend_->available()) { error(cmd, "unsupported_operation", backend_->problem()); return; }
    if (recordingOp) {
        if (op == "recording.start") backend_->startRecording(); else backend_->stopRecording();
        ++rev_; const auto state = recordingState(); result(cmd, state); event("recording.state", state); return;
    }
    if (mixerOps.contains(op)) {
        if (op == "mixer.channel.orientation") {
            const int channel = deckIndex(params["deck"].toString().toUpper());
            if (channel < 0 || !integer(params["orientation"]) || params["orientation"].toDouble() > 2) { error(cmd, "invalid_params", "Expected deck and orientation 0/1/2"); return; }
            backend_->orientation(channel, params["orientation"].toInt());
            ++rev_; result(cmd, backend_->mixer()); event("mixer.state", backend_->mixer()); return;
        }
        if (op == "mixer.channel.pfl") {
            const int channel = deckIndex(params["deck"].toString().toUpper());
            if (channel < 0 || !params["enabled"].isBool()) { error(cmd, "invalid_params", "Expected deck and boolean enabled"); return; }
            if (!backend_->audio()["pflApplied"].toBool()) { error(cmd, "unsupported_operation", "Choose DDJ-1000 output to use its headphone cue bus"); return; }
            backend_->pfl(channel, params["enabled"].toBool());
            ++rev_; result(cmd, backend_->mixer()); event("mixer.state", backend_->mixer()); return;
        }
        if (op == "mixer.fx.set") {
            const int channel = deckIndex(params["deck"].toString().toUpper());
            if (channel < 0) { error(cmd, "deck_not_found", "Expected deck A, B, C or D"); return; }
            if (params.contains("trackId") && params["trackId"] != slots_[channel].state["track"].toObject()["trackId"]) { error(cmd, "invalid_params", "The loaded track changed"); return; }
            const auto effect = params["effect"].toString();
            const auto mix = params["mix"];
            // 掛かりの強さ。省略時は従来どおりの中央値。
            const auto depthValue = params.contains("depth") ? params["depth"] : QJsonValue(0.5);
            if (!depthValue.isDouble() || !std::isfinite(depthValue.toDouble()) || depthValue.toDouble() < 0 || depthValue.toDouble() > 1) { error(cmd, "invalid_params", "depth must be 0..1"); return; }
            if (!padEffects.contains(effect) || !params["enabled"].isBool() || !mix.isDouble() || !std::isfinite(mix.toDouble()) || mix.toDouble() < 0 || mix.toDouble() > 1) { error(cmd, "invalid_params", "Expected a supported pad effect, boolean enabled, mix 0..1"); return; }
            if (!backend_->fx(channel, effect, params["enabled"].toBool(), mix.toDouble(), depthValue.toDouble())) { error(cmd, "unsupported_operation", "Native effect processor is unavailable"); return; }
            ++rev_; const auto state = backend_->mixer(); result(cmd, state); event("mixer.state", state); return;
        }
        if (op == "mixer.channel.eq" || op == "mixer.eq.set" || op == "mixer.filter.set" || op == "mixer.trim.set") {
            const int channel = deckIndex(params["deck"].toString().toUpper());
            if (channel < 0) { error(cmd, "deck_not_found", "Expected deck A, B, C or D"); return; }
            const bool isFilter = op == "mixer.filter.set";
            const auto value = params.value(isFilter ? "value" : "gain");
            const auto band = params.value("band").toString();
            if (!value.isDouble() || !std::isfinite(value.toDouble()) || value.toDouble() < (isFilter ? -1.0 : 0.0) || value.toDouble() > (isFilter ? 1.0 : op == "mixer.trim.set" ? 2.0 : 4.0) ||
                    ((op == "mixer.channel.eq" || op == "mixer.eq.set") && band != "low" && band != "mid" && band != "high")) { error(cmd, "invalid_params", "Expected filter -1..1, trim gain 0..2, EQ gain 0..4 and band low/mid/high"); return; }
            if (isFilter) backend_->filter(channel, value.toDouble());
            else if (op == "mixer.trim.set") backend_->trim(channel, value.toDouble());
            else backend_->eq(channel, band, value.toDouble());
            ++rev_; const auto state = backend_->mixer(); result(cmd, state); event("mixer.state", state); return;
        }
        const auto value = params[op == "mixer.crossfader" ? "position" : "gain"];
        if (!value.isDouble() || value.toDouble() < (op == "mixer.crossfader" ? -1.0 : 0.0) || value.toDouble() > 1.0) { error(cmd, "invalid_params", "Mixer value outside range"); return; }
        if (op == "mixer.channel.gain") {
            const auto name = params["deck"].toString().toUpper();
            const int index = deckIndex(name);
            if (index < 0) { error(cmd, "deck_not_found", "Expected deck A, B, C or D"); return; }
            backend_->gain(index, value.toDouble());
        } else if (op == "mixer.master.gain") backend_->masterGain(value.toDouble());
        else backend_->crossfader(value.toDouble());
        ++rev_; result(cmd, backend_->mixer()); event("mixer.state", backend_->mixer()); return;
    }
    const auto name = params["deck"].toString().toUpper();
    const int index = deckIndex(name);
    if (index < 0) { error(cmd, "deck_not_found", "Expected deck A, B, C or D"); return; }
    auto& slot = slots_[index]; auto& deck_ = slot.state; auto& descriptor_ = slot.descriptor;
    if (op == "deck.load") {
        const auto descriptor = params["track"].toObject();
        if (!descriptor["path"].isString() || !QDir::isAbsolutePath(descriptor["path"].toString()) || descriptor["trackId"].toString().trimmed().isEmpty()) { error(cmd, "invalid_params", "track requires trackId and an absolute local path"); return; }
        if (descriptor.contains("musicalKey") && (!descriptor["musicalKey"].isString() || descriptor["musicalKey"].toString().size() > 32)) { error(cmd, "invalid_params", "Expected short musical key string"); return; }
        if (descriptor.contains("hotCues") && !validCues(descriptor["hotCues"], std::numeric_limits<double>::max())) { error(cmd, "invalid_params", "hotCues must contain 8 or 16 null or finite non-negative positions"); return; }
        const bool hasBeatTimes = descriptor.contains("beatTimesMs");
        const auto meter = descriptor.contains("beatsPerBar") ? descriptor["beatsPerBar"] : QJsonValue(4);
        if ((descriptor.contains("beatsPerBar") && !validMeter(meter)) ||
                (descriptor.contains("beatNumbers") && (!hasBeatTimes || !descriptor["beatTimesMs"].isArray() || !validBeatNumbers(descriptor["beatNumbers"], descriptor["beatTimesMs"].toArray().size(), static_cast<int>(meter.toDouble())))) ||
                (hasBeatTimes && (!parseBeatTimes(descriptor["beatTimesMs"], std::numeric_limits<double>::max()) || !validOptionalBpm(descriptor["bpm"]) || !validOptionalOffset(descriptor["beatgridOffsetMs"], std::numeric_limits<double>::max()))) ||
                (descriptor.contains("beatgridOffsetMs") && !hasBeatTimes && !validGrid(descriptor["bpm"], descriptor["beatgridOffsetMs"], meter, std::numeric_limits<double>::max()))) {
            error(cmd, "invalid_params", "Invalid beat grid: beatTimesMs must contain 2..100000 finite, strictly increasing non-negative timestamps; beatNumbers must match it and contain integers 1..beatsPerBar"); return;
        }
        if (deck_["status"] == "loading") { error(cmd, "track_not_ready", "Wait for the current load to finish or unload first"); return; }
        beginLoad(cmd, index, descriptor); return;
    }
    if (op == "deck.unload") {
        slot.generation = ++generation_; backend_->unload(index); resetDeck(index); descriptor_ = {}; ++rev_; result(cmd, {{"deck", name}}); event("deck.state", deck_); event("mixer.state", backend_->mixer()); return;
    }
    if (deck_["status"] == "loading") { error(cmd, "track_not_ready", "Deck is still loading"); return; }
    if (op == "deck.timing.trace") { result(cmd, backend_->timingTrace(index)); return; }
    if (!deck_["track"].isObject()) { error(cmd, "no_track_loaded", "No track loaded"); return; }
    if (op == "deck.pitchbend") {
        const auto amount = params["amount"];
        if (!amount.isDouble() || !std::isfinite(amount.toDouble()) || std::abs(amount.toDouble()) > 0.75 ||
                (params.contains("trackId") && params["trackId"] != deck_["track"].toObject()["trackId"])) {
            error(cmd, "invalid_params", "Expected pitchbend -0.75..0.75 for the loaded track"); return;
        }
        backend_->pitchbend(index, amount.toDouble());
        result(cmd, {{"accepted", true}, {"deck", name}}); return;
    }
    if (op.startsWith("deck.key.") || op == "deck.slip.set" || op == "deck.reverse.set" || op == "deck.slipReverse.set") {
        if (params.contains("trackId") && params["trackId"] != deck_["track"].toObject()["trackId"]) { error(cmd, "invalid_params", "The loaded track changed"); return; }
        if (op == "deck.key.shift") {
            const auto value = params["semitones"];
            if (!value.isDouble() || !std::isfinite(value.toDouble()) || value.toDouble() < -12 || value.toDouble() > 12) { error(cmd, "invalid_params", "Expected semitones -12..12"); return; }
            backend_->keylock(index, true); deck_["keylock"] = true;
            backend_->performanceControl(index, "pitch_adjust", value.toDouble());
        } else if (op == "deck.key.reset" || op == "deck.key.sync") {
            const QString control = op == "deck.key.reset" ? "reset_key" : "sync_key";
            backend_->performanceControl(index, control, 1); backend_->performanceControl(index, control, 0);
        } else {
            if (!params["enabled"].isBool()) { error(cmd, "invalid_params", "Expected enabled boolean"); return; }
            backend_->performanceControl(index, op == "deck.slip.set" ? "slip_enabled" : op == "deck.reverse.set" ? "reverse" : "reverseroll", params["enabled"].toBool() ? 1 : 0);
            if (op == "deck.slipReverse.set") {
                const auto gesture = ++slot.reverseGesture, generation = slot.generation;
                if (params["enabled"].toBool()) {
                    const double bpm = backend_->effectiveBpm(index);
                    const int duration = static_cast<int>(480000.0 / (bpm > 0 ? bpm : 120));
                    QTimer::singleShot(duration, this, [this,index,gesture,generation] { if(slots_[index].generation==generation && slots_[index].reverseGesture==gesture) backend_->performanceControl(index,"reverseroll",0); });
                }
            }
        }
        ++rev_; result(cmd, {{"accepted", true}}); return;
    }
    if (op.startsWith("deck.hotcue.") || op.startsWith("deck.loop.") || op == "deck.beatjump" || op == "deck.quantize.set") {
        if (params.contains("trackId") && params["trackId"] != deck_["track"].toObject()["trackId"]) { error(cmd, "invalid_params", "The loaded track changed"); return; }
        const double duration = deck_["track"].toObject()["durationMs"].toDouble();
        if (op.startsWith("deck.hotcue.")) {
            if (params.contains("quantize") && !params["quantize"].isBool()) { error(cmd, "invalid_params", "quantize must be boolean"); return; }
            const auto cue = params["index"];
            if (!integer(cue) || cue.toDouble() > 15) { error(cmd, "invalid_params", "Hotcue index must be integer 0..15"); return; }
            std::optional<double> position;
            if (op == "deck.hotcue.set" && params.contains("positionMs")) {
                const auto value = params["positionMs"];
                if (!value.isDouble() || !std::isfinite(value.toDouble()) || value.toDouble() < 0 || value.toDouble() >= duration) { error(cmd, "invalid_params", "Hotcue position must be inside the track"); return; }
                position = value.toDouble();
            }
            if (op == "deck.hotcue.jump" && (cue.toInt() >= backend_->performanceState(index)["hotCues"].toArray().size() || backend_->performanceState(index)["hotCues"].toArray()[cue.toInt()].isNull())) { error(cmd, "invalid_params", "Hotcue is not set"); return; }
            const bool quantize = backend_->performanceState(index)["quantize"].toBool();
            if (op == "deck.hotcue.set" && !position) {
                const bool wanted = params.contains("quantize") ? params["quantize"].toBool() : quantize;
                position = wanted ? backend_->quantizedPositionMs(index) : backend_->positionMs(index);
                backend_->hotcue(index, cue.toInt(), "set", qBound(0.0, *position, duration));
            } else backend_->hotcue(index, cue.toInt(), op.section('.', -1), position);
        } else if (op == "deck.loop.set") {
            const auto start = params["startMs"], end = params["endMs"];
            if (!start.isDouble() || !end.isDouble() || !std::isfinite(start.toDouble()) || !std::isfinite(end.toDouble()) || start.toDouble() < 0 || end.toDouble() <= start.toDouble() || end.toDouble() > duration) { error(cmd, "invalid_params", "Loop must have 0 <= startMs < endMs <= duration"); return; }
            backend_->loop(index, start.toDouble(), end.toDouble());
        } else if (op == "deck.loop.enable" || op == "deck.quantize.set") {
            if (!params["enabled"].isBool()) { error(cmd, "invalid_params", "enabled must be boolean"); return; }
            if (op == "deck.quantize.set") backend_->quantize(index, params["enabled"].toBool());
            else {
                if (params["enabled"].toBool() && !backend_->performanceState(index)["loopRegion"].isObject()) { error(cmd, "invalid_params", "Set a loop region first"); return; }
                backend_->loopEnable(index, params["enabled"].toBool());
            }
        } else {
            const auto beats = params["beats"];
            if (!beats.isDouble() || !std::isfinite(beats.toDouble()) || std::abs(beats.toDouble()) > 64 || (op == "deck.beatjump" ? beats.toDouble() == 0 : beats.toDouble() < 0.125)) { error(cmd, "invalid_params", "Beatjump needs nonzero signed beats up to 64; loop needs 0.125..64 beats"); return; }
            if (backend_->beatgridState(index).isEmpty()) { error(cmd, "invalid_params", "Apply a beat grid first"); return; }
            if (op == "deck.beatjump") backend_->beatjump(index, beats.toDouble());
            else backend_->beatloop(index, beats.toDouble());
        }
        const auto state = backend_->performanceState(index);
        for (auto it = state.begin(); it != state.end(); ++it) deck_[it.key()] = it.value();
        descriptor_["hotCues"] = deck_["hotCues"];
        ++rev_; result(cmd, deck_); event("deck.state", deck_); return;
    }
    if (op == "deck.scratch") {
        const auto phase = params["phase"].toString();
        const auto gesture = params["gestureId"].toString();
        const auto position = params["positionMs"];
        if ((phase != "begin" && phase != "move" && phase != "end") ||
                gesture.trimmed().isEmpty() || gesture.size() > 128 ||
                !position.isDouble() || !std::isfinite(position.toDouble()) ||
                std::abs(position.toDouble()) > 60000 || (phase == "begin" && position.toDouble() != 0)) {
            error(cmd, "invalid_params", "scratch requires phase begin/move/end, gestureId 1..128 characters, and finite relative positionMs within +/-60000 (begin must be zero)"); return;
        }
        const double captured=params.value("capturedNativeUs").toDouble();
        const bool keepalive=params.value("keepalive").toBool();
        if(params.contains("capturedNativeUs") && (!std::isfinite(captured) || captured <= 0 || captured > deckclock::monotonicUs()+10000 || deckclock::monotonicUs()-captured>500000)) {
            error(cmd,"invalid_params","Scratch capture clock is stale or invalid");return;
        }
        // Input may be serviced before a delayed Qt timer. Expired moves must
        // not revive a gesture already released by the audio-clock watchdog.
        if (!slot.scratchGesture.isEmpty() && clock_.elapsed() - slot.scratchLastInputMs >= 1500) releaseScratch(index);
        if (phase == "begin") {
            if (slot.scratchGesture != gesture) {
                // The audio mailbox generation replaces the previous gesture.
                slot.scratchGesture = gesture;
                slot.scratchLastPositionMs = 0;
                backend_->scratch(index, phase, 0, captured);
            }
            slot.scratchLastInputMs = clock_.elapsed();
        } else if (slot.scratchGesture == gesture) {
            slot.scratchLastInputMs = clock_.elapsed();
            slot.scratchLastPositionMs = position.toDouble();
            if (phase == "end") releaseScratch(index);
            else backend_->scratch(index, phase, position.toDouble(), captured, keepalive);
        } else {
            // A delayed move/end must never grab or release a newer gesture.
            result(cmd, {{"deck", name}, {"accepted", false}, {"scratching", backend_->scratching(index)}}); return;
        }
        result(cmd, {{"deck", name}, {"accepted", true}, {"scratching", backend_->scratching(index)}}); return;
    }
    if (op == "deck.beatgrid.set") {
        auto track = deck_["track"].toObject();
        if (!params["trackId"].isString() || params["trackId"].toString().isEmpty()) { error(cmd, "invalid_params", "trackId is required to guard the loaded track"); return; }
        if (params["trackId"] != track["trackId"]) { error(cmd, "invalid_params", "The deck track changed; reload its grid before applying"); return; }
        const auto meter = params.contains("beatsPerBar") ? params["beatsPerBar"] : QJsonValue(4);
        const bool hasBeatTimes = params.contains("beatTimesMs");
        const double duration = track["durationMs"].toDouble();
        std::optional<QVector<double>> beatTimes;
        QJsonValue times = params.value("beatTimesMs"), numbers = params.value("beatNumbers");
        if (hasBeatTimes) {
            QVector<double> values;
            // The shape is checked without a bound first so that trimming only
            // ever removes a decoder-trimmed tail, never hides malformed input.
            if (!validMeter(meter) || !parseBeatTimes(times, std::numeric_limits<double>::max()) ||
                    (params.contains("beatNumbers") && !validBeatNumbers(numbers, times.toArray().size(), static_cast<int>(meter.toDouble()))) ||
                    !trimBeatsToDuration(times, numbers, duration) ||
                    !parseBeatTimes(times, duration, &values) || !validMeter(meter) ||
                    !validOptionalBpm(params.value("bpm")) || !validOptionalOffset(params.value("firstBeatMs"), duration) ||
                    (params.contains("beatNumbers") && !validBeatNumbers(numbers, values.size(), static_cast<int>(meter.toDouble())))) {
                error(cmd, "invalid_params", "beatTimesMs must contain 2..100000 finite, strictly increasing timestamps, at least two of them inside the track; beatNumbers must match it and contain integers 1..beatsPerBar"); return;
            }
            beatTimes = std::move(values);
        } else if (params.contains("beatNumbers") || !validGrid(params["bpm"], params["firstBeatMs"], meter, duration)) {
            error(cmd, "invalid_params", "Expected BPM 20..300, firstBeatMs >= 0 and before track duration, integer beatsPerBar 1..16"); return;
        }
        if (!backend_->beatgrid(index, params["bpm"].toDouble(), params["firstBeatMs"].toDouble(), beatTimes)) { error(cmd, "internal", "Mixxx could not replace the loaded beat grid (the track may be BPM-locked)"); return; }
        track["beatsPerBar"] = meter; track["beatgridApplied"] = true;
        descriptor_["beatsPerBar"] = meter;
        if (hasBeatTimes) {
            track["beatTimesMs"] = times; descriptor_["beatTimesMs"] = times;
            if (params.contains("beatNumbers")) { track["beatNumbers"] = numbers; descriptor_["beatNumbers"] = numbers; }
            else { track.remove("beatNumbers"); descriptor_.remove("beatNumbers"); }
            if (params.contains("bpm") && !params["bpm"].isNull()) { track["bpm"] = params["bpm"]; descriptor_["bpm"] = params["bpm"]; }
            else { track["bpm"] = QJsonValue::Null; descriptor_.remove("bpm"); }
            if (params.contains("firstBeatMs")) { track["beatgridOffsetMs"] = params["firstBeatMs"]; descriptor_["beatgridOffsetMs"] = params["firstBeatMs"]; }
            else { track.remove("beatgridOffsetMs"); descriptor_.remove("beatgridOffsetMs"); }
        } else {
            track.remove("beatTimesMs"); track.remove("beatNumbers"); descriptor_.remove("beatTimesMs"); descriptor_.remove("beatNumbers");
            track["bpm"] = params["bpm"]; track["beatgridOffsetMs"] = params["firstBeatMs"];
            descriptor_["bpm"] = params["bpm"]; descriptor_["beatgridOffsetMs"] = params["firstBeatMs"];
        }
        track["nativeBeatgrid"] = backend_->beatgridState(index);
        deck_["track"] = track;
        deck_["effectiveBpm"] = effectiveBpmValue(backend_->effectiveBpm(index));
        ++rev_; result(cmd, deck_); event("deck.state", deck_); return;
    }
    if (op == "deck.tempo.set") {
        const auto value = params["rate"];
        if (!value.isDouble() || value.toDouble() < 0.25 || value.toDouble() > 4.0) { error(cmd, "invalid_params", "rate is outside 0.25..4.0"); return; }
        backend_->tempo(index, value.toDouble()); deck_["rate"] = value; ++rev_; result(cmd, deck_); event("deck.state", deck_); return;
    }
    if (op == "deck.keylock.set") {
        if (!params["enabled"].isBool()) { error(cmd, "invalid_params", "enabled must be boolean"); return; }
        backend_->keylock(index, params["enabled"].toBool()); deck_["keylock"] = params["enabled"]; ++rev_; result(cmd, deck_); event("deck.state", deck_); return;
    }
    if (op == "deck.sync.set") {
        if (!params["enabled"].isBool()) { error(cmd, "invalid_params", "enabled must be boolean"); return; }
        const bool syncEnabled = params["enabled"].toBool();
        const auto leader = params["leader"];
        int leaderIndex = -1;
        if (leader.isString()) {
            leaderIndex = deckIndex(leader.toString().toUpper());
            if (leaderIndex < 0) { error(cmd, "invalid_params", "leader must be deck A, B, C or D"); return; }
        }
        backend_->sync(index, syncEnabled);
        if (syncEnabled && leaderIndex >= 0) backend_->setSyncLeader(leaderIndex);
        deck_["syncEnabled"] = syncEnabled;
        publishSyncLeader(index);
        ++rev_; result(cmd, deck_); event("deck.state", deck_); return;
    }
    if (op == "deck.seek") {
        const auto value = params["positionMs"];
        if (!value.isDouble() || value.toDouble() < -60000 || value.toDouble() > deck_["track"].toObject()["durationMs"].toDouble()) { error(cmd, "invalid_params", "positionMs is outside the transport range"); return; }
        backend_->seek(index, value.toDouble());
    } else {
        slot.transportTouched = true;
        backend_->play(index, op == "deck.play");
    }
    // Command acceptance is not an invented applied state. The poll below emits
    // the audio engine's observed position / play state on the next Qt tick.
    result(cmd, {{"deck", name}, {"accepted", true}});
}
void Host::beginLoad(const QJsonObject& cmd, int index, const QJsonObject& descriptor) {
    auto& slot = slots_[index];
    slot.descriptor = descriptor; slot.generation = ++generation_; ++rev_; resetDeck(index); slot.state["status"] = "loading"; slot.state["loadId"] = static_cast<qint64>(slot.generation);
    result(cmd, {{"accepted", true}, {"deck", deckNames[index]}, {"loadId", static_cast<qint64>(slot.generation)}}); event("deck.state", slot.state);
    backend_->load(index, descriptor["path"].toString(), slot.generation); event("mixer.state", backend_->mixer());
}
void Host::completed(int index, quint64 generation, QJsonObject metadata, QString failure) {
    auto& slot = slots_[index]; auto& deck_ = slot.state; auto& descriptor_ = slot.descriptor;
    if (generation != slot.generation || deck_["status"] != "loading") return;
    ++rev_; deck_["loadId"] = QJsonValue::Null; deck_["loadGeneration"] = double(generation);
    if (!failure.isEmpty()) {
        deck_["status"] = "error"; deck_["lastError"] = failure;
        event("deck.load.failed", {{"deck", deckNames[index]}, {"loadId", static_cast<qint64>(generation)}, {"trackId", descriptor_["trackId"]}, {"error", QJsonObject{{"code", "load_failed"}, {"message", failure}}}});
    } else {
        auto track = descriptor_;
        for (auto it = metadata.begin(); it != metadata.end(); ++it) track.insert(it.key(), it.value());
        for (const auto* key : {"title", "artist", "bpm"}) if (!track.contains(key)) track.insert(key, QJsonValue::Null);
        if (!track.contains("beatsPerBar")) track.insert("beatsPerBar", 4);
        const double duration = track["durationMs"].toDouble();
        // Only the deck copy is emptied. The library keeps every stored cue, so
        // a later load of a correctly decoded file restores them all.
        if (track.contains("hotCues")) track["hotCues"] = cuesWithinDuration(track["hotCues"], duration);
        const bool hasBeatTimes = track.contains("beatTimesMs");
        if (hasBeatTimes) {
            QJsonValue times = track["beatTimesMs"], numbers = track.value("beatNumbers");
            const bool usable = trimBeatsToDuration(times, numbers, duration) && parseBeatTimes(times, duration) &&
                    (!track.contains("beatNumbers") || validBeatNumbers(numbers, times.toArray().size(), static_cast<int>(track["beatsPerBar"].toDouble())));
            if (!usable) {
                deck_["status"] = "error"; deck_["lastError"] = "Fewer than two saved beatTimesMs fall inside the decoded track duration";
                backend_->unload(index);
                event("deck.load.failed", {{"deck", deckNames[index]}, {"loadId", static_cast<qint64>(generation)}, {"trackId", descriptor_["trackId"]}, {"error", QJsonObject{{"code", "invalid_beatgrid"}, {"message", deck_["lastError"]}}}});
                event("deck.state", deck_);
                return;
            }
            track["beatTimesMs"] = times;
            if (track.contains("beatNumbers")) track["beatNumbers"] = numbers;
        }
        const auto beatTimes = descriptorBeatTimes(track, duration);
        const bool hasConstantGrid = !hasBeatTimes && validGrid(track.value("bpm"), track.value("beatgridOffsetMs"), track.value("beatsPerBar"), duration);
        // Saved grid metadata must reach Mixxx on load, not only the waveform.
        track["beatgridApplied"] = (beatTimes || hasConstantGrid) && backend_->beatgrid(index, track.value("bpm").toDouble(), track.value("beatgridOffsetMs").toDouble(), beatTimes);
        if (hasBeatTimes && !track["beatgridApplied"].toBool()) {
            deck_["status"] = "error"; deck_["lastError"] = "Mixxx could not apply the saved variable beat grid";
            backend_->unload(index);
            event("deck.load.failed", {{"deck", deckNames[index]}, {"loadId", static_cast<qint64>(generation)}, {"trackId", descriptor_["trackId"]}, {"error", QJsonObject{{"code", "beatgrid_apply_failed"}, {"message", deck_["lastError"]}}}});
            event("deck.state", deck_);
            return;
        }
        if (track["beatgridApplied"].toBool()) track["nativeBeatgrid"] = backend_->beatgridState(index);
        if (track.contains("musicalKey")) backend_->trackKey(index, track["musicalKey"].toString());
        if (track.contains("hotCues")) {
            const auto cues = track["hotCues"].toArray();
            for (int cue = 0; cue < cues.size(); ++cue) if (!cues[cue].isNull()) backend_->hotcue(index, cue, "set", cues[cue].toDouble());
        }
        const auto performance = backend_->performanceState(index);
        for (auto it = performance.begin(); it != performance.end(); ++it) deck_[it.key()] = it.value();
        deck_["track"] = track; deck_["status"] = "ready";
        deck_["effectiveBpm"] = effectiveBpmValue(backend_->effectiveBpm(index));
        event("deck.loaded", {{"deck", deckNames[index]}, {"loadId", static_cast<qint64>(generation)}, {"track", track}});
    }
    event("deck.state", deck_);
}
void Host::sample() {
    if (!backend_->available()) return; // session.hello starts the engine asynchronously.
    QJsonObject positions; QJsonArray clockPoints; unsigned dropped = 0;
    for (int index = 0; index < 4; ++index) {
        if (!slots_[index].scratchGesture.isEmpty() && clock_.elapsed() - slots_[index].scratchLastInputMs >= 1500) releaseScratch(index);
        const auto batch = backend_->clockPoints(index);
        dropped += batch["dropped"].toInt();
        for (const auto& value : batch["points"].toArray()) {
            auto point = value.toObject();
            if (point["loadGeneration"].toDouble() != double(slots_[index].generation) || !slots_[index].state["track"].isObject()) continue;
            point["deck"] = deckNames[index]; point["engineEpoch"] = engineId_;
            clockPoints.append(point);
        }
        const auto position = sampleDeck(index);
        if (!position.isEmpty()) positions.insert(deckNames[index], position);
    }
    if (!clockPoints.isEmpty()) event("deck.clock.v2", {{"points", clockPoints}, {"dropped", int(dropped)}});
    if (!positions.isEmpty()) event("deck.position", {{"decks", positions}});
    sampleRecordingTimeline();
}
QJsonObject Host::sampleDeck(int index) {
    auto& deck_ = slots_[index].state;
    if (!deck_["track"].isObject()) return {};
    const auto track = deck_["track"].toObject();
    // Negative time is real audio-clock transport (silence before the file),
    // not a separate UI countdown. Never hide it at the protocol boundary.
    const auto position = qMin(backend_->positionMs(index), track["durationMs"].toDouble());
    const auto previousStatus = deck_["status"].toString();
    const QString status = backend_->playing(index) ? "playing" : (previousStatus == "ready" && !slots_[index].transportTouched ? "ready" : "paused");
    const auto effectiveBpm = effectiveBpmValue(backend_->effectiveBpm(index));
    const auto rate = backend_->playbackRate(index);
    const bool bpmChanged = effectiveBpm.isDouble() != deck_["effectiveBpm"].isDouble() ||
            (effectiveBpm.isDouble() && std::abs(effectiveBpm.toDouble() - deck_["effectiveBpm"].toDouble()) > 0.0001);
    const bool tempoChanged = bpmChanged || (std::isfinite(rate) && rate > 0 && std::abs(rate - deck_["rate"].toDouble()) > 0.000001);
    const bool scratching = backend_->scratching(index);
    const bool scratchChanged = scratching != deck_["scratching"].toBool();
    const auto performance = backend_->performanceState(index);
    bool performanceChanged = false;
    for (auto it = performance.begin(); it != performance.end(); ++it) {
        if (deck_[it.key()] != it.value()) { performanceChanged = true; deck_[it.key()] = it.value(); }
    }
    if (position == deck_["positionMs"].toDouble() && status == previousStatus && !tempoChanged && !scratchChanged && !performanceChanged) return {};
    deck_["positionMs"] = position; deck_["positionFrames"] = std::floor(position * track["sampleRateHz"].toDouble() / 1000.0); deck_["status"] = status;
    deck_["effectiveBpm"] = effectiveBpm; if (std::isfinite(rate) && rate > 0) deck_["rate"] = rate;
    deck_["scratching"] = scratching;
    ++rev_;
    // Beat grids can contain 100,000 markers. Never resend that full descriptor
    // for per-buffer tempo/scratch changes; the lightweight event carries them.
    if (status != previousStatus || performanceChanged) event("deck.state", deck_);
    return {{"positionMs", position}, {"positionFrames", deck_["positionFrames"]}, {"rate", deck_["rate"]}, {"effectiveBpm", effectiveBpm}, {"status", status}, {"scratching", scratching}};
}
