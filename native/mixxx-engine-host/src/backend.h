#pragma once
#include <QJsonArray>
#include <QJsonObject>
#include <QString>
#include <QVector>
#include <functional>
#include <memory>
#include <optional>

namespace junction { class Runtime; }

// Main-thread boundary. Implementations must marshal controls through Mixxx's
// control machinery; JSON and these callbacks never run in an audio callback.
class PlaybackBackend {
public:
    virtual ~PlaybackBackend() = default;
    virtual void attachJunction(junction::Runtime*) {}
    virtual QString privatePreviewCommand(const QString&,const QJsonObject&) { return "Private preview unavailable"; }
    virtual QJsonObject privatePreviewState() const { return {}; }
    /// The JUNCTION deck: another DJ's audio as a local mixer channel.
    virtual QJsonObject junctionInputState() const { return {{"available", false}}; }
    virtual QString junctionInputSet(const QJsonObject&) { return "JUNCTION MASTERはこの音声エンジンでは使えません"; }
    /// Taking over from the deck's DJ: unity level, THRU, flat EQ.
    virtual void junctionInputTakeOver() {}
    /// Route the JUNCTION input to main only while the local operator is
    /// actively mixing it out. PFL remains independent.
    virtual void junctionInputMainMix(bool) {}
    /// The engine renders a LOCAL NEXT bus (local channels only, never the
    /// JUNCTION input or a microphone) for the return feed. Without it a local
    /// return must be refused whenever JUNCTION MASTER is in main.
    virtual bool junctionLocalReturnBus() const { return false; }
    /// Latest per-callback peak of the LOCAL NEXT bus: this DJ's own sound.
    virtual float junctionLocalPeak() const { return 0; }
    /// OUTGOING tail: restrict the LOCAL NEXT bus to `decks` at the current
    /// main gain, and fix their tempo (SYNC off). Empty restores the full bus.
    virtual void junctionTail(const QList<int>&) {}
    virtual bool available() const = 0;
    virtual QString implementation() const { return "unavailable"; }
    virtual void start() {}
    virtual QString colorFx(int, const QString&, double) { return "Color FX unavailable"; }
    virtual QString beatFx(const QJsonObject&) { return "Beat FX unavailable"; }
    virtual QJsonObject samplerState() const { return {}; }
    virtual QString samplerCommand(const QString&, const QJsonObject&) { return "Sampler unavailable"; }
    virtual QString problem() const = 0;
    virtual QJsonObject audio() const = 0;
    virtual QJsonObject audioDevices() const {
        return {{"devices", QJsonArray{}}, {"reason", problem()}};
    }
    // Empty string means success. Recover the previous routing on failure and
    // expose an unavailable audio state if recovery itself fails.
    virtual QString configureOutputRouting(const QJsonObject&) { return "Output routing is unavailable on this host"; }
    virtual QString configureMicrophone(const QJsonObject&) { return "Microphone input is unavailable on this host"; }
    virtual QJsonObject mixer() const {
        const auto channel = [](const QString& deck) { return QJsonObject{{"deck", deck}, {"gain", 0.0}, {"eqLow", 1.0}, {"eqMid", 1.0}, {"eqHigh", 1.0}, {"pfl", false}, {"available", false}}; };
        return {{"available", false}, {"crossfader", 0.0}, {"masterGain", 0.0}, {"headphoneGain", 0.0}, {"headphoneMix", 0.0}, {"channels", QJsonObject{{"A", channel("A")}, {"B", channel("B")}, {"C", channel("C")}, {"D", channel("D")}}}};
    }
    virtual void load(int deck, const QString& path, quint64 generation) = 0;
    virtual void unload(int deck) = 0;
    virtual void play(int deck, bool enabled) = 0;
    virtual void seek(int deck, double positionMs) = 0;
    virtual void scratch(int, const QString&, double, double = 0, bool = false, quint64 = 0) {}
    virtual bool scratching(int) const { return false; }
    virtual QJsonObject waveformCommand(const QString&, const QJsonObject&) { return {{"error","UNAVAILABLE"}}; }
    virtual QJsonObject clockPoints(int) { return {}; }
    virtual QJsonObject timingTrace(int) { return {}; }
    virtual void tempo(int, double) {}
    virtual void pitchbend(int, double) {}
    virtual void keylock(int, bool) {}
    virtual void trackKey(int, const QString&) {}
    virtual void performanceControl(int, const QString&, double) {}
    virtual void sync(int, bool) {}
    /// Make one deck the sync leader. Without this the engine elects its own
    /// and the follower locks to a tempo the caller never chose.
    virtual void setSyncLeader(int) {}
    /// Index of the current sync leader, or -1 when no deck holds it.
    virtual int syncLeader() const { return -1; }
    virtual void quantize(int, bool) {}
    virtual void hotcue(int, int, const QString&, std::optional<double> = std::nullopt) {}
    virtual void loop(int, double, double) {}
    virtual void loopEnable(int, bool) {}
    virtual void beatjump(int, double) {}
    virtual void beatloop(int, double) {}
    virtual QJsonObject performanceState(int) const { return {}; }
    virtual void eq(int, const QString&, double) {}
    virtual void filter(int, double) {}
    virtual void trim(int, double) {}
    virtual bool fx(int, const QString&, bool, double, double) { return false; }
    virtual bool resetFx() { return false; }
    /// 録音の保存先を差し替える。次の録音から反映される。
    virtual QString recordingDirectory(const QString&) { return {}; }
    /// このビルドで実際に書き出せる形式。ビルド時のエンコーダ構成で決まる。
    virtual QJsonArray recordingFormats() const { return {}; }
    /// 保存形式を差し替える。次の録音から反映される。
    virtual QString recordingFormat(const QString&) { return {}; }
    virtual bool beatgrid(int, double, double, const std::optional<QVector<double>>& = std::nullopt) { return false; }
    virtual QJsonObject beatgridState(int) const { return {}; }
    virtual double effectiveBpm(int) const { return 0; }
    virtual double playbackRate(int) const { return 1; }
    virtual double positionMs(int deck) const = 0;
    /** 直近の拍に丸めた現在位置。グリッドが無ければ現在位置そのもの。 */
    virtual double quantizedPositionMs(int deck) const { return positionMs(deck); }
    virtual bool playing(int deck) const = 0;
    virtual void gain(int, double) {}
    virtual void masterGain(double) {}
    virtual void pfl(int, bool) {}
    virtual void orientation(int, int) {}
    virtual void crossfader(double) {}
    virtual QJsonObject recording() const {
        return {{"active", false}, {"path", QJsonValue::Null}, {"startedAt", QJsonValue::Null}, {"elapsedMs", 0}, {"error", "Recording is unavailable"}};
    }
    virtual void startRecording() {}
    virtual void stopRecording() {}
    std::function<void(int, quint64, QJsonObject, QString)> loaded;
    std::function<void(QJsonObject)> recordingChanged;
};
std::unique_ptr<PlaybackBackend> makeBackend();
