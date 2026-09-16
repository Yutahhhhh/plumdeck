#pragma once
#include "pcm_ring.h"
#include <functional>
#include <memory>
#include <QStringList>
namespace junction {
struct StreamManifest {
    QString streamId,producerPeerId;
    quint64 epoch=0,generation=0,mediaFrameOrigin=0;
    quint32 ssrc=0,rtpTimestampOrigin=0;
    quint32 codecLookaheadFrames=312;
};
/// Connectivity of one of the two peer connections, mapped from libdatachannel
/// so callers never depend on the vendored enum.
enum class LinkState { New, Connecting, Connected, Disconnected, Failed, Closed };

class MediaTransport {
public:
    struct Callbacks {
        std::function<void(bool,QString,QString,QString)> localDescription;
        std::function<void(bool,QString,QString)> localCandidate;
        std::function<void(QByteArray)> control,bulk,validation;
        std::function<void(QString)> error;
        std::function<void(StreamManifest)> producerManifest;
        /// ICE gathering finished on one connection. The aggregated local
        /// description is then complete and safe to export as a manual packet.
        /// Used only by the manual (signalling-free) exchange; the server mode
        /// keeps trickling candidates as before.
        std::function<void(bool)> gatheringComplete;
        std::function<void(bool,LinkState)> linkState;
    };
    struct Identity { QString certificatePath,keyPath,fingerprint; };
    static std::shared_ptr<Identity> createIdentity(const QString& directory,QString* error=nullptr);
    MediaTransport(QString authenticatedPeerId,QStringList iceServers,Callbacks callbacks,std::shared_ptr<Identity> identity={},bool forceRelay=false);
    ~MediaTransport();
    bool start(bool offerer,QString* error=nullptr);
    /// PlumDeck Lite uses one browser-compatible connection: the host offers a
    /// recv-only audio m-line and the guest answers with its MASTER track.
    bool startLite(bool host,QString* error=nullptr);
    void close();
    bool remoteDescription(bool bulk,const QString& sdp,const QString& type,const QString& authenticatedFingerprint,QString* error=nullptr);
    bool remoteCandidate(bool bulk,const QString& candidate,const QString& mid);
    bool sendControl(const QByteArray&),sendBulk(const QByteArray&),sendValidation(const QByteArray&);
    /// Keeps the normally idle asset-transfer connection alive without adding
    /// an application message to the transfer protocol.
    bool sendKeepAlive();
    bool setSendManifest(const StreamManifest&),setReceiveManifest(const StreamManifest&);
    /// Accept browser RTP without a Junction stream manifest. The first packet
    /// locks its SSRC/timestamp to the supplied session frame.
    void setBrowserReceive(quint64 epoch,quint64 mediaFrameOrigin);
    void setBrowserSendEpoch(quint64 epoch,quint64 mediaFrameOrigin);
    void startProducer(PcmRing*);
    /// Reattach a persistent producer ring after a handoff. The transport
    /// worker, as that ring's sole consumer, discards any old queued blocks.
    void restartProducer(PcmRing*);
    void inheritProducerHistory(MediaTransport& previous);
    void enableAutomaticManifest(bool enabled);
    bool acknowledgeSendManifest(const QString& streamId);
    PcmRing& decodedRing();
    PcmRing& preCodecRing();
    struct Statistics { quint64 receivedPackets=0,sentPackets=0,senderReports=0,receivedReports=0,nacksSent=0,retransmittedPackets=0; };
    Statistics statistics() const;
    bool selectedRelay(bool bulk=false) const;
    /// True once ICE gathering has completed on that connection.
    bool gatheringComplete(bool bulk) const;
    /// Both connections gathered: a non-trickle packet can now be exported.
    bool readyForManualExport() const;
    /// The aggregated local description, including every gathered candidate.
    /// Empty until gathering completes, so a half-collected offer can never be
    /// handed to the user. `type` and `fingerprint` are filled when given.
    QString aggregatedDescription(bool bulk,QString* type=nullptr,QString* fingerprint=nullptr) const;
    LinkState linkState(bool bulk=false) const;
    /// Worst of the two connections: a session is only really up when both are.
    LinkState aggregateLinkState() const;
    // Requests repair only while the packet is inside the decoder's deadline.
    bool requestRetransmission(quint64 mediaFrame);
    static bool available();
private:
    struct Impl; std::unique_ptr<Impl> d;
};
}
