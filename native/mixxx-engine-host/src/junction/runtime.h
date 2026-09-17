#pragma once
#include "authority.h"
#include "audio_clock.h"
#include "producer_tap.h"
#include <QObject>
#include <QTimer>
#include <memory>
#include <functional>
class PlaybackBackend;
namespace junction {
class Runtime final : public QObject {
public:
    explicit Runtime(PlaybackBackend*,QObject* parent=nullptr);
    ~Runtime();
    /// Host teardown: stop and detach while the backend is alive. Runtime
    /// storage remains valid until the backend has joined its audio callback.
    void detachBackend();
    QJsonObject command(const QString& op,const QJsonObject& params,QString* error);
    QJsonObject snapshot() const;
    bool active() const;
    quint64 currentMediaFrame() const;
    void setCaptureAnchor(quint64 sourceFrame,quint64 mediaFrame);
    quint64 mediaFrameForSource(quint64 sourceFrame,unsigned rate=44100) const;
    QString authorize(const QString&,const QJsonObject&) const;
    /// `local`: the LOCAL NEXT bus of the same callback (may be null).
    void capture(const float* master,const float* local,unsigned frames,quint64 sourceFrame,unsigned rate) noexcept;
    void captureLocalReturn(const float*,unsigned frames,quint64 sourceFrame,unsigned rate) noexcept;
    bool localMasterAudible() const noexcept;
    /// Audio thread: the JUNCTION deck's next `frames` stereo frames at 44.1 kHz.
    void readJunctionInput(float* out,unsigned frames) noexcept;
    /// Performer side: candidate local decks. Runtime selects current + next;
    /// paths are hashed here and never announced.
    std::function<QJsonArray()> localDeckTracks;
private:
    struct Impl; std::unique_ptr<Impl> d;
};
}
