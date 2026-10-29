#pragma once
#include "junction_input.h"
#include "pcm_ring.h"
#include "program_output.h"
#include <QByteArray>
#include <QJsonObject>
#include <QObject>
#include <QPointer>
#include <QTimer>
#include <atomic>
#include <vector>
class PlaybackBackend;
class QTcpServer;
class QTcpSocket;
struct SRC_STATE_tag;
namespace junction {
/// Junction v2 audio bridge between the engine and the WebView.
///
/// Junction v2 runs WebRTC in the WebView (RTCPeerConnection, Opus). This
/// class only moves PCM between that WebView and the engine over a loopback
/// WebSocket (127.0.0.1, random port, random token handed out through the
/// NDJSON `jlink.start` result):
///
///   kind 1  WebView -> engine  48 kHz stereo for the JUNCTION deck ([Auxiliary1])
///   kind 2  WebView -> engine  48 kHz stereo program for the venue/stream device
///   kind 3  engine -> WebView  48 kHz stereo uplink: the main mix before the
///                              master knob ("full", includes the J deck) or the
///                              local decks only ("local", never J or the mic)
///
/// Binary message = 16-byte little-endian header {u8 kind, u8 channels(2),
/// u16 reserved, u32 frames, f64 first frame index} + float32 interleaved PCM.
///
/// Clock domains: the J input and the program each go through an existing
/// ASRC stage (JunctionInput, ProgramOutput) that follows its own buffer fill;
/// the uplink is converted 44.1 -> 48 kHz here and the WebView's elastic buffer
/// absorbs the drift. Session/turn logic, the splice and the network all live
/// in the WebView; nothing here knows about other DJs.
class Link final : public QObject {
public:
    explicit Link(PlaybackBackend*,QObject* parent=nullptr);
    ~Link() override;
    /// Main thread. `op` without the "jlink." prefix.
    QJsonObject command(const QString& op,const QJsonObject& params,QString* error);
    QJsonObject state() const;
    bool active() const {return active_.load(std::memory_order_acquire);}
    /// Host teardown: stop before the backend goes away.
    void detachBackend();
    /// Audio thread: next JUNCTION deck frames at the engine rate.
    void readInput(float* out,unsigned frames) noexcept;
    /// Audio thread: one engine block. `full` is the main mix before the
    /// master knob, `local` the J-less local return bus (may be null).
    void capture(const float* full,const float* local,unsigned frames) noexcept;
    static constexpr unsigned kEngineRate=44100;
private:
    enum class Uplink:int {Off=0,Full=1,Local=2};
    void accept();
    void readSocket();
    void handleFrame(int opcode,const QByteArray& payload);
    void handleAudio(const QByteArray& payload);
    void sendFrame(int opcode,const char* data,qsizetype size);
    void pump();
    void stop();
    PlaybackBackend* backend_;
    QTcpServer* server_=nullptr;
    QPointer<QTcpSocket> client_;
    QByteArray inbound_,fragment_;
    int fragmentOpcode_=0;
    bool upgraded_=false;
    QString token_;
    QTimer pumpTimer_;
    std::atomic<bool> active_{false};
    std::atomic<int> uplink_{0};
    JunctionInput input_;
    ProgramOutput program_;
    bool programOpen_=false;
    QString programDevice_;
    PcmRing uplinkRing_{256,4096,2};
    SRC_STATE_tag* uplinkSrc_=nullptr;
    std::vector<float> uplinkIn_,uplinkOut_;
    double uplinkFrame_=0;
    quint64 captureFrame_=0,inputSeq_=0,programSeq_=0,generation_=1;
    quint64 framesIn_=0,framesProgram_=0,framesUp_=0,dropped_=0,rejected_=0;
};
}
