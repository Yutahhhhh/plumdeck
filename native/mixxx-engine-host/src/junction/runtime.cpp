#include "../sound_file.h"
#include "runtime.h"
#include "media_transport.h"
#include "program_output.h"
#include "junction_input.h"
#include "asset_cache.h"
#include "validation.h"
#include "validation_capture.h"
#include "manual_exchange.h"
#include "ice_servers.h"
#include "network_settings.h"
#include "shared_tracks.h"
#include <samplerate.h>
#include "../backend.h"
#include <QJsonDocument>
#include <QDebug>
#include <QUrl>
#include <QUrlQuery>
#include <QRegularExpression>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QDateTime>
#include <QCryptographicHash>
#include <QtEndian>
#include <QFile>
#include <QFileInfo>
#include <QSaveFile>
#include "../platform_file.h"
#ifdef __APPLE__
#include <IOKit/pwr_mgt/IOPMLib.h>
#endif
#include <QDir>
#include <QPointer>
#include <QThread>
#include <QQueue>
#include <future>
#include <algorithm>
#include <cmath>
#include <array>
#include <deque>
#include <map>
#include <vector>
#include <sndfile.h>
#if defined(PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL)
#include <rtc/rtc.hpp>
#endif
namespace junction {
namespace {
// Desktop screen sharing, power management and a busy audio engine can all
// pause delivery for several seconds. A missing heartbeat is a degradation,
// not permission to destroy a connection which WebRTC may still recover.
constexpr qint64 kControlSilenceNanos=10000000000LL;
constexpr quint64 kSnapshotIntervalTicks=100; // 500 ms at the 5 ms session tick.
// A Junction Live asset copy is background work: one 32 KiB chunk per ~50 ms keeps
// the performer uplink free for Program audio, and verification hashes the
// received file in slices so the coordinator tick (Program routing) never stalls.
constexpr quint64 kBackgroundChunkTicks=10;
constexpr qint64 kTrackDigestSliceBytes=512*1024;
constexpr qint64 kTrackTransferStallNanos=30000000000LL;
QByteArray json(const QJsonObject& v) {return QJsonDocument(v).toJson(QJsonDocument::Compact);}
// This identifies the minimum wire-compatible Junction protocol, not the app
// release. Additive features are negotiated independently in peer.hello so a
// newer plumdeck can still connect to an older compatible release.
QString fingerprint() {return QStringLiteral("mixxx-3ebac449e7e5fe2a0186596657696e87ce8b0e56-junction-5");}
constexpr auto kJunctionTracksCapability="junction-live-monitor-v1";
bool exchangeAwaitingConnection(ExchangeState state) {
    // The answerer has no signalling channel on which the offerer can announce
    // approval. Its first reliable approval signal is both WebRTC links being
    // connected, while its local exchange state still says response_ready (or
    // awaiting_host in clients which explicitly acknowledge copying).
    return state==ExchangeState::ResponseReady||state==ExchangeState::AwaitingHost
        ||state==ExchangeState::Connecting;
}
bool validOrigin(const QUrl& u) {
    return u.isValid() && !u.host().isEmpty() && u.userInfo().isEmpty() && u.fragment().isEmpty() && u.query().isEmpty() && (u.scheme()=="wss" || (u.scheme()=="ws" && (u.host()=="127.0.0.1" || u.host()=="localhost" || u.host()=="::1")));
}
QString profileColor(const QString& requested,const QString& seed) {
    static const QRegularExpression valid(QStringLiteral("^#[0-9a-fA-F]{6}$"));
    if(valid.match(requested).hasMatch())return requested.toUpper();
    static const std::array<const char*,8> palette={"#0EA5E9","#8B5CF6","#EC4899","#F97316","#22C55E","#14B8A6","#EAB308","#6366F1"};
    bool ok=false;const auto bucket=sha256Hex(seed.toUtf8()).left(2).toUInt(&ok,16);
    return QString::fromLatin1(palette[ok?bucket%palette.size():0]);
}
QString profileAvatar(const QString& requested) {
    // Avatars travel in the bounded control snapshot. SVG is deliberately not
    // accepted, and the UI must resize/compress before sending.
    if(requested.isEmpty())return {};
    if(requested.size()>4096)return {};
    static const QRegularExpression valid(QStringLiteral("^data:image/(?:png|jpeg|webp);base64,[A-Za-z0-9+/=]+$"));
    return valid.match(requested).hasMatch()?requested:QString{};
}
QJsonObject streamJson(const StreamManifest& m) {return {{"streamId",m.streamId},{"producerPeerId",m.producerPeerId},{"epoch",u64(m.epoch)},{"generation",u64(m.generation)},{"mediaFrameOrigin",u64(m.mediaFrameOrigin)},{"ssrc",double(m.ssrc)},{"rtpTimestampOrigin",double(m.rtpTimestampOrigin)},{"codecLookaheadFrames",int(m.codecLookaheadFrames)}};}
std::optional<StreamManifest> readStream(const QJsonObject& o) {
    auto epoch=parseU64(o["epoch"]),gen=parseU64(o["generation"]),origin=parseU64(o["mediaFrameOrigin"]);
    if(!epoch || !gen || !origin || !validOpaqueId(o["streamId"].toString()) || !validOpaqueId(o["producerPeerId"].toString())) return {};
    const double ssrc=o["ssrc"].toDouble(-1),rtp=o["rtpTimestampOrigin"].toDouble(-1);
    if(ssrc<0 || ssrc>4294967295.0 || rtp<0 || rtp>4294967295.0 || std::floor(ssrc)!=ssrc || std::floor(rtp)!=rtp) return {};
    return StreamManifest{o["streamId"].toString(),o["producerPeerId"].toString(),*epoch,*gen,*origin,quint32(ssrc),quint32(rtp)};
}
}
struct Runtime::Impl {
    Runtime* q;
#ifdef __APPLE__
    IOPMAssertionID sleepLease=kIOPMNullAssertionID;
#endif
    PlaybackBackend* backend;Authority auth;MediaTimeline timeline;ClockEstimator clock;
    ProducerTap tap;ProgramOutput program;AssetCache cache;
    QTemporaryDir identityDir;
    std::shared_ptr<MediaTransport::Identity> identity;
    // One manual offer/answer round for one DJ. The peer id it belongs to is
    // stable across re-exchange; only `generation` and `attempt` move.
    struct ManualAttempt {
        ExchangeState state=ExchangeState::Idle;
        QString inviteId,inviteText,responseText,noticeText,detail,errorCode,waitingFor=QStringLiteral("none");
        QJsonArray ice;
        QString answerSdp[2],answerType[2],answerFingerprint,answerName,answerDigest;
        bool answerPending=false;
        quint64 generation=0,attempt=0;
        qint64 expiresAt=0,collectDeadline=0,connectDeadline=0;
        unsigned retries=0;
        void clearArtifacts(){
            inviteText.clear();responseText.clear();answerPending=false;answerFingerprint.clear();
            answerName.clear();answerDigest.clear();
            for(auto& value:answerSdp)value.clear();
            for(auto& value:answerType)value.clear();
        }
    };
    // `candidate` is a replacement connection attempt built while `transport`
    // is still carrying audio. It is promoted only once it actually connects,
    // so a manual re-exchange never interrupts an established peer.
    struct PendingControl {QString type;QByteArray bytes;};
    struct Peer {QString id,name,fp,avatarDataUrl,themeColor;bool approved=false,hello=false,junctionTracksV1=false,producing=false,remoteStreamReady=false,endingAck=false,lite=false;QString liteSdp,liteSdpType;QJsonArray liteDecks;qint64 liteDecksAt=0;std::unique_ptr<MediaTransport> transport;QQueue<PendingControl> pending;
        qint64 lastControlAt=0,healthAt=0,bulkDisconnectedAt=0;quint64 serial=0,candidateSerial=0;std::unique_ptr<MediaTransport> candidate,retiring; qint64 retireAt=0;ManualAttempt manual;HealthReportMessage health;bool hasHealth=false;
        MediaTransport::Statistics healthStats;bool healthStatsReady=false,probeReady=false;double lastProbeRttMs=0,smoothedRttMs=0,smoothedJitterMs=0;};
    std::map<QString,std::unique_ptr<Peer>> peers;
#if defined(PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL)
    std::shared_ptr<rtc::WebSocket> signal;
#endif
    QTimer timer;quint64 ticks=0,controlSeq=0;QString origin,room,name,displayName,avatarDataUrl,themeColor,token,recovery,invite,problem,connection="disconnected",programState="idle";
    QStringList iceServers;bool iceReady=false;QQueue<QJsonObject> deferredSignals;QJsonObject pendingInvite,graph,preparedGraph;QJsonArray participantRoster;
    QString lifecycle=QStringLiteral("live");QStringList rosterOrder,finishedOrder;QSet<QString> turnRequests;
    bool hosting=false,adopt=false,programOpened=false,prepared=false,ready=false,bootstrapStart=false,liteSession=false;
    QStringList reasons;quint32 delay=24000;int programDevice=-1,maxPeers=8;
    qint64 inviteExpiry=0,turnRefreshAt=0,fenceDeadline=0,startDeadline=0,lastSharedChange=0,reconnectAt=0,endingAt=0;bool endingHost=false;unsigned reconnectAttempts=0;bool graphDirty=false;quint64 generation=1,graphSeq=0,signalGeneration=0;
    bool aligned=false,finalCheckpoint=false,validationSent=false;
    quint64 validationStart=0,validationEnd=0;
    QString validationId;unsigned validationRound=0;
    ValidationCapture validationCapture;
    std::map<QString,std::vector<float>> validationPcm;
    struct ValidationReceive {QString sender;QByteArray bytes;std::vector<bool> chunks;};
    std::map<QString,ValidationReceive> validationReceiving;
    QQueue<QByteArray> validationOutgoing;
    QString validationTarget;
    struct ProgramBlock {PcmBlockInfo info;std::vector<float> samples;};
    std::map<quint64,ProgramBlock> programPending,backupPending;
    quint64 programEnqueuedThrough=0; qint64 ownerAudioAt=0;quint64 recoveryUntil=0,recoveryResumeFrame=0,recoveryEpoch=0;
    QString backupOwner;quint64 backupEpoch=0;
    // --- JUNCTION deck: another DJ's audio as a local mixer channel --------
    // The previous Lite owner keeps sounding here after an operator switch
    // until the new operator fades it out (`releasingPeer`).
    JunctionInput input;QString inputPeer,releasingPeer;InputRelease inputRelease;
    QSet<QString> liteSenders; // Lite guest: peers the host still wants to hear.
    // Each Lite connection owns one outbound SPSC ring for its entire session.
    // A ring is never reassigned to another transport worker or freed while
    // the audio callback may still hold its pointer.
    std::map<QString,std::unique_ptr<PcmRing>> liteReturnRings;
    std::atomic<PcmRing*> localReturnTarget{nullptr};
    std::atomic<unsigned> localReturnReaders{0};
    std::atomic<quint64> returnEpoch{0},returnGeneration{1},returnSequence{0};
    QString returnRelaySource,returnRelayTarget;
    // The Lite->Mac seam. `seam` names the outgoing DJ whose direct stream must
    // keep reaching Program; `seamFrame` is the boundary it reaches, measured
    // by the first captured block instead of guessed from the clock, and 0
    // until that block lands. `takeoverAnchor` arms that one measurement.
    struct InputSeam {QString oldOwner;quint64 oldEpoch=0;};std::optional<InputSeam> seam;
    struct LaneBucket {float peak=0;QString deck;};std::deque<LaneBucket> lane;float lanePeak=0;quint32 laneFrames=0;

    std::atomic<bool> captureEnabled{false},audible{true},mainAudible{true},separateLocalMaster{true};
    std::atomic<quint64> lastCaptureSourceEnd{0};
    std::atomic<quint64> blockSequence{0},captureEpoch{1},sourceAnchor{UINT64_MAX},mediaAnchor{0};
    std::atomic<qint64> timelineAnchor{0};
    std::atomic<quint64> pendingAnchorSource{0},pendingAnchorMedia{0},pendingAnchorRevision{0};
    quint64 appliedAnchorRevision=0;
    std::atomic<quint64> captureGeneration{1},scheduledEpoch{0},scheduledFrame{UINT64_MAX},seamFrame{0};
    std::atomic<bool> takeoverAnchor{false};
    std::array<float,8192> pcm{};
    std::map<QString,QString> assets;
    std::map<QString,QSet<QString>> assetWaiters;
    std::map<QString,QString> assetKinds;
    QString pendingGraphAsset;bool preserveWarmGraph=false;QString exportHandoff;
    QSet<QString> requestedAssets,pinnedAssets;
    QString validationAwaitingAck;
    struct Outgoing {QString peer,hash,path;quint64 offset=0;bool background=false;};
    QQueue<Outgoing> outgoing;
    std::future<std::pair<QJsonObject,std::map<QString,QString>>> exportJob;
    quint64 exportRevision=0;
    QJsonObject privateState;
    // --- Junction Live -----------------------------------------------------
    // Coordinator: tracks the remote performer has loaded. One asset is fetched
    // at a time and is listed as loadable only after local verification.
    SharedTrackList sharedTracks;QSet<QString> trackPins,droppedTransfers;QString trackTransfer,trackTransferSource;qint64 trackTransferAt=0;
    std::map<QString,std::pair<QString,quint64>> announceSeen;
    struct TrackDigest {QString hash,path;bool partial=false;QFile file;QCryptographicHash sha{QCryptographicHash::Sha256};quint64 size=0,done=0;};
    std::unique_ptr<TrackDigest> trackDigest;
    // Performer: deck files hashed off the main thread. Paths never leave here.
    struct HashedFile {qint64 size=0,modified=0;QString hash;};
    std::map<QString,HashedFile> deckHashes;
    std::future<std::vector<std::pair<QString,HashedFile>>> deckHashJob;
    QString announceStream,announcedTracks;qint64 announcedAt=0;quint64 announceRevision=0,backgroundPumpTick=0;
    // --- manual (signalling-free) exchange -------------------------------
    bool manual=false;QByteArray hostCertificatePem;QString pinnedHostFingerprint;
    quint64 serialCounter=0;
    QString manualDetail,manualErrorCode;
    std::unique_ptr<MediaTransport> networkProbe;
    QJsonObject networkTestResult;
    qint64 networkTestDeadline=0;
    NetworkSettings network;
    explicit Impl(Runtime* owner,PlaybackBackend* b):q(owner),backend(b),cache(QStandardPaths::writableLocation(QStandardPaths::CacheLocation)+"/junction") {
        timer.setInterval(5);QObject::connect(&timer,&QTimer::timeout,q,[this]{tick();});timer.start();
    }
    quint64 now() const {return hosting?timeline.now():timeline.frameAt(clock.toHostNanos(monotonicNanos()));}
    template<class F> void post(F fn) {QMetaObject::invokeMethod(q,std::move(fn),Qt::QueuedConnection);}
    void fail(const QString& text) {if(qEnvironmentVariableIsSet("PLUMDECK_JUNCTION_TRACE"))qWarning()<<"junction failure"<<hosting<<text;problem=text;reasons={text};ready=false;}
    void signalSend(QJsonObject m) {
#if defined(PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL)
        m["v"]=1;
        if(signal && signal->isOpen()) {try{signal->send(json(m).toStdString());}catch(const std::exception&){fail("接続サービスへ送信できません");}}
#else
        Q_UNUSED(m);
#endif
    }
    void queue(Peer& p,const QString& type,QJsonObject payload) {
        const auto bytes=json({{"version",1},{"sessionId",auth.sessionId},{"senderPeerId",auth.local},{"epoch",u64(auth.epoch)},{"messageId",secureRandomHex(12)},{"type",type},{"payload",payload}});
        // Snapshots and probes describe only the newest state. Coalescing them
        // prevents a briefly busy client from replaying stale traffic after it
        // resumes. A profile-bearing snapshot wins until it is actually sent.
        if(type=="session.snapshot"||type=="clock.probe")for(int index=p.pending.size()-1;index>=0;--index){
            auto& queued=p.pending[index];if(queued.type!=type)continue;
            if(type=="session.snapshot"&&queued.bytes.contains("\"avatarDataUrl\"")&&!bytes.contains("\"avatarDataUrl\""))return;
            queued.bytes=bytes;return;
        }
        if(p.pending.size()>=64&&(type=="session.snapshot"||type=="clock.probe"))return;
        if(p.pending.size()>=128) {p.pending.clear();if(manual)manualSetState(p,ExchangeState::NeedsExchange,"このDJへの制御通信が混雑しています。接続情報を作り直してください","control_backpressure");else fail("制御通信が混雑しています");return;}
        p.pending.enqueue(PendingControl{type,bytes});
    }
    void broadcast(const QString& type,const QJsonObject& payload) {for(auto& [id,p]:peers)if(p->approved&&p->transport&&!p->lite)queue(*p,type,payload);}
    void restoreLobby() {
        lifecycle="lobby";startDeadline=0;bootstrapStart=false;
        captureEnabled.store(false);tap.enable(false,false);for(auto& [id,p]:peers){Q_UNUSED(id);if(p->producing&&p->transport){p->transport->startProducer(nullptr);p->producing=false;}}
        if(hosting){program.close();programOpened=false;programState="idle";}
    }
    QJsonObject localProfile() const {
        return {{"fingerprint",fingerprint()},{"displayName",displayName},{"djName",displayName},{"avatarDataUrl",avatarDataUrl},{"themeColor",themeColor},{"junctionCapabilities",QJsonArray{kJunctionTracksCapability}}};
    }
    QJsonObject rosterMetadata(const QString& id) const {
        for(const auto& value:participantRoster){const auto row=value.toObject();if(row["peerId"].toString()==id)return row;}
        return {};
    }
    int rosterIndex(const QString& id) const {
        if(hosting){const int index=rosterOrder.indexOf(id);return index<0?999:index;}
        const auto row=rosterMetadata(id);return row.contains("orderIndex")?row["orderIndex"].toInt(999):999;
    }
    QString qualityLevel(double rttMs,double lossFraction) const {
        if(rttMs<=100.0&&lossFraction<=.01)return QStringLiteral("good");
        if(rttMs<=250.0&&lossFraction<=.05)return QStringLiteral("fair");
        return QStringLiteral("poor");
    }
    QJsonObject quality(const Peer* peer,bool local=false) const {
        if(local&&hosting)return {{"level","good"},{"rttMs",0.0},{"packetLossPct",0.0}};
        const Peer* measured=peer;
        if(local&&!hosting){
            auto host=peers.find(auth.host);if(host!=peers.end())measured=host->second.get();
        }
        if(measured&&manual){
            const auto state=measured->manual.state;
            if(state==ExchangeState::Interrupted)return {{"level","poor"}};
            if(state==ExchangeState::NeedsExchange||state==ExchangeState::Failed||state==ExchangeState::Expired||state==ExchangeState::Cancelled||state==ExchangeState::Rejected)return {{"level","offline"}};
        }
        if(!measured||!measured->approved||!measured->hello)return {{"level","unknown"}};
        // Health reports are expected once per second. Never leave an old green
        // result on screen after a suspended or half-open connection.
        if(!measured->hasHealth||!measured->healthAt||monotonicNanos()-measured->healthAt>4500000000LL){
            if(measured->transport&&measured->transport->linkState(false)!=LinkState::Connected)return {{"level","offline"}};
            return {{"level","unknown"}};
        }
        return {{"level",qualityLevel(measured->health.rttMs,measured->health.lossFraction)},{"rttMs",measured->health.rttMs},{"jitterMs",measured->health.jitterMs},{"packetLossPct",measured->health.lossFraction*100.0}};
    }
    QString rosterStatus(const QString& id,const Peer* peer,bool local=false) const {
        if(lifecycle=="live"&&id==auth.owner)return QStringLiteral("performing");
        if(id==auth.next)return ready?QStringLiteral("ready"):QStringLiteral("next");
        if(finishedOrder.contains(id))return QStringLiteral("finished");
        if(!hosting&&rosterMetadata(id)["rosterStatus"].toString()=="finished")return QStringLiteral("finished");
        if(turnRequests.contains(id)||(!hosting&&rosterMetadata(id)["turnRequested"].toBool()))return QStringLiteral("requested");
        if(local)return connection=="connected"?QStringLiteral("waiting"):connection;
        if(manual&&peer){
            switch(peer->manual.state){
            case ExchangeState::Idle:case ExchangeState::Collecting:case ExchangeState::InviteReady:case ExchangeState::AwaitingAnswer:return QStringLiteral("invited");
            case ExchangeState::ApprovalPending:case ExchangeState::ResponseReady:case ExchangeState::AwaitingHost:return QStringLiteral("approval_pending");
            case ExchangeState::Connecting:return QStringLiteral("connecting");
            case ExchangeState::Connected:return peer->hello?QStringLiteral("waiting"):QStringLiteral("connecting");
            case ExchangeState::Interrupted:return QStringLiteral("unstable");
            case ExchangeState::NeedsExchange:case ExchangeState::Failed:case ExchangeState::Expired:case ExchangeState::Cancelled:case ExchangeState::Rejected:return QStringLiteral("disconnected");
            }
        }
        return peer&&peer->hello?QStringLiteral("waiting"):peer&&peer->approved?QStringLiteral("connecting"):QStringLiteral("approval_pending");
    }
    void enrichRosterRow(QJsonObject& row,const QString& id,const Peer* peer,bool local=false) const {
        const auto synced=!hosting?rosterMetadata(id):QJsonObject{};
        const auto djName=local?displayName:peer&&!peer->name.isEmpty()?peer->name:sanitizeDisplayName(synced["djName"].toString(synced["displayName"].toString()));
        const auto avatar=local?avatarDataUrl:peer&&!peer->avatarDataUrl.isEmpty()?peer->avatarDataUrl:profileAvatar(synced["avatarDataUrl"].toString());
        const auto color=local?themeColor:peer&&!peer->themeColor.isEmpty()?peer->themeColor:profileColor(synced["themeColor"].toString(),id);
        row["displayName"]=djName;row["djName"]=djName;row["avatarDataUrl"]=avatar;row["themeColor"]=color;
        row["slotId"]=id;row["orderIndex"]=rosterIndex(id);row["isPlaceholder"]=peer&&!peer->approved;
        row["turnRequested"]=turnRequests.contains(id)||(!hosting&&synced["turnRequested"].toBool());
        if(peer&&!peer->manual.inviteId.isEmpty())row["invitationId"]=peer->manual.inviteId;
        row["rosterStatus"]=rosterStatus(id,peer,local);
        auto measured=quality(peer,local);if(!hosting&&!local&&peer&&!peer->hasHealth&&synced["connectionQuality"].isObject())measured=synced["connectionQuality"].toObject();
        row["connectionQuality"]=measured;
    }
    /// `wire` strips everything a remote peer must not see. Exchange packets
    /// carry another DJ's invite/response/notice text and never go on the wire.
    QJsonObject publicState(bool wire=false) const {
        std::vector<QJsonObject> rows;const bool isLive=lifecycle=="live";
        if(!auth.local.isEmpty()){QJsonObject row{{"peerId",auth.local},{"approved",true},{"isHost",auth.local==auth.host},{"isCoordinator",auth.local==auth.host},{"isPerformer",isLive&&auth.owner==auth.local},{"isNextUp",auth.next==auth.local},{"status",connection}};enrichRosterRow(row,auth.local,nullptr,true);rows.push_back(row);}
        for(const auto& [id,p]:peers){
            QJsonObject row{{"peerId",id},{"approved",p->approved},{"isHost",id==auth.host},{"isCoordinator",id==auth.host},{"isPerformer",isLive&&id==auth.owner},{"isNextUp",id==auth.next},{"status",p->hello?"connected":p->approved?"connecting":"pending"}};enrichRosterRow(row,id,p.get());
            // A Lite peer connects through liteSdp/hello, never the manual
            // packet exchange, so its idle exchange state must not be shown.
            if(manual&&!wire&&!p->lite)row["exchange"]=manualExchangeJson(*p);
            if(p->lite)row["client"]="lite";
            rows.push_back(row);
        }
        if(!hosting)for(const auto& value:participantRoster){const auto row=value.toObject();const auto id=row["peerId"].toString();if(!validOpaqueId(id)||id==auth.local||peers.count(id))continue;
            QJsonObject copy{{"peerId",id},{"displayName",sanitizeDisplayName(row["displayName"].toString())},{"djName",sanitizeDisplayName(row["djName"].toString(row["displayName"].toString()))},{"avatarDataUrl",profileAvatar(row["avatarDataUrl"].toString())},{"themeColor",profileColor(row["themeColor"].toString(),id)},{"approved",row["approved"].toBool()},{"isHost",id==auth.host},{"isCoordinator",id==auth.host},{"isPerformer",isLive&&id==auth.owner},{"isNextUp",id==auth.next},{"status",row["status"].toString()},{"slotId",id},{"orderIndex",row["orderIndex"].toInt(999)},{"rosterStatus",row["rosterStatus"].toString()},{"turnRequested",row["turnRequested"].toBool()},{"connectionQuality",row["connectionQuality"].toObject()},{"isPlaceholder",row["isPlaceholder"].toBool()}};
            if(row["invitationId"].isString())copy["invitationId"]=row["invitationId"];
            rows.push_back(copy);
        }
        std::stable_sort(rows.begin(),rows.end(),[](const QJsonObject& a,const QJsonObject& b){return a["orderIndex"].toInt(999)<b["orderIndex"].toInt(999);});
        QJsonArray participants;for(const auto& row:rows)participants.append(row);
        QJsonArray why;for(const auto& r:reasons)why.append(r);
        QJsonObject state{{"active",!auth.sessionId.isEmpty()},{"sessionId",auth.sessionId},{"localPeerId",auth.local},{"hostPeerId",auth.host},{"coordinatorPeerId",auth.host},{"performerPeerId",isLive?auth.owner:QString{}},{"nextPeerId",auth.next},{"epoch",u64(auth.epoch)},{"revision",double(auth.revision)},{"sessionName",name},{"lifecycle",lifecycle},{"handoffState",auth.phase},{"handoffId",auth.handoffId},{"participants",participants},{"readiness",QJsonObject{{"ready",ready},{"reasons",why}}},{"connection",QJsonObject{{"state",connection},{"detail",problem}}},{"program",QJsonObject{{"state",programState},{"captureActive",captureEnabled.load()},{"localMonitor",separateLocalMaster.load()?"direct":"program-delayed"},{"outputDevice",QString::number(programDevice)},{"recording",program.recording()},{"underruns",u64(program.underruns())},{"meter",double(program.peak())},{"rms",double(program.rms())},{"sampleRateHz",int(program.sampleRate())},{"deviceLatencySeconds",program.deviceLatencySeconds()}}},{"invite",hosting?invite:QString{}},{"privatePreview",backend->privatePreviewState()},{"exchange",wire?QJsonObject{{"mode",manual?QStringLiteral("manual"):QStringLiteral("server")}}:exchangeState()}};
        // Junction Live cache paths are local-only. Only the coordinator while
        // another peer performs needs the presentation pair.
        if(!wire){state["localPrep"]=localPrep();state["junctionInput"]=inputState();}
        if(!wire)state["junctionTracks"]=(hosting&&auth.owner!=auth.local)
            ?sharedTracks.toJson([this](const SharedTrackList::Entry& entry){return trackProgress(entry);})
            :QJsonArray{};
        return state;
    }
    /// Session-level exchange summary. For a guest it mirrors the attempt with
    /// the host, which is the only one it has.
    QJsonObject exchangeState() const {
        QJsonObject value{{"mode",manual?QStringLiteral("manual"):QStringLiteral("server")}};
        if(!manual){value["state"]=exchangeStateName(connection=="connected"?ExchangeState::Connected:connection=="disconnected"?ExchangeState::Idle:ExchangeState::Connecting);
            if(!problem.isEmpty())value["detail"]=problem;return value;}
        const Peer* subject=nullptr;
        if(!hosting){auto i=peers.find(auth.host);if(i!=peers.end())subject=i->second.get();}
        else{
            // The host's own card summarises the attempt that most needs the
            // user: an approval first, then anything still being produced.
            for(const auto& [id,p]:peers){
                const auto state=p->manual.state;
                if(state==ExchangeState::ApprovalPending){subject=p.get();break;}
                if(!subject&&(state==ExchangeState::Collecting||state==ExchangeState::InviteReady))subject=p.get();
            }
        }
        value["state"]=exchangeStateName(subject?subject->manual.state:ExchangeState::Idle);
        if(subject){
            if(!subject->manual.detail.isEmpty())value["detail"]=subject->manual.detail;
            if(!subject->manual.errorCode.isEmpty())value["errorCode"]=subject->manual.errorCode;
            if(!subject->manual.responseText.isEmpty())value["responseText"]=subject->manual.responseText;
            if(!subject->manual.inviteId.isEmpty())value["inviteId"]=subject->manual.inviteId;
            if(subject->manual.expiresAt)value["expiresAt"]=double(subject->manual.expiresAt);
        }else if(!manualDetail.isEmpty())value["detail"]=manualDetail;
        if(!manualErrorCode.isEmpty())value["errorCode"]=manualErrorCode;
        return value;
    }
    void updateInvite() {
        if(!hosting || room.isEmpty())return;
        QUrl u("plumdeck-junction://join");QUrlQuery query;
        query.addQueryItem("version","1");query.addQueryItem("signaling",origin);query.addQueryItem("room",room);query.addQueryItem("token",token);query.addQueryItem("host",identity->fingerprint);query.addQueryItem("expiresAt",QString::number(inviteExpiry));u.setQuery(query);invite=u.toString(QUrl::FullyEncoded);
    }
    QString connect(bool host) {
        hosting=host;QString error;
#ifdef __APPLE__
        if(sleepLease==kIOPMNullAssertionID)IOPMAssertionCreateWithName(kIOPMAssertionTypePreventUserIdleSystemSleep,kIOPMAssertionLevelOn,CFSTR("plumdeck Junction audio session"),&sleepLease);
#elif defined(_WIN32)
        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
#endif
        if(!identity)identity=MediaTransport::createIdentity(identityDir.path(),&error);if(!identity)return error;
#if defined(PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL)
        rtc::WebSocket::Configuration config;config.maxMessageSize=65536;
        signal=std::make_shared<rtc::WebSocket>(config);
        QPointer<Runtime> safe=q;const auto serial=++signalGeneration;
        signal->onMessage([safe,serial](rtc::message_variant data){if(!safe)return;if(auto* s=std::get_if<std::string>(&data)){if(s->size()>65536)return;auto bytes=QByteArray(s->data(),int(s->size()));QMetaObject::invokeMethod(safe,[safe,bytes,serial]{if(safe&&safe->d->signalGeneration==serial)safe->d->onSignal(QJsonDocument::fromJson(bytes).object());},Qt::QueuedConnection);}});
        signal->onClosed([safe,serial]{if(safe)QMetaObject::invokeMethod(safe,[safe,serial]{if(safe&&safe->d->signalGeneration==serial){safe->d->connection="reconnecting";if(safe->d->hosting)safe->d->reconnectAt=monotonicNanos()+1000000000LL;safe->d->problem="接続サービスとの接続が切れました。確立済み音声は継続します";}},Qt::QueuedConnection);});
        signal->onError([safe,serial](std::string){if(safe)QMetaObject::invokeMethod(safe,[safe,serial]{if(safe&&safe->d->signalGeneration==serial){safe->d->fail("接続サービスへ接続できません");if(safe->d->hosting)safe->d->reconnectAt=monotonicNanos()+2000000000LL;}},Qt::QueuedConnection);});
        try{signal->open(origin.toStdString());}catch(const std::exception&){return "接続サービスへ接続できません";}
        connection="connecting";return {};
#else
        return "このビルドにはWebRTCが含まれていません";
#endif
    }
    /// Brings up a manual session without contacting a signaling service. The
    /// host mints its own identifiers and either waits in the lobby or starts
    /// immediately, according to the lifecycle selected by the caller.
    QString startManual(bool host) {
        manual=true;hosting=host;QString error;
#ifdef __APPLE__
        if(sleepLease==kIOPMNullAssertionID)IOPMAssertionCreateWithName(kIOPMAssertionTypePreventUserIdleSystemSleep,kIOPMAssertionLevelOn,CFSTR("plumdeck Junction audio session"),&sleepLease);
#elif defined(_WIN32)
        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
#endif
        if(!MediaTransport::available())return "このビルドにはWebRTCが含まれていません";
        if(!identity)identity=MediaTransport::createIdentity(identityDir.path(),&error);
        if(!identity)return error.isEmpty()?QStringLiteral("この端末の識別情報を作成できません"):error;
        hostCertificatePem=readCertificatePem(identity->certificatePath);
        if(hostCertificatePem.isEmpty())return "この端末の証明書を読み込めません";
        // Manual mode needs no negotiated TURN credentials before it can build
        // a transport; servers are resolved per attempt from local settings.
        iceReady=true;
        if(host){
            auth.local=secureRandomHex(16);auth.host=auth.local;auth.owner=auth.local;
            if(!rosterOrder.contains(auth.local))rosterOrder.append(auth.local);
            pinnedHostFingerprint=identity->fingerprint;
            timeline.start(monotonicNanos());timelineAnchor.store(timeline.originNanos());
            if(lifecycle=="live"){captureEnabled.store(true);tap.enable(false,true);openProgram();}
            connection="connected";problem.clear();
        }
        return {};
    }
    void onSignal(const QJsonObject& m) {
        if(m["v"]!=1)return;
        const auto type=m["type"].toString();
        if(type=="server.hello") {
            if(hosting)signalSend({{"type","host.register"},{"roomLocator",room},{"inviteTokenHash",sha256Hex(token.toUtf8())},{"inviteExpiresAt",double(inviteExpiry)},{"hostFingerprint",identity->fingerprint},{"recoverySecret",recovery},{"sessionName",name},{"djName",displayName},{"avatarDataUrl",avatarDataUrl},{"themeColor",themeColor},{"maxPeers",maxPeers}});
            else signalSend({{"type","guest.join"},{"roomLocator",room},{"inviteToken",token},{"displayName",displayName},{"djName",displayName},{"avatarDataUrl",avatarDataUrl},{"themeColor",themeColor},{"peerFingerprint",identity->fingerprint}});
        } else if(type=="host.registered") {
            room=m["roomLocator"].toString();const auto registered=m["hostPeerId"].toString();
            if(auth.local.isEmpty()){auth.local=registered;auth.host=auth.local;auth.owner=auth.local;if(!rosterOrder.contains(auth.local))rosterOrder.append(auth.local);timeline.start(monotonicNanos());timelineAnchor.store(timeline.originNanos());if(lifecycle=="live"){captureEnabled.store(true);tap.enable(false,true);}}
            else if(registered!=auth.local){fail("接続サービスの識別情報が変わりました。確立済み音声は継続します");return;}
            reconnectAt=0;reconnectAttempts=0;connection="connected";problem.clear();signalSend({{"type","turn.credentials"}});updateInvite();if(lifecycle=="live")openProgram();
        } else if(type=="join.pending") {auth.local=m["peerId"].toString();auth.host=m["hostPeerId"].toString();connection="pending";}
        else if(type=="join.accepted") {auth.local=m["peerId"].toString();auth.host=m["hostPeerId"].toString();connection="connecting";auto p=std::make_unique<Peer>();p->id=auth.host;p->name="ホスト";p->fp=pendingInvite["host"].toString();p->approved=true;peers[auth.host]=std::move(p);signalSend({{"type","turn.credentials"}});}
        else if(type=="room.join_request" && hosting) {const auto id=m["guestPeerId"].toString();if(!validOpaqueId(id))return;auto p=std::make_unique<Peer>();p->id=id;p->name=sanitizeDisplayName(m["djName"].toString(m["displayName"].toString()));p->avatarDataUrl=profileAvatar(m["avatarDataUrl"].toString());p->themeColor=profileColor(m["themeColor"].toString(),id);p->fp=m["peerFingerprint"].toString();peers[id]=std::move(p);if(!rosterOrder.contains(id))rosterOrder.append(id);++auth.revision;broadcast("session.snapshot",wireState(true));}
        else if(type=="signal.deliver") {
            const auto id=m["fromPeerId"].toString();auto i=peers.find(id);if(i==peers.end()||!i->second->approved)return;auto& p=*i->second;if(!p.transport)makePeer(p,false);
            const auto payload=m["payload"].toObject();QString error;
            if(payload["kind"]=="description") {if(!p.transport->remoteDescription(payload["bulk"].toBool(),payload["sdp"].toString(),payload["descriptionType"].toString(),p.fp,&error))fail(error);}
            else if(payload["kind"]=="candidate")p.transport->remoteCandidate(payload["bulk"].toBool(),payload["candidate"].toString(),payload["mid"].toString());
        } else if(type=="join.rejected" || type=="error") {fail(type=="join.rejected"?QStringLiteral("参加できません: ")+m["reason"].toString():m["message"].toString());connection="error";}
        else if(type=="signaling.unavailable") {connection="reconnecting";problem="接続サービスは利用できません。確立済みP2P音声は継続します";}
        else if(type=="room.closed") {stop();connection="disconnected";problem="ホストがセッションを終了しました";}
        else if(type=="peer.gone") {const auto id=m["peerId"].toString();if(id==auth.owner&&(!auth.committed||now()>=auth.cutoverFrame))beginRecovery("プレイ担当者との接続が切れました");peers.erase(id);rosterOrder.removeAll(id);finishedOrder.removeAll(id);turnRequests.remove(id);++auth.revision;}
        else if(type=="turn.credentials") {
            // Credentials stay native; never included in snapshots or logs.
            iceServers=iceServerUrls(m["iceServers"].toArray());
            const auto lifetime=std::clamp<qint64>(qint64(m["expiresAt"].toDouble())-QDateTime::currentMSecsSinceEpoch(),1000,3600000);
            turnRefreshAt=monotonicNanos()+std::max<qint64>(1000,lifetime-std::min<qint64>(60000,lifetime/2))*1000000;
            iceReady=true;for(auto& [id,p]:peers)if(p->approved&&!p->transport)makePeer(*p,hosting);
            while(!deferredSignals.isEmpty())onSignal(deferredSignals.dequeue());
        }
    }
    /// Resolves the slot a callback belongs to. A stale serial means the
    /// attempt was replaced or cancelled while the callback was in flight;
    /// every callback below drops out rather than touching the new attempt.
    MediaTransport* transportForSerial(Peer& p,quint64 serial) const {
        if(serial&&p.serial==serial)return p.transport.get();
        if(serial&&p.candidateSerial==serial)return p.candidate.get();
        return nullptr;
    }
    Peer* peerForSerial(const QString& id,quint64 serial) {
        auto i=peers.find(id);if(i==peers.end())return nullptr;
        return transportForSerial(*i->second,serial)?i->second.get():nullptr;
    }
    /// True while the callback belongs to the slot that is allowed to carry
    /// session traffic. A not-yet-promoted candidate is admitted first.
    bool liveSerial(Peer& p,quint64 serial) {
        if(p.candidateSerial==serial)manualPromoteIfReady(p);
        return p.serial==serial;
    }
    std::unique_ptr<MediaTransport> buildTransport(const QString& id,quint64 serial,bool offerer,const QStringList& servers,QString* error) {
        QPointer<Runtime> safe=q;
        MediaTransport::Callbacks callbacks;
        // Manual mode aggregates candidates instead of trickling them, so the
        // description/candidate callbacks below are inert without signalling.
        callbacks.gatheringComplete=[safe,id,serial](bool){if(safe)QMetaObject::invokeMethod(safe,[safe,id,serial]{if(!safe)return;auto* p=safe->d->peerForSerial(id,serial);if(p)safe->d->manualCollected(*p,serial);},Qt::QueuedConnection);};
        callbacks.linkState=[safe,id,serial](bool bulk,LinkState state){if(safe)QMetaObject::invokeMethod(safe,[safe,id,serial,bulk,state]{if(!safe)return;auto* p=safe->d->peerForSerial(id,serial);if(p)safe->d->manualLinkChanged(*p,serial,bulk,state);},Qt::QueuedConnection);};
        callbacks.localDescription=[safe,id,serial](bool bulk,QString sdp,QString type,QString){if(safe)QMetaObject::invokeMethod(safe,[safe,id,serial,bulk,sdp,type]{if(!safe||safe->d->manual)return;auto* p=safe->d->peerForSerial(id,serial);if(p)safe->d->signalSend({{"type","signal.relay"},{"toPeerId",id},{"payload",QJsonObject{{"kind","description"},{"bulk",bulk},{"sdp",sdp},{"descriptionType",type}}}});},Qt::QueuedConnection);};
        callbacks.localCandidate=[safe,id,serial](bool bulk,QString candidate,QString mid){if(safe)QMetaObject::invokeMethod(safe,[safe,id,serial,bulk,candidate,mid]{if(!safe||safe->d->manual)return;auto* p=safe->d->peerForSerial(id,serial);if(p)safe->d->signalSend({{"type","signal.relay"},{"toPeerId",id},{"payload",QJsonObject{{"kind","candidate"},{"bulk",bulk},{"candidate",candidate},{"mid",mid}}}});},Qt::QueuedConnection);};
        callbacks.producerManifest=[safe,id,serial](StreamManifest manifest){if(safe)QMetaObject::invokeMethod(safe,[safe,id,serial,manifest]{if(!safe)return;auto* p=safe->d->peerForSerial(id,serial);if(p&&safe->d->liveSerial(*p,serial)){auto profile=safe->d->localProfile();profile["stream"]=streamJson(manifest);safe->d->queue(*p,"peer.hello",profile);}},Qt::QueuedConnection);};
        callbacks.control=[safe,id,serial](QByteArray bytes){if(safe)QMetaObject::invokeMethod(safe,[safe,id,serial,bytes]{if(!safe)return;auto* p=safe->d->peerForSerial(id,serial);if(p&&safe->d->liveSerial(*p,serial))safe->d->control(id,bytes);},Qt::QueuedConnection);};
        callbacks.validation=[safe,id,serial](QByteArray bytes){if(safe)QMetaObject::invokeMethod(safe,[safe,id,serial,bytes]{if(!safe)return;auto* p=safe->d->peerForSerial(id,serial);if(p&&safe->d->liveSerial(*p,serial))safe->d->validationChunk(id,bytes);},Qt::QueuedConnection);};
        callbacks.bulk=[safe,id,serial](QByteArray bytes){if(safe)QMetaObject::invokeMethod(safe,[safe,id,serial,bytes]{if(!safe)return;auto* p=safe->d->peerForSerial(id,serial);if(p&&safe->d->liveSerial(*p,serial))safe->d->bulk(id,bytes);},Qt::QueuedConnection);};
        // A transport-level failure is scoped to its own peer. It must never
        // tear down the shared session or another DJ's connection.
        callbacks.error=[safe,id,serial](QString failure){if(safe)QMetaObject::invokeMethod(safe,[safe,id,serial,failure]{if(!safe)return;auto* p=safe->d->peerForSerial(id,serial);if(!p)return;
            if(safe->d->manual){if(serial!=safe->d->attemptSerial(*p)||exchangeStateTerminal(p->manual.state))return;safe->d->manualSetState(*p,ExchangeState::Failed,failure,QStringLiteral("transport"));}
            else safe->d->fail(failure);},Qt::QueuedConnection);};
        auto transport=std::make_unique<MediaTransport>(id,servers,std::move(callbacks),identity,qEnvironmentVariable("PLUMDECK_JUNCTION_FORCE_RELAY")=="1");
        if(!transport->start(offerer,error))return {};
        return transport;
    }
    void makePeer(Peer& p,bool offerer) {
        if(!iceReady||p.transport)return;
        const auto serial=++serialCounter;p.serial=serial;QString error;
        auto transport=buildTransport(p.id,serial,offerer,iceServers,&error);
        if(!transport){p.serial=0;fail(error);return;}
        p.transport=std::move(transport);
        queue(p,"peer.hello",localProfile());
        if(hosting)queue(p,"session.snapshot",wireState(true));
    }
    void sendLite(Peer& p,const QJsonObject& value) {
        if(p.lite&&p.transport)p.transport->sendControl(json(value));
    }
    /// `senderPeerIds`: peers whose audio must keep flowing. The operator is
    /// always one DJ; an outgoing Lite DJ stays a sender until released.
    QJsonObject liteOwnerMessage() const {
        QJsonArray senders{auth.owner};if(!releasingPeer.isEmpty()&&releasingPeer!=auth.owner)senders.append(releasingPeer);
        return {{"type","owner"},{"ownerPeerId",auth.owner},{"senderPeerIds",senders}};
    }
    PcmRing* returnRing(const QString& id) {
        auto& slot=liteReturnRings[id];if(!slot)slot=std::make_unique<PcmRing>(128,4096,2);return slot.get();
    }
    void stopLiteReturns() {
        localReturnTarget.store(nullptr,std::memory_order_release);returnRelaySource.clear();returnRelayTarget.clear();
        if(!hosting)return;
        for(auto& [id,p]:peers)if(p->lite&&p->transport)p->transport->startProducer(nullptr);
    }
    void startLiteReturn(const QString& previous,const QString& target) {
        stopLiteReturns();
        if(!hosting||target==auth.local||previous.isEmpty()||previous==target)return;
        auto found=peers.find(target);if(found==peers.end()||!found->second->lite||!found->second->transport)return;
        auto* ring=returnRing(target);const auto generation=returnGeneration.fetch_add(1,std::memory_order_relaxed)+1;
        StreamManifest manifest{secureRandomHex(12),auth.local,auth.epoch,generation,0,1,0};auto random=secureRandomBytes(8);
        manifest.ssrc=qFromBigEndian<quint32>(reinterpret_cast<const uchar*>(random.constData()));if(!manifest.ssrc)manifest.ssrc=1;
        manifest.rtpTimestampOrigin=qFromBigEndian<quint32>(reinterpret_cast<const uchar*>(random.constData()+4));
        found->second->transport->setSendManifest(manifest);found->second->transport->restartProducer(ring);
        returnEpoch.store(auth.epoch,std::memory_order_relaxed);
        if(previous==auth.local)localReturnTarget.store(ring,std::memory_order_release);
        else if(litePeer(previous)){returnRelaySource=previous;returnRelayTarget=target;}
    }
    void sendLiteOwner() {
        const auto message=liteOwnerMessage();
        for(auto& [id,p]:peers){Q_UNUSED(id);if(p->lite&&p->approved)sendLite(*p,message);}
    }
    bool litePeer(const QString& id) const {auto it=peers.find(id);return it!=peers.end()&&it->second->lite;}
    /// While a Lite DJ performs, this computer's decks are not shared: its DJ
    /// may prepare and cue locally, and only its own output device hears it.
    /// Only a receiver prepares: a DJ whose master is still being sent is
    /// never one, so its decks stay locked until the new operator releases it.
    bool localPrep() const {return !auth.sessionId.isEmpty()&&auth.owner!=auth.local&&!localSending()&&(liteSession||(hosting&&litePeer(auth.owner)));}
    /// An outgoing Lite DJ that the host still wants to hear after the switch.
    bool localSending() const {
        if(auth.owner==auth.local)return false;
        // A browser guest learns this from the host's senderPeerIds. The
        // desktop host has the same obligation when its own master is the
        // outgoing source being returned to the new Lite operator.
        return (liteSession&&!hosting&&liteSenders.contains(auth.local))
            || (hosting&&releasingPeer==auth.local);
    }
    QString junctionInputPeer() const {
        if(!hosting)return auth.owner==auth.local&&!releasingPeer.isEmpty()?auth.host:QString{};
        if(!releasingPeer.isEmpty())return litePeer(releasingPeer)?releasingPeer:QString{};
        return litePeer(auth.owner)?auth.owner:QString{};
    }
    void releaseInput() {
        if(releasingPeer.isEmpty())return;
        const auto released=releasingPeer;
        stopLiteReturns();
        releasingPeer.clear();inputRelease.clear();++auth.revision;
        backend->junctionInputMainMix(false);
        if(hosting)sendLiteOwner();
        else{auto host=peers.find(auth.host);if(host!=peers.end())sendLite(*host->second,{{"type","input-released"},{"senderPeerId",released}});}
    }
    QString audibleDeck(const Peer& p) const {
        if(p.liteDecks.isEmpty()||monotonicNanos()-p.liteDecksAt>3000000000LL)return {};
        QString best;double loudest=.001;
        for(const auto& value:p.liteDecks){const auto deck=value.toObject();const double level=deck["audibility"].toDouble()*(deck["playing"].toBool()?1:.5);if(level>loudest){loudest=level;best=deck["deck"].toString();}}
        return best;
    }
    QJsonArray displayedLiteDecks(const Peer& p) const {
        const auto ageNanos=monotonicNanos()-p.liteDecksAt;
        if(p.liteDecks.isEmpty()||ageNanos<0||ageNanos>3000000000LL)return {};
        const double elapsedMs=double(ageNanos)/1000000.0;
        QJsonArray result;
        for(const auto& value:p.liteDecks){
            auto deck=value.toObject();
            if(deck["playing"].toBool()){
                const double duration=deck["durationMs"].toDouble();
                const double maximum=duration>0?duration:86400000.0;
                deck["positionMs"]=std::clamp(deck["positionMs"].toDouble()+elapsedMs*deck["rate"].toDouble(1),0.0,maximum);
            }
            result.append(deck);
        }
        return result;
    }
    void feedInput(const float* samples,const PcmBlockInfo& info) {
        input.write(samples,info);
        const auto bucketFrames=info.sampleRateHz/25;if(!bucketFrames)return;
        for(quint32 i=0;i<info.frameCount;++i){
            lanePeak=std::max({lanePeak,std::abs(samples[size_t(i)*2]),std::abs(samples[size_t(i)*2+1])});
            if(++laneFrames<bucketFrames)continue;
            auto source=peers.find(inputPeer);
            lane.push_back(LaneBucket{std::min(lanePeak,1.f),source!=peers.end()?audibleDeck(*source->second):QString{}});
            while(lane.size()>300)lane.pop_front();
            lanePeak=0;laneFrames=0;
        }
    }
    void resetInput() {input.reset();lane.clear();lanePeak=0;laneFrames=0;}
    QJsonObject inputState() const {
        QJsonObject state{{"peerId",inputPeer},{"releasingPeerId",releasingPeer},{"receiving",!inputPeer.isEmpty()&&input.receiving()},
            {"latencyMs",double(input.latencyFrames48k())/48.0},{"underruns",u64(input.underruns())},{"channel",backend->junctionInputState()},
            {"seamFrame",u64(seamFrame.load(std::memory_order_acquire))},{"seamPending",takeoverAnchor.load(std::memory_order_acquire)}};
        auto source=peers.find(inputPeer);
        if(source!=peers.end()){
            state["djName"]=source->second->name;
            const auto decks=displayedLiteDecks(*source->second);if(!decks.isEmpty())state["decks"]=decks;
            state["audibleDeck"]=audibleDeck(*source->second);
        }
        QJsonArray peaks,decks;for(const auto& bucket:lane){peaks.append(std::round(bucket.peak*1000)/1000);decks.append(bucket.deck);}
        state["lane"]=QJsonObject{{"bucketMs",40},{"peaks",peaks},{"decks",decks}};
        return state;
    }
    void selectLiteOwner(const QString& target) {
        if(target!=auth.local){auto found=peers.find(target);if((found==peers.end()||!found->second->approved)&&!(liteSession&&validOpaqueId(target)))return;}
        const auto previous=auth.owner;if(previous==target)return;
        const bool previousLite=litePeer(previous);const auto previousEpoch=auth.epoch;
        auth.owner=target;auth.next.clear();auth.handoffId.clear();auth.committed.reset();auth.phase="playing";++auth.epoch;++auth.revision;
        turnRequests.remove(target);finishedOrder.removeAll(target);if(!previous.isEmpty()&&previous!=target){finishedOrder.removeAll(previous);finishedOrder.append(previous);}rosterOrder.removeAll(target);rosterOrder.prepend(target);
        ownerAudioAt=0;captureEpoch.store(auth.epoch);scheduledEpoch.store(0);scheduledFrame.store(UINT64_MAX);
        // Taking over from a Lite DJ: their audio stays on the JUNCTION deck,
        // so this master now carries it, delayed by whatever the deck buffered.
        // Only the audio callback can say by how much, so the seam is left
        // unmeasured here and the capture path fills it in.
        const bool takeOver=target==auth.local&&previous!=auth.local&&(hosting?previousLite:liteSession);
        // The new operator starts from an audible JUNCTION deck (unity, THRU,
        // flat EQ): stale faders must neither mute the outgoing DJ on Program
        // nor count as the fade-out that releases their stream.
        releasingPeer=!previous.isEmpty()&&previous!=target?previous:QString{};
        if(takeOver){backend->junctionInputTakeOver();backend->junctionInputMainMix(true);}
        else backend->junctionInputMainMix(false);
        if(!releasingPeer.isEmpty())inputRelease.begin(monotonicNanos());else inputRelease.clear();
        if(hosting)startLiteReturn(previous,target);
        seam.reset();seamFrame.store(0,std::memory_order_relaxed);
        // One shot, and armed only here: a later Mac-to-Mac handoff runs the
        // ordinary fenced cutover and must never inherit this anchor.
        const bool measureSeam=takeOver&&programOpened&&!previous.isEmpty();
        if(measureSeam)seam=InputSeam{previous,previousEpoch};
        takeoverAnchor.store(measureSeam,std::memory_order_release);
        const bool stillSending=localSending();
        if(target==auth.local){q->setCaptureAnchor(UINT64_MAX,0);captureEnabled.store(true);tap.enable(liteSession,true);}else if(!stillSending){captureEnabled.store(false);tap.enable(false,false);}
        for(auto& [id,p]:peers){
            if(p->lite&&p->transport){if((hosting&&id==target)||(!hosting&&id==auth.host&&target==auth.local))p->transport->setBrowserReceive(auth.epoch,now());if(!hosting&&id==auth.host){if(target==auth.local){p->transport->setBrowserSendEpoch(auth.epoch,now());p->transport->startProducer(&tap.networkRing());p->producing=true;}else if(!stillSending){p->transport->startProducer(nullptr);p->producing=false;}}}
        }
        audible.store(target==auth.local||localPrep());
        // Local preparation keeps the headphone bus alive, but it must never
        // leak this computer's master into the venue while a Lite peer owns it.
        mainAudible.store(target==auth.local);
        sendLiteOwner();broadcast("session.snapshot",wireState(false));
    }
    void liteControl(const QString& id,const QByteArray& bytes) {
        auto found=peers.find(id);if(found==peers.end()||!found->second->lite||bytes.size()>4096)return;
        QJsonParseError parse;const auto document=QJsonDocument::fromJson(bytes,&parse);if(parse.error!=QJsonParseError::NoError||!document.isObject())return;
        const auto message=document.object();const auto type=message["type"].toString();
        if(type=="handoff-request"&&hosting&&found->second->approved){turnRequests.insert(id);++auth.revision;return;}
        if(type=="input-released"&&hosting&&found->second->approved&&auth.owner==id&&!releasingPeer.isEmpty()){releaseInput();return;}
        if(type=="decks"&&hosting&&found->second->approved&&message["decks"].isArray()){
            QJsonArray decks;
            for(const auto& value:message["decks"].toArray()){if(decks.size()>=4)break;const auto deck=value.toObject();const auto name=deck["deck"].toString();if(name!="A"&&name!="B"&&name!="C"&&name!="D")continue;
                const auto number=[&](const char* key,double low,double high){const double v=deck[key].toDouble();return std::isfinite(v)?std::clamp(v,low,high):0.0;};
                QJsonObject row{{"deck",name},{"role",deck["role"].toString()=="current"?"current":"next"},{"title",deck["title"].toString().left(200)},{"artist",deck["artist"].toString().left(200)},
                    {"bpm",number("bpm",0,999)},{"positionMs",number("positionMs",0,86400000)},{"durationMs",number("durationMs",0,86400000)},{"rate",number("rate",0,4)},{"audibility",number("audibility",0,1)},{"playing",deck["playing"].toBool()},{"beatsPerBar",std::clamp(deck["beatsPerBar"].toInt(4),1,16)}};
                if(deck["firstBeatMs"].isDouble())row["firstBeatMs"]=number("firstBeatMs",-60000,86400000);
                decks.append(row);}
            found->second->liteDecks=decks;found->second->liteDecksAt=monotonicNanos();return;
        }
        if(type=="owner"&&!hosting&&id==auth.host){
            const auto owner=message["ownerPeerId"].toString();if(!validOpaqueId(owner))return;
            QSet<QString> senders{owner};if(message["senderPeerIds"].isArray())for(const auto& value:message["senderPeerIds"].toArray())if(validOpaqueId(value.toString()))senders.insert(value.toString());
            liteSenders=senders;selectLiteOwner(owner);
            // Released after the new operator faded this computer out.
            if(owner!=auth.local&&!liteSenders.contains(auth.local)&&found->second->producing){found->second->transport->startProducer(nullptr);found->second->producing=false;captureEnabled.store(false);tap.enable(false,false);++auth.revision;}
        }
    }
    QString buildLitePeer(Peer& p,bool host) {
        if(p.transport)return {};
        QPointer<Runtime> safe=q;const auto id=p.id;MediaTransport::Callbacks callbacks;
        callbacks.localDescription=[safe,id](bool,QString sdp,QString type,QString){if(safe)QMetaObject::invokeMethod(safe,[safe,id,sdp,type]{if(!safe)return;auto it=safe->d->peers.find(id);if(it==safe->d->peers.end()||!it->second->lite)return;it->second->liteSdp=sdp;it->second->liteSdpType=type;++safe->d->auth.revision;},Qt::QueuedConnection);};
        callbacks.linkState=[safe,id](bool,LinkState state){if(safe)QMetaObject::invokeMethod(safe,[safe,id,state]{if(!safe)return;auto it=safe->d->peers.find(id);if(it==safe->d->peers.end()||!it->second->lite)return;it->second->hello=state==LinkState::Connected;if(it->second->hello){safe->d->connection="connected";safe->d->problem.clear();safe->d->sendLiteOwner();}++safe->d->auth.revision;},Qt::QueuedConnection);};
        callbacks.control=[safe,id](QByteArray bytes){if(safe)QMetaObject::invokeMethod(safe,[safe,id,bytes]{if(safe)safe->d->liteControl(id,bytes);},Qt::QueuedConnection);};
        callbacks.error=[safe,id](QString failure){if(safe)QMetaObject::invokeMethod(safe,[safe,id,failure]{if(!safe)return;auto it=safe->d->peers.find(id);if(it!=safe->d->peers.end()){it->second->hello=false;safe->d->problem=failure;++safe->d->auth.revision;}},Qt::QueuedConnection);};
        auto transport=std::make_unique<MediaTransport>(p.id,QStringList{QStringLiteral("stun:stun.l.google.com:19302")},std::move(callbacks),identity,false);QString error;
        if(!transport->startLite(host,&error))return error.isEmpty()?QStringLiteral("PlumDeck Liteとの接続を開始できません"):error;
        p.transport=std::move(transport);p.lite=true;p.approved=true;
        if(host)p.transport->setBrowserReceive(auth.epoch,now());
        else{StreamManifest manifest{secureRandomHex(12),auth.local,auth.epoch,1,0,1,0};auto random=secureRandomBytes(8);manifest.ssrc=qFromBigEndian<quint32>(reinterpret_cast<const uchar*>(random.constData()));if(!manifest.ssrc)manifest.ssrc=1;manifest.rtpTimestampOrigin=qFromBigEndian<quint32>(reinterpret_cast<const uchar*>(random.constData()+4));p.transport->setSendManifest(manifest);p.transport->setBrowserReceive(auth.epoch,now());}
        return {};
    }
    // ---------------------------------------------------------------------
    // Manual exchange
    // ---------------------------------------------------------------------
    MediaTransport* attemptSlot(Peer& p) const {return p.candidate?p.candidate.get():p.transport.get();}
    quint64 attemptSerial(const Peer& p) const {return p.candidate?p.candidateSerial:p.serial;}
    void manualSetState(Peer& p,ExchangeState state,const QString& detail={},const QString& code={}) {
        p.manual.state=state;p.manual.detail=detail;p.manual.errorCode=code;
        if(!hosting&&!p.candidate){
            if(state==ExchangeState::Connected)connection="connected";
            else if(state==ExchangeState::Interrupted)connection="reconnecting";
            else if(state==ExchangeState::NeedsExchange||state==ExchangeState::Failed||state==ExchangeState::Expired||state==ExchangeState::Cancelled||state==ExchangeState::Rejected)connection="error";
            else if(state==ExchangeState::Collecting||state==ExchangeState::ResponseReady||state==ExchangeState::AwaitingHost||state==ExchangeState::Connecting)connection="connecting";
        }
        p.manual.waitingFor=state==ExchangeState::InviteReady?QStringLiteral("guest")
            :state==ExchangeState::ResponseReady||state==ExchangeState::AwaitingHost?QStringLiteral("host")
            :state==ExchangeState::ApprovalPending||state==ExchangeState::Collecting?QStringLiteral("local")
            :QStringLiteral("none");
        ++auth.revision;
    }
    // Transport-scoped state must not leak into a fresh connection. In
    // particular, an old heartbeat must not immediately interrupt the new link.
    void resetLinkHandshake(Peer& p) {
        p.hello=false;p.producing=false;p.remoteStreamReady=false;
        p.lastControlAt=monotonicNanos();p.hasHealth=false;p.healthAt=0;p.bulkDisconnectedAt=0;
        p.healthStatsReady=false;p.probeReady=false;p.lastProbeRttMs=0;
        p.smoothedRttMs=0;p.smoothedJitterMs=0;p.pending.clear();
    }
    /// Starts one connection attempt for `p`. When the peer already has a live
    /// transport the attempt is built alongside it as a replacement candidate,
    /// so an established connection keeps carrying audio until the new one is
    /// actually up.
    QString manualStartAttempt(Peer& p,bool offerer,const QJsonArray& remoteIce) {
        // ICE can still report Connected after a suspended/half-open link has
        // stopped carrying control traffic. Preserve only a recently live link.
        const auto now=monotonicNanos();
        const bool replacement=p.transport&&p.transport->linkState(false)==LinkState::Connected
            &&p.hello&&p.lastControlAt&&now-p.lastControlAt<=kControlSilenceNanos;
        const auto serial=++serialCounter;QString error;
        auto transport=buildTransport(p.id,serial,offerer,iceServerUrls(remoteIce),&error);
        if(!transport)return error.isEmpty()?QStringLiteral("接続を開始できません"):error;
        if(replacement){p.candidate=std::move(transport);p.candidateSerial=serial;}
        else{
            // Not connected: replace outright and drop any older candidate so
            // exactly one attempt is ever gathering for this peer.
            p.candidate.reset();p.candidateSerial=0;
            if(p.transport)p.transport->close();
            p.transport=std::move(transport);p.serial=serial;
            resetLinkHandshake(p);
        }
        p.manual.retries=0;p.manual.clearArtifacts();p.manual.noticeText.clear();
        p.manual.collectDeadline=monotonicNanos()+30000000000LL;p.manual.connectDeadline=0;
        manualSetState(p,ExchangeState::Collecting,QStringLiteral("音声・操作と楽曲転送の両方の接続先を収集しています。完了までお待ちください"));
        if(!replacement)queue(p,"peer.hello",localProfile());
        return {};
    }
    /// Both connections finished gathering: the aggregated descriptions are
    /// now complete and the packet can be produced.
    void manualCollected(Peer& p,quint64 serial) {
        if(!manual||serial!=attemptSerial(p))return;
        auto* transport=attemptSlot(p);
        if(!transport||!transport->readyForManualExport())return;
        if(p.manual.state!=ExchangeState::Collecting)return;
        p.manual.collectDeadline=0;
        const auto failure=hosting?manualBuildInvite(p):manualBuildResponse(p);
        if(!failure.isEmpty()){manualSetState(p,ExchangeState::Failed,failure,QStringLiteral("packet"));return;}
    }
    QString manualPacketBase(Peer& p,ExchangePacket& packet) const {
        packet.sessionId=auth.sessionId;packet.sessionName=name;packet.inviteId=p.manual.inviteId;
        packet.hostPeerId=auth.host;packet.hostName=hosting?displayName:p.name;
        packet.hostFingerprint=hosting?identity->fingerprint:pinnedHostFingerprint;
        packet.peerId=hosting?p.id:auth.local;packet.generation=p.manual.generation;packet.attempt=p.manual.attempt;
        packet.expiresAt=p.manual.expiresAt;
        if(packet.hostFingerprint.isEmpty())return QStringLiteral("ホストの識別情報がありません");
        return {};
    }
    QString manualSign(ExchangePacket& packet) const {
        if(hostCertificatePem.isEmpty())return QStringLiteral("この端末の証明書を読み込めません");
        packet.certificatePem=hostCertificatePem;QString failure;
        packet.signature=signExchangePayload(identity->keyPath,packet.canonicalPayload(),&failure);
        return packet.signature.isEmpty()?failure:QString{};
    }
    QString manualBuildInvite(Peer& p) {
        auto* transport=attemptSlot(p);if(!transport)return QStringLiteral("接続を開始できません");
        ExchangePacket packet;packet.kind=ExchangeKind::Invite;
        auto failure=manualPacketBase(p,packet);if(!failure.isEmpty())return failure;
        for(int index=0;index<2;++index){
            QString type,fp;const auto sdp=transport->aggregatedDescription(index==1,&type,&fp);
            if(sdp.isEmpty()||!sdp.contains("a=candidate:")||fp!=identity->fingerprint)return QStringLiteral("接続情報を作成できません");
            packet.description[index]={type,sdp};
        }
        if(hosting)packet.iceServers=p.manual.ice;
        failure=manualSign(packet);if(!failure.isEmpty())return failure;
        const auto text=encodeExchangePacket(packet,&failure);if(text.isEmpty())return failure;
        p.manual.inviteText=text;p.manual.responseText.clear();
        manualSetState(p,ExchangeState::InviteReady,QStringLiteral("この接続情報を相手に渡してください"));
        return {};
    }
    QString manualBuildResponse(Peer& p) {
        auto* transport=attemptSlot(p);if(!transport)return QStringLiteral("接続を開始できません");
        ExchangePacket packet;packet.kind=ExchangeKind::Response;
        auto failure=manualPacketBase(p,packet);if(!failure.isEmpty())return failure;
        packet.peerName=displayName;packet.peerFingerprint=identity->fingerprint;
        for(int index=0;index<2;++index){
            QString type,fp;const auto sdp=transport->aggregatedDescription(index==1,&type,&fp);
            if(sdp.isEmpty()||!sdp.contains("a=candidate:")||fp!=identity->fingerprint)return QStringLiteral("接続情報を作成できません");
            packet.description[index]={type,sdp};
        }
        if(hosting)packet.iceServers=p.manual.ice;
        failure=manualSign(packet);if(!failure.isEmpty())return failure;
        const auto text=encodeExchangePacket(packet,&failure);if(text.isEmpty())return failure;
        p.manual.responseText=text;
        // Producing the text is local work only. It is never evidence that the
        // host has received, read or accepted anything.
        manualSetState(p,ExchangeState::ResponseReady,QStringLiteral("この応答をホストに渡してください"));
        return {};
    }
    /// Applies an imported answer. Only reached after the host has approved.
    QString manualApplyAnswer(Peer& p) {
        auto* transport=attemptSlot(p);
        if(!transport||!p.manual.answerPending)return QStringLiteral("応答が読み込まれていません");
        for(int index=0;index<2;++index){
            QString failure;
            if(!transport->remoteDescription(index==1,p.manual.answerSdp[index],p.manual.answerType[index],p.manual.answerFingerprint,&failure))
                return failure.isEmpty()?QStringLiteral("応答を適用できません"):failure;
        }
        p.fp=p.manual.answerFingerprint;
        if(!p.manual.answerName.isEmpty())p.name=p.manual.answerName;
        p.manual.answerPending=false;
        p.manual.connectDeadline=monotonicNanos()+45000000000LL;
        manualSetState(p,ExchangeState::Connecting,QStringLiteral("接続しています"));
        return {};
    }
    /// Promotes a replacement candidate once it is genuinely connected. The
    /// previously established transport is only dropped at that point.
    void manualPromoteIfReady(Peer& p) {
        if(!p.candidate||p.candidate->aggregateLinkState()!=LinkState::Connected)return;
        if(p.transport){
            if(p.producing){p.transport->close();p.candidate->inheritProducerHistory(*p.transport);}
            else {p.retiring=std::move(p.transport);p.retireAt=monotonicNanos()+500000000LL;}
        }
        p.transport=std::move(p.candidate);p.serial=p.candidateSerial;
        p.candidate.reset();p.candidateSerial=0;
        // The new link starts from a clean handshake; the peer id, approval and
        // authenticated fingerprint are deliberately preserved.
        resetLinkHandshake(p);
        queue(p,"peer.hello",localProfile());
        if(hosting)queue(p,"session.snapshot",wireState(true));
        if(!hosting&&(auth.owner==auth.local||auth.next==auth.local))sendManifest(p);
        p.manual.collectDeadline=0;p.manual.connectDeadline=0;
        manualSetState(p,ExchangeState::Connected,QStringLiteral("接続しました"));
    }
    void manualLinkChanged(Peer& p,quint64 serial,bool bulk,LinkState state) {
        if(!manual)return;
        if(serial==p.candidateSerial){if(state==LinkState::Connected)manualPromoteIfReady(p);
            else if(state==LinkState::Failed)manualSetState(p,ExchangeState::NeedsExchange,QStringLiteral("再接続できませんでした。接続情報を作り直してください"),QStringLiteral("candidate_failed"));
            return;}
        if(serial!=p.serial||p.candidate||exchangeStateTerminal(p.manual.state))return;
        if(bulk){
            // Asset transfer is intentionally separate from audio and control.
            // It is idle for most of a set, so its transient state must not
            // disconnect a healthy session or prevent DJs gathering in lobby.
            if(state==LinkState::Connected){
                p.bulkDisconnectedAt=0;
                if(exchangeAwaitingConnection(p.manual.state)&&p.transport->aggregateLinkState()==LinkState::Connected){p.manual.connectDeadline=0;p.manual.retries=0;manualSetState(p,ExchangeState::Connected,QStringLiteral("接続しました"));}
            }else if(state==LinkState::Disconnected||state==LinkState::Failed){
                if(!p.bulkDisconnectedAt)p.bulkDisconnectedAt=monotonicNanos();
                if(p.manual.state==ExchangeState::Connecting&&state==LinkState::Failed)manualSetState(p,ExchangeState::NeedsExchange,QStringLiteral("楽曲転送用の接続を開始できませんでした。接続情報を作り直してください"),QStringLiteral("bulk_failed"));
            }
            return;
        }
        if(state==LinkState::Connected){
            if(exchangeAwaitingConnection(p.manual.state)&&p.transport->aggregateLinkState()!=LinkState::Connected)return;
            if(!p.hello)p.lastControlAt=monotonicNanos();p.manual.connectDeadline=0;p.manual.retries=0;
            if(p.manual.state==ExchangeState::Interrupted||exchangeAwaitingConnection(p.manual.state))manualSetState(p,ExchangeState::Connected,QStringLiteral("接続しました"));
        }
        // Disconnected is deliberately debounced by manualTick. The existing
        // PeerConnection keeps retrying and incoming control restores the UI.
        else if(state==LinkState::Failed)manualSetState(p,ExchangeState::NeedsExchange,QStringLiteral("音声・操作の接続を復旧できませんでした。接続情報を作り直してください"),QStringLiteral("control_failed"));
    }
    /// Signed offline notice. Without a channel to the other end, the only
    /// honest option is a transferable packet the user delivers by hand.
    QString manualBuildNotice(Peer& p,const QString& reason,const QString& text) {
        if(!hosting)return QStringLiteral("通知はホストだけが発行できます");
        ExchangePacket packet;packet.kind=ExchangeKind::Notice;
        auto failure=manualPacketBase(p,packet);if(!failure.isEmpty())return failure;
        packet.noticeReason=reason;packet.noticeText=text.left(200);
        packet.expiresAt=QDateTime::currentMSecsSinceEpoch()+7LL*24*3600*1000;
        failure=manualSign(packet);if(!failure.isEmpty())return failure;
        const auto encoded=encodeExchangePacket(packet,&failure);if(encoded.isEmpty())return failure;
        p.manual.noticeText=encoded;return {};
    }
    QJsonObject manualExchangeJson(const Peer& p) const {
        QJsonObject value{{"state",exchangeStateName(p.manual.state)},{"waitingFor",p.manual.waitingFor}};
        if(!p.manual.detail.isEmpty())value["detail"]=p.manual.detail;
        if(!p.manual.errorCode.isEmpty())value["errorCode"]=p.manual.errorCode;
        if(!p.manual.inviteText.isEmpty())value["inviteText"]=p.manual.inviteText;
        if(!p.manual.noticeText.isEmpty())value["noticeText"]=p.manual.noticeText;
        if(!p.manual.inviteId.isEmpty())value["inviteId"]=p.manual.inviteId;
        if(p.manual.expiresAt)value["expiresAt"]=double(p.manual.expiresAt);
        if(p.manual.attempt)value["attempt"]=double(p.manual.attempt);
        const auto* transport=p.candidate?p.candidate.get():p.transport.get();
        if(transport&&p.manual.state==ExchangeState::Connected)
            value["route"]=transport->selectedRelay(false)||transport->selectedRelay(true)
                ?(transport->selectedRelay(false)&&transport->selectedRelay(true)?QStringLiteral("relay"):QStringLiteral("mixed"))
                :QStringLiteral("direct");
        else value["route"]=QStringLiteral("unknown");
        return value;
    }
    void discardAttempt(Peer& p) {
        if(p.candidate){p.candidateSerial=0;p.candidate.reset();}
        else if(p.transport&&p.transport->linkState(false)!=LinkState::Connected){p.serial=0;p.transport.reset();p.hello=false;p.pending.clear();}
        p.manual.collectDeadline=0;p.manual.connectDeadline=0;p.manual.clearArtifacts();
    }
    QString acceptManualInvite(const ExchangePacket& packet,bool initial) {
        if(packet.kind!=ExchangeKind::Invite)return "ホストから届いた招待を取り込んでください";
        if(!initial&&(packet.sessionId!=auth.sessionId||packet.peerId!=auth.local||packet.hostPeerId!=auth.host||packet.hostFingerprint!=pinnedHostFingerprint))return "別のセッション・参加者への招待です";
        QString failure;
        if(!verifyExchangeSignature(packet,initial?packet.hostFingerprint:pinnedHostFingerprint,&failure))return failure;
        if(initial){
            auth.sessionId=packet.sessionId;auth.local=packet.peerId;auth.host=packet.hostPeerId;auth.owner=auth.host;
            name=packet.sessionName;pinnedHostFingerprint=packet.hostFingerprint;audible.store(false);connection="pending";
            auto peer=std::make_unique<Peer>();peer->id=auth.host;peer->name=packet.hostName;peer->fp=packet.hostFingerprint;peer->approved=true;peers[auth.host]=std::move(peer);
        }
        auto& peer=*peers.at(auth.host);
        if(!initial&&packet.generation<=peer.manual.generation)return "取り込み済み、または古い招待です。ホストから新しい招待を受け取ってください";
        peer.manual.inviteId=packet.inviteId;peer.manual.generation=packet.generation;peer.manual.attempt=packet.attempt;peer.manual.expiresAt=packet.expiresAt;
        // The host supplies only bounded, expiring TURN credentials. Local STUN
        // may supplement them but a guest's saved TURN is not redistributed.
        QJsonArray servers=packet.iceServers;
        const auto local=network.credentials(auth.sessionId,auth.local,QDateTime::currentMSecsSinceEpoch());
        for(const auto& item:local)servers.append(item);
        failure=manualStartAttempt(peer,false,servers);if(!failure.isEmpty())return failure;
        auto* transport=attemptSlot(peer);
        for(int i=0;i<2;++i)if(!transport->remoteDescription(i==1,packet.description[i].sdp,packet.description[i].type,pinnedHostFingerprint,&failure)){discardAttempt(peer);manualSetState(peer,ExchangeState::Failed,failure);return failure;}
        return {};
    }
    QJsonObject testNetwork(bool start) {
        if(networkProbe){
            if(networkProbe->readyForManualExport()){
                const bool relay=networkProbe->aggregatedDescription(false).contains(" typ relay")&&networkProbe->aggregatedDescription(true).contains(" typ relay");
                networkTestResult={{"state",relay?"success":"failure"},{"detail",relay?"中継用の接続先を取得できました。相手との接続成立は招待・返答の交換後に確認します":"中継用の接続先を取得できませんでした。URL・資格情報と回線を確認してください"}};
                networkProbe.reset();networkTestDeadline=0;
            }else if(monotonicNanos()>=networkTestDeadline){networkProbe.reset();networkTestDeadline=0;networkTestResult={{"state","timeout"},{"detail","30秒以内に中継用の接続先を取得できませんでした"}};}
            return networkTestResult;
        }
        if(!start)return networkTestResult;
        QString failure;auto servers=network.credentials(secureRandomHex(16),secureRandomHex(16),QDateTime::currentMSecsSinceEpoch(),&failure);
        bool hasTurn=false;for(const auto& v:servers)hasTurn|=v.toObject().contains("credential");
        if(!hasTurn)return {{"state","failure"},{"detail",failure.isEmpty()?QStringLiteral("中継設定を適用してからテストしてください"):failure}};
        networkProbe=std::make_unique<MediaTransport>(secureRandomHex(16),iceServerUrls(servers),MediaTransport::Callbacks{},nullptr,true);
        if(!networkProbe->start(true,&failure)){networkProbe.reset();return {{"state","failure"},{"detail",failure}};}
        networkTestDeadline=monotonicNanos()+30000000000LL;
        networkTestResult={{"state","checking"},{"detail","中継用の接続先を収集しています"}};return networkTestResult;
    }
    /// Deadlines and expiry for every manual attempt. Runs from the session
    /// tick so one peer's timeout never touches another's.
    void manualTick() {
        if(!manual)return;
        const auto nowNanos=monotonicNanos(),nowMs=QDateTime::currentMSecsSinceEpoch();
        for(auto& [id,p]:peers){
            auto& attempt=p->manual;
            if(p->retiring){if(hosting)route(p->retiring->decodedRing(),id);if(nowNanos>=p->retireAt)p->retiring.reset();}
            if(attempt.state==ExchangeState::Connected&&p->lastControlAt&&nowNanos-p->lastControlAt>kControlSilenceNanos){manualSetState(*p,ExchangeState::Interrupted,"通信が一時的に途切れています。同じ接続への自動再接続を続けています",QStringLiteral("control_silent"));attempt.connectDeadline=0;}
            if(p->candidate)manualPromoteIfReady(*p);
            if(attempt.collectDeadline&&nowNanos>=attempt.collectDeadline){
                discardAttempt(*p);
                manualSetState(*p,ExchangeState::Failed,QStringLiteral("接続情報を作成できませんでした。ネットワーク設定を確認して、もう一度お試しください"),QStringLiteral("gathering_timeout"));
            }
            if(attempt.state==ExchangeState::Connecting&&attempt.connectDeadline&&nowNanos>=attempt.connectDeadline){
                attempt.connectDeadline=0;
                if(attempt.state==ExchangeState::Connecting){discardAttempt(*p);
                    manualSetState(*p,ExchangeState::NeedsExchange,QStringLiteral("時間内に接続できませんでした。接続情報を作り直してください"),QStringLiteral("connect_timeout"));}
            }
            const bool waiting=attempt.state==ExchangeState::InviteReady||attempt.state==ExchangeState::ResponseReady
                ||attempt.state==ExchangeState::AwaitingHost||attempt.state==ExchangeState::ApprovalPending;
            // Interrupted keeps the authenticated transport alive. WebRTC can
            // recover it without another invite; only a terminal Failed state
            // or an explicit replacement exchange discards the connection.
            if(waiting&&attempt.expiresAt&&nowMs>=attempt.expiresAt){
                discardAttempt(*p);
                manualSetState(*p,ExchangeState::Expired,QStringLiteral("接続情報の期限が切れました。作り直してください"),QStringLiteral("expired"));
            }
        }
    }
    QJsonObject wireState(bool includeAvatars=false) const {
        auto s=publicState(true);s.remove("invite");s.remove("privatePreview");s.remove("program");s.remove("junctionTracks");
        s["timelineOriginNanos"]=QString::number(timeline.originNanos());s["timelineOriginFrame"]=u64(timeline.originFrame());s["programDelayFrames"]=int(delay);s["engineFingerprint"]=fingerprint();if(auth.committed)s["commit"]=auth.committed->toJson();
        // The 100 ms heartbeat must stay lightweight. Full profile snapshots
        // are sent only on connection/profile/roster changes, and even those
        // have a bounded aggregate avatar budget inside the 64 KiB channel.
        auto participants=s["participants"].toArray();
        for(int index=0;index<participants.size();++index){auto row=participants[index].toObject();if(!includeAvatars)row.remove("avatarDataUrl");participants[index]=row;}
        s["participants"]=participants;
        while(json(s).size()>60*1024){bool removed=false;for(int index=participants.size()-1;index>=0;--index){auto row=participants[index].toObject();if(row.contains("avatarDataUrl")){row.remove("avatarDataUrl");participants[index]=row;s["participants"]=participants;removed=true;break;}}if(!removed)break;}
        return s;
    }
    void sendManifest(Peer& p) {
        if(p.producing)return;
        p.producing=true;
        StreamManifest m{secureRandomHex(12),auth.local,captureEpoch.load(),captureGeneration.load(),0,1,0};
        p.transport->enableAutomaticManifest(true);p.transport->setSendManifest(m);p.transport->startProducer(&tap.networkRing());tap.enable(true,true);captureEnabled.store(true);
    }
    void control(const QString& id,const QByteArray& bytes) {
        auto i=peers.find(id);if(i==peers.end())return;auto& p=*i->second;
        Envelope envelope;auto decoded=decodeEnvelopeBytes(bytes,&envelope,id);if(!decoded.ok())return;
        if(!hosting && auth.sessionId=="pending" && id==auth.host && envelope.type==MessageType::SessionSnapshot)auth.sessionId=envelope.sessionId;
        if(envelope.sessionId!=auth.sessionId)return;
        p.lastControlAt=monotonicNanos();
        if(manual&&p.manual.state==ExchangeState::Interrupted&&p.transport&&p.transport->linkState(false)==LinkState::Connected){p.manual.connectDeadline=0;manualSetState(p,ExchangeState::Connected,"通信が復旧しました");}
        const auto payload=envelope.payload;
        if(envelope.type==MessageType::SessionSnapshot && payload["engineFingerprint"]!=fingerprint()){fail("Junctionの基礎プロトコルに互換性がありません");return;}
        if(!p.hello && envelope.type!=MessageType::PeerHello && envelope.type!=MessageType::SessionSnapshot)return;
        if(envelope.type==MessageType::PeerHello){if(payload["fingerprint"]!=fingerprint()){p.hello=false;if(manual){discardAttempt(p);manualSetState(p,ExchangeState::Failed,"Junctionの基礎プロトコルに互換性がありません","version_mismatch");}else fail("Junctionの基礎プロトコルに互換性がありません");return;}const bool firstHello=!p.hello;p.hello=true;const auto capabilities=payload["junctionCapabilities"].toArray();if(capabilities.size()<=32&&capabilities.contains(kJunctionTracksCapability))p.junctionTracksV1=true;if(firstHello)queue(p,"peer.hello",localProfile());if(payload.contains("djName")||payload.contains("displayName"))p.name=sanitizeDisplayName(payload["djName"].toString(payload["displayName"].toString(p.name)));if(payload.contains("avatarDataUrl"))p.avatarDataUrl=profileAvatar(payload["avatarDataUrl"].toString());if(payload.contains("themeColor"))p.themeColor=profileColor(payload["themeColor"].toString(),id);if(!manual||p.transport->aggregateLinkState()==LinkState::Connected)connection="connected";
            if(payload["stream"].isObject()){auto m=readStream(payload["stream"].toObject());if(m && m->producerPeerId==id && (id==auth.owner || id==auth.next) && (m->epoch==auth.epoch || (auth.committed&&m->epoch==auth.committed->newEpoch))){p.transport->setReceiveManifest(*m);p.remoteStreamReady=true;queue(p,"peer.hello",{{"fingerprint",fingerprint()},{"streamAck",m->streamId}});}}
            else if(payload["streamAck"].isString())p.transport->acknowledgeSendManifest(payload["streamAck"].toString());
            else if(!hosting && (auth.owner==auth.local || auth.next==auth.local))sendManifest(p);
            if(hosting&&(firstHello||payload.contains("djName")||payload.contains("displayName")||payload.contains("avatarDataUrl")||payload.contains("themeColor")))broadcast("session.snapshot",wireState(true));
        } else if(envelope.type==MessageType::SessionSnapshot && id==auth.host && !hosting) {
            const auto epoch=parseU64(payload["epoch"]);if(!epoch||*epoch<auth.epoch)return;
            const auto incomingLifecycle=payload["lifecycle"].toString("live");if(incomingLifecycle=="lobby"||incomingLifecycle=="starting"||incomingLifecycle=="live")lifecycle=incomingLifecycle;
            if(payload["commit"].isObject()&&!auth.committed){
                auto commit=HandoffCommitMessage::fromJson(payload["commit"].toObject());
                if(commit&&commit->sessionId==auth.sessionId&&commit->newEpoch>auth.epoch){
                    auth.epoch=commit->oldEpoch;auth.owner=commit->oldOwner;auth.next=commit->newOwner;auth.phase="fenced";auth.handoffId=commit->handoffId;auth.fenceFrame=commit->fencedAtMediaFrame;auth.throughSeq=commit->lastAppliedSeq;
                    if(auth.commit(*commit,id).isEmpty()){scheduleCaptureCommit(*commit);auth.advance(now());}
                }
            }
            if(manual&&auth.phase=="recovery"&&payload["handoffState"]=="playing"){problem.clear();reasons.clear();}
            if(!auth.committed){auth.epoch=*epoch;const auto publicOwner=payload["performerPeerId"].toString();auth.owner=publicOwner.isEmpty()&&lifecycle!="live"?auth.host:publicOwner;auth.next=payload["nextPeerId"].toString();auth.phase=payload["handoffState"].toString();auth.handoffId=payload["handoffId"].toString();}
            name=payload["sessionName"].toString();const auto incomingRoster=payload["participants"].toArray();if(incomingRoster.size()<=64){QJsonArray merged;for(const auto& value:incomingRoster){auto row=value.toObject();if(!row.contains("avatarDataUrl")){const auto previous=rosterMetadata(row["peerId"].toString());if(previous.contains("avatarDataUrl"))row["avatarDataUrl"]=previous["avatarDataUrl"];}merged.append(row);}participantRoster=merged;}
            bool valid=false;const auto t0=payload["timelineOriginNanos"].toString().toLongLong(&valid);auto f0=parseU64(payload["timelineOriginFrame"]);if(valid&&f0)timeline.adopt(t0,*f0);
            // Only a peer that is neither performing nor incoming stops sending:
            // the incoming DJ streams during bootstrap and handoff validation.
            if(!p.hello)queue(p,"peer.hello",localProfile());if(!captureEnabled.load())captureEpoch.store(auth.epoch);if(auth.owner==auth.local&&!p.producing)sendManifest(p);else if(auth.owner!=auth.local&&auth.next!=auth.local&&p.producing){p.transport->startProducer(nullptr);p.producing=false;if(!hosting){captureEnabled.store(false);tap.enable(false,false);}}audible.store(auth.owner==auth.local);++auth.revision;
        } else if(envelope.type==MessageType::ClockProbeRequest) {queue(p,"clock.reply",{{"t1",payload["t1"]},{"t2",QString::number(monotonicNanos())},{"t3",QString::number(monotonicNanos())}});}
        else if(envelope.type==MessageType::ClockProbeReply && id==auth.host) {bool a,b,c;auto t1=payload["t1"].toString().toLongLong(&a),t2=payload["t2"].toString().toLongLong(&b),t3=payload["t3"].toString().toLongLong(&c);const auto t4=monotonicNanos();const ClockProbe probe{t1,t2,t3,t4};if(a&&b&&c&&clock.add(probe)){timelineAnchor.store(clock.toLocalNanos(timeline.originNanos()));const double sampleRttMs=double((t4-t1)-(t3-t2))/1000000.0;const auto stats=p.transport?p.transport->statistics():MediaTransport::Statistics{};quint64 received=stats.receivedPackets,nacks=stats.nacksSent;if(p.healthStatsReady&&stats.receivedPackets>=p.healthStats.receivedPackets&&stats.nacksSent>=p.healthStats.nacksSent){received-=p.healthStats.receivedPackets;nacks-=p.healthStats.nacksSent;}const auto total=received+nacks;HealthReportMessage report;if(p.probeReady){p.smoothedJitterMs=.75*p.smoothedJitterMs+.25*std::abs(sampleRttMs-p.lastProbeRttMs);p.smoothedRttMs=.65*p.smoothedRttMs+.35*sampleRttMs;}else{p.smoothedRttMs=sampleRttMs;p.smoothedJitterMs=0;p.probeReady=true;}p.lastProbeRttMs=sampleRttMs;p.healthStats=stats;p.healthStatsReady=true;report.rttMs=std::clamp(p.smoothedRttMs,0.0,600000.0);report.jitterMs=std::clamp(p.smoothedJitterMs,0.0,600000.0);report.lossFraction=total?std::clamp(double(nacks)/double(total),0.0,1.0):0;report.queueFrames=0;report.audioClockErrorMs=0;report.acceptable=report.rttMs<=250.0&&report.lossFraction<=.05;p.health=report;p.hasHealth=true;p.healthAt=t4;queue(p,"health.report",report.toJson());}}
        else if(envelope.type==MessageType::HealthReport && hosting&&p.approved){QString reason;const auto report=HealthReportMessage::fromJson(payload,&reason);if(report){p.health=*report;p.hasHealth=true;p.healthAt=monotonicNanos();++auth.revision;}}
        else if(envelope.type==MessageType::HandoffRequest && hosting && p.approved&&lifecycle=="live"){turnRequests.insert(id);++auth.revision;broadcast("session.snapshot",wireState(false));}
        else if(envelope.type==MessageType::HandoffPrepare && id==auth.host) {auth.next=payload["targetPeerId"].toString();auth.handoffId=payload["handoffId"].toString();auth.phase="preparing";bootstrapStart=payload["bootstrap"].toBool();resetPreparation();if(auth.owner==auth.local&&!bootstrapStart)startExport();if(auth.next==auth.local){captureEpoch.store(auth.epoch);if(!bootstrapStart)sendManifest(p);}}
        else if(envelope.type==MessageType::GraphApplied && (id==auth.owner||id==auth.host) && auth.phase=="preparing"){
            ready=false;reasons={"演奏の変更に同期しています"};if(auth.next==auth.local)prepared=false;
            if(hosting)broadcast("graph.applied",payload);
        }
        else if(envelope.type==MessageType::GraphManifest && (id==auth.owner||id==auth.host) && payload["handoffId"]==auth.handoffId){
            const auto hash=payload["assetId"].toString();const auto size=parseU64(payload["sizeBytes"]);
            if(!validHexDigest(hash,32)||!size||*size>8*1024*1024)return;
            pendingGraphAsset=hash;assetKinds[hash]="junction-graph-v1";preserveWarmGraph=prepared&&aligned&&auth.phase=="fenced"&&payload["throughSeq"]==preparedGraph["throughSeq"];prepared=false;ready=false;if(!preserveWarmGraph)aligned=false;
            if(hosting&&auth.next!=auth.local){auto next=peers.find(auth.next);if(next!=peers.end())queue(*next->second,"graph.manifest",payload);}
            const auto cached=cache.resolve(hash);if(!cached.isEmpty()){assets[hash]=cached;assetReceived(hash);}
            else if(!requestedAssets.contains(hash)){requestedAssets.insert(hash);queue(p,"asset.request",{{"assetId",hash}});}
        }
        else if(envelope.type==MessageType::GraphCheckpoint && (id==auth.owner || id==auth.host)) {preparedGraph=payload["graph"].toObject();prepared=false;aligned=false;ready=false;if(hosting && auth.next!=auth.local){auto n=peers.find(auth.next);if(n!=peers.end())queue(*n->second,"graph.checkpoint",payload);}if(auth.next==auth.local || hosting)requestAssets(p);}
        else if(envelope.type==MessageType::AssetRequest) {auto hash=payload["assetId"].toString();if(!validHexDigest(hash,32))return;auto asset=assets.find(hash);if(asset==assets.end()){
                if(hosting && id!=auth.owner && (hash==pendingGraphAsset||references(graph).contains(hash)||references(preparedGraph).contains(hash)) && assetWaiters.size()<128){
                    assetWaiters[hash].insert(id);auto owner=peers.find(auth.owner);
                    if(owner!=peers.end()&&!requestedAssets.contains(hash)){requestedAssets.insert(hash);queue(*owner->second,"asset.request",{{"assetId",hash}});}
                }return;
            }if(payload["receiverReady"].toBool()){const bool background=payload["background"].toBool();bool queued=false;for(auto& job:outgoing)if(job.peer==id&&job.hash==hash){queued=true;if(!background)job.background=false;break;}if(!queued&&outgoing.size()<256)outgoing.enqueue({id,hash,asset->second,0,background});}else queue(p,"asset.manifest",{{"assetId",hash},{"sizeBytes",u64(quint64(QFileInfo(asset->second).size()))}});}
        else if(envelope.type==MessageType::AssetManifest && (id==auth.owner||id==auth.host||trackSender(id,payload["assetId"].toString()))){const auto hash=payload["assetId"].toString();const auto size=parseU64(payload["sizeBytes"]);QString error;if(size&&requestedAssets.contains(hash)){if(trackOnly(hash)&&*size>kMaxAnnouncedAssetBytes)trackFailed(hash,QStringLiteral("音源が大きすぎます"),false);else if(!cache.begin(hash,*size,&error)){if(trackOnly(hash))trackFailed(hash,error,false);else fail(error);}else{QJsonObject request{{"assetId",hash},{"receiverReady",true}};if(trackOnly(hash)){request["background"]=true;trackTransferAt=monotonicNanos();}queue(p,"asset.request",request);}}}
        else if(envelope.type==MessageType::AssetComplete && (id==auth.owner||id==auth.host||trackSender(id,payload["assetId"].toString()))){finishAsset(payload["assetId"].toString());}
        else if(envelope.type==MessageType::TrackAnnounce && p.junctionTracksV1 && trackSource(id)){
            QString stream;quint64 revision=0;const auto rows=parseTrackAnnouncement(payload,&stream,&revision);if(!rows)return;
            auto& seen=announceSeen[id];if(seen.first==stream&&revision<=seen.second)return;seen={stream,revision};
            const auto before=sharedTracks.assetIds();sharedTracks.apply(id,p.name,*rows);const auto after=sharedTracks.assetIds();
            for(const auto& hash:before-after){
                if(!neededByHandoff(hash)){
                    if(trackDigest&&trackDigest->hash==hash)trackDigest.reset();
                    if(trackTransfer==hash){trackTransfer.clear();trackTransferSource.clear();}
                    requestedAssets.remove(hash);droppedTransfers.insert(hash);cache.discard(hash);
                }
                releaseTrackPin(hash);
            }
            ++auth.revision;pumpSharedTracks();
        }
        else if(envelope.type==MessageType::HandoffCancel && id==auth.host && payload["handoffId"].toString()==auth.handoffId){if(auth.cancel().isEmpty()){if(lifecycle=="starting")restoreLobby();prepared=false;ready=false;audible.store(auth.owner==auth.local);}}
        else if(envelope.type==MessageType::HandoffReady && hosting && id==auth.next && payload["handoffId"]==auth.handoffId){
            if(payload["requestFence"].toBool()){QString failure;q->command("handoff.accept",{},&failure);if(!failure.isEmpty())fail(failure);return;}
            ready=payload["ready"].toBool();reasons.clear();if(!ready)reasons.append(payload["reason"].toString());
            if(ready&&bootstrapStart&&payload["bootstrap"].toBool()&&lifecycle=="starting"&&auth.phase=="preparing"){if(!p.remoteStreamReady){ready=false;reasons={"最初のDJの音声を確認しています"};return;}finishBootstrap();return;}
            if(ready&&lifecycle=="starting"&&auth.phase=="preparing"){QString failure;q->command("handoff.accept",{},&failure);if(!failure.isEmpty()){restoreLobby();fail(failure);}return;}
            if(ready&&auth.phase=="fenced"&&payload["stage"]=="fenced"&&parseU64(payload["throughSeq"])==std::optional<quint64>(auth.throughSeq)&&!validationStart){beginValidation(now()+24000);requestValidationCapture(validationStart);}
        }
        else if(envelope.type==MessageType::HandoffFence && id==auth.host && payload["handoffId"]==auth.handoffId){auto frame=parseU64(payload["frame"]),through=parseU64(payload["throughSeq"]);if(frame&&auth.phase=="preparing"){const auto watermark=bootstrapStart&&payload["bootstrap"].toBool()&&through?*through:controlSeq;auth.fence(*frame,watermark);finalCheckpoint=false;fenceDeadline=monotonicNanos()+6000000000LL;ready=false;}}
        else if(envelope.type==MessageType::HandoffFenced && (id==auth.owner || id==auth.host) && payload["handoffId"]==auth.handoffId){auto frame=parseU64(payload["fencedAtMediaFrame"]),seq=parseU64(payload["lastAppliedSeq"]);if(frame&&seq&&auth.phase=="fenced"){auth.fenceFrame=*frame;auth.throughSeq=*seq;if(hosting)broadcast("handoff.fenced",payload);}}
        else if(envelope.type==MessageType::ValidationWindow){
            if(payload["stage"]=="received" && id==auth.host && payload["payloadSha256"]==validationAwaitingAck){validationAwaitingAck.clear();}
            else if(payload["stage"]=="align"&&id==auth.host&&auth.next==auth.local&&payload["handoffId"]==auth.handoffId){
                auto start=parseU64(payload["startMediaFrame"]);const int lag=payload["lagFrames"].toInt(1000);if(start&&std::abs(lag)<=256&&auth.phase=="fenced")alignValidation(lag,*start);
            }
            else if(payload["stage"]=="capture" && id==auth.host && payload["handoffId"]==auth.handoffId){auto start=parseU64(payload["startMediaFrame"]);if(start&&*start>now()&&*start-now()<=480000)beginValidation(*start);}
            else if(hosting && (id==auth.owner||id==auth.next) && payload["handoffId"]==auth.handoffId && payload["sampleRateHz"]==48000 && payload["channels"]==2 && parseU64(payload["startMediaFrame"])==std::optional<quint64>(validationStart)){
                int count=payload["frameCount"].toInt();auto hash=payload["payloadSha256"].toString();if(count==12000&&validHexDigest(hash,32)&&validationReceiving.size()<4){validationReceiving.emplace(hash,ValidationReceive{id,QByteArray(count*8,0),std::vector<bool>(size_t((count*8+32767)/32768),false)});queue(p,"validation.window",{{"stage","received"},{"payloadSha256",hash},{"handoffId",auth.handoffId}});}
            }
        }
        else if(envelope.type==MessageType::HandoffCommit && id==auth.host){auto commit=HandoffCommitMessage::fromJson(payload);if(commit){auto failure=applyCommit(*commit,id);if(!failure.isEmpty())fail(failure);else scheduleCaptureCommit(*commit);}}

        else if(envelope.type==MessageType::SessionRecovery&&id==auth.host){
            if(manual&&payload["stage"]=="active"&&auth.owner==auth.local&&!auth.committed){q->setCaptureAnchor(lastCaptureSourceEnd.load(),now());captureEnabled.store(true);sendManifest(p);}
            if(payload["stage"]=="scheduled"){auto frame=parseU64(payload["frame"]),epoch=parseU64(payload["epoch"]);if(frame&&epoch&&*epoch>auth.epoch){recoveryResumeFrame=*frame;recoveryEpoch=*epoch;}}
            auth.phase="recovery";fail(payload["reason"].toString("配信を復旧中です"));
        }
        else if(envelope.type==MessageType::SessionEnd){
            if(hosting&&endingAt&&payload["ack"].toBool())p.endingAck=true;
            else if(id==auth.host&&!hosting){queue(p,"session.end",{{"ack",true}});connection="closing";endingAt=monotonicNanos()+200000000LL;}
        }
        else if(envelope.type==MessageType::PeerLeave){if(id==auth.owner)beginRecovery("プレイ担当者が退出しました");peers.erase(id);rosterOrder.removeAll(id);finishedOrder.removeAll(id);turnRequests.remove(id);}
    }
    bool validateAsset(const QString& hash,const QString& path) const {
        auto kind=assetKinds.find(hash);
        if(kind!=assetKinds.end()&&kind->second=="junction-graph-v1"){
            QFile file(path);if(file.size()>8*1024*1024||!file.open(QIODevice::ReadOnly))return false;
            const auto object=QJsonDocument::fromJson(file.readAll()).object();return object["schema"]==1&&object["decks"].toArray().size()==4;
        }
        if(kind!=assetKinds.end()&&(kind->second=="plumdeck-ddj-dsp-v1"||kind->second=="plumdeck-keylock-v1"||kind->second=="plumdeck-fx-v1"))return backend->validateDspAsset(path);
        // Use Mixxx's real decoder registry first (AAC/M4A included on macOS),
        // then retain libsndfile as a conservative fallback for test backends.
        if(backend->validateAudioAsset(path))return true;
        SF_INFO info{};auto* f=openSoundFile(path,SFM_READ,&info);if(f)sf_close(f);return f!=nullptr;
    }
    void sendGraph(Peer& peer,const QJsonObject& value) {
        const auto bytes=json(value);if(bytes.size()>8*1024*1024){fail("演奏状態が転送上限を超えています");return;}
        const auto hash=sha256Hex(bytes),path=identityDir.path()+"/"+hash+".graph";
        if(assets.count(hash)==0){QSaveFile file(path);if(!file.open(QIODevice::WriteOnly)||file.write(bytes)!=bytes.size()||!file.commit()){fail("演奏状態を準備できません");return;}assets[hash]=path;}
        assetKinds[hash]="junction-graph-v1";
        queue(peer,"graph.manifest",{{"assetId",hash},{"sizeBytes",u64(quint64(bytes.size()))},{"throughSeq",value["throughSeq"]},{"handoffId",auth.handoffId}});
    }
    static QSet<QString> references(const QJsonValue& value){
        QSet<QString> result;
        std::function<void(const QJsonValue&)> visit=[&](const QJsonValue& v){
            if(v.isArray()){for(const auto& x:v.toArray())visit(x);return;}
            if(!v.isObject())return;const auto object=v.toObject();const auto hash=object["assetId"].toString();if(validHexDigest(hash,32))result.insert(hash);
            for(auto it=object.begin();it!=object.end();++it)visit(it.value());
        };visit(value);return result;
    }
    // Only our immutable transfer capsules are removed. User audio and the
    // graph currently loaded in the engine retain their separate lifetimes.
    void pruneCapsules(){
        if(exportJob.valid())return;
        auto keep=references(graph)|references(preparedGraph)|requestedAssets|pinnedAssets;
        keep.insert(pendingGraphAsset);for(const auto& job:outgoing)keep.insert(job.hash);
        for(const auto& [hash,waiters]:assetWaiters)keep.insert(hash);
        const auto cutoff=QDateTime::currentDateTimeUtc().addSecs(-120);
        const QDir directory(identityDir.path());
        for(const auto& file:directory.entryInfoList({"*.dsp","*.graph"},QDir::Files)){
            const auto hash=file.completeBaseName();
            if(!validHexDigest(hash,32)||file.isSymLink()||file.lastModified()>cutoff||keep.contains(hash))continue;
            if(QFile::remove(file.absoluteFilePath())){assets.erase(hash);assetKinds.erase(hash);}
        }
        // Eviction may have removed unpinned cache entries. Never advertise a
        // stale file path as an available source in a later handoff.
        for(auto it=assets.begin();it!=assets.end();)if(!keep.contains(it->first)&&!QFileInfo::exists(it->second)){assetKinds.erase(it->first);it=assets.erase(it);}else ++it;
    }
    void resetPreparation(){
        prepared=false;aligned=false;ready=false;finalCheckpoint=false;preserveWarmGraph=false;graphDirty=false;
        graph={};preparedGraph={};pendingGraphAsset.clear();validationRound=0;validationStart=0;validationCapture.cancel();validationReceiving.clear();validationOutgoing.clear();validationPcm.clear();fenceDeadline=0;
        reasons={"音源と演奏状態を準備しています"};
    }
    void startExport() {
        if(exportJob.valid())return;
        pruneCapsules();
        quint64 temporaryBytes=0;for(const auto& file:QDir(identityDir.path()).entryInfoList({"*.dsp","*.graph"},QDir::Files))temporaryBytes+=quint64(file.size());
        if(temporaryBytes>512ULL*1024*1024){fail("一時的な転送データが上限に達しました。準備を取り消し、しばらくしてから再試行してください");return;}
        auto local=backend->junctionGraph();if(local.isEmpty())return;bool renderValid=false;const auto render=local["renderFrame"].toString().toULongLong(&renderValid);local["atMediaFrame"]=u64(renderValid?q->mediaFrameForSource(render):now());if(!local.contains("throughSeq"))local["throughSeq"]=u64(controlSeq);exportRevision=auth.revision;exportHandoff=auth.handoffId;
        const auto transferDirectory=identityDir.path();
        exportJob=std::async(std::launch::async,[local,transferDirectory]()mutable{
            std::map<QString,QString> paths;
            std::function<QJsonValue(QJsonValue)> visit=[&](QJsonValue v)->QJsonValue{
                if(v.isArray()){QJsonArray a;for(const auto& x:v.toArray())a.append(visit(x));return a;}
                if(!v.isObject())return v;
                auto o=v.toObject();
                if(o.contains("path")){auto path=o.take("path").toString();if(!path.isEmpty()){auto hash=AssetCache::hashFile(path);if(!hash.isEmpty()){
                    // The backend reuses its checkpoint slot. Keep an immutable,
                    // content-addressed copy for outstanding peer transfers.
                    if(o["format"]=="plumdeck-ddj-dsp-v1"||o["format"]=="plumdeck-keylock-v1"||o["format"]=="plumdeck-fx-v1"){
                        const auto stable=transferDirectory+"/"+hash+".dsp";
                        if(!QFileInfo::exists(stable)&&!QFile::copy(path,stable)){o["assetError"]="DSP状態を保存できません";return o;}
                        path=stable;
                    }
                    paths[hash]=path;o["assetId"]=hash;
                }else o["assetError"]="音源を読み込めません";}o.remove("trackId");o.remove("localTrackId");}
                for(auto it=o.begin();it!=o.end();++it)it.value()=visit(it.value());
                return o;
            };
            return std::make_pair(visit(local).toObject(),paths);
        });
    }
    void requestAssets(Peer& source) {
        std::function<void(QJsonValue)> walk=[&](QJsonValue v){if(v.isArray()){for(const auto& x:v.toArray())walk(x);return;}if(!v.isObject())return;auto o=v.toObject();auto hash=o["assetId"].toString();if(o["format"]=="plumdeck-ddj-dsp-v1"||o["format"]=="plumdeck-keylock-v1"||o["format"]=="plumdeck-fx-v1")assetKinds[hash]=o["format"].toString();if(!hash.isEmpty() && assets.find(hash)==assets.end()){auto path=cache.resolve(hash);if(path.isEmpty()){if(!requestedAssets.contains(hash)){requestedAssets.insert(hash);queue(source,"asset.request",{{"assetId",hash}});}else if(hash==trackTransfer&&source.id==trackTransferSource&&!cache.receivedBitmap(hash).isEmpty())queue(source,"asset.request",{{"assetId",hash},{"receiverReady",true}});}else assets[hash]=path;}for(auto it=o.begin();it!=o.end();++it)walk(it.value());};walk(preparedGraph);tryPrepare();
    }
    void tryPrepare() {
        if(preparedGraph.isEmpty() || prepared || auth.next!=auth.local || (bootstrapStart&&lifecycle=="starting"))return;
        bool missing=false;std::function<QJsonValue(QJsonValue)> visit=[&](QJsonValue v)->QJsonValue{if(v.isArray()){QJsonArray a;for(const auto& x:v.toArray())a.append(visit(x));return a;}if(!v.isObject())return v;auto o=v.toObject();o.remove("path");auto hash=o["assetId"].toString();if(!hash.isEmpty()){auto it=assets.find(hash);if(it==assets.end())missing=true;else{o["path"]=it->second;}}for(auto it=o.begin();it!=o.end();++it)if(it.key()!="path")it.value()=visit(it.value());return o;};
        auto local=visit(preparedGraph).toObject();if(missing){reasons={"音源を受信しています"};return;}
        const auto nextPins=references(preparedGraph);for(const auto& hash:nextPins)cache.pin(hash,true);
        const auto error=backend->restoreJunctionGraph(local);if(!error.isEmpty()){for(const auto& hash:nextPins-pinnedAssets)if(!trackPins.contains(hash))cache.pin(hash,false);fail(error);return;}
        for(const auto& hash:pinnedAssets-nextPins)if(!trackPins.contains(hash))cache.pin(hash,false);pinnedAssets=nextPins;
        prepared=true;aligned=false;reasons={"音声の位置と状態を確認しています"};
    }
    void bulk(const QString& id,const QByteArray& bytes) {
        if(bytes.size()<72 || bytes.size()>72+32768)return;
        auto hash=QString::fromLatin1(bytes.constData(),64);
        if(id!=auth.owner && id!=auth.host && !trackSender(id,hash))return;
        // A dropped Junction Live asset may still be draining from its sender.
        if(droppedTransfers.contains(hash)&&!requestedAssets.contains(hash))return;
        quint64 offset=0;for(int n=64;n<72;++n)offset=(offset<<8)|quint8(bytes[n]);QString error;
        if(!cache.put(hash,offset,bytes.mid(72),&error)){if(trackOnly(hash))trackFailed(hash,error,false);else fail(error);return;}
        if(hash==trackTransfer)trackTransferAt=monotonicNanos();
        // Completion is tested after each chunk: bulk and reliable control are
        // separate connections, so their arrival order is deliberately free.
        finishAsset(hash);
    }
    void finishAsset(const QString hash){
        const auto bitmap=cache.receivedBitmap(hash);if(bitmap.isEmpty()||bitmap.contains(char(0)))return;
        // A Junction Live asset is verified in tick slices. Anything a handoff
        // also waits for keeps the original synchronous publication.
        if(trackOnly(hash)){startTrackDigest(hash,true);return;}
        QString failure;if(cache.finalize(hash,[this,hash](const QString& path){return validateAsset(hash,path);},&failure))assetReceived(hash);else{if(sharedTracks.find(hash))trackFailed(hash,QStringLiteral("音源の検証に失敗しました"),false);fail(failure);}
    }
    void assetReceived(const QString hash,const QString& verifiedPath={}) {
        assets[hash]=verifiedPath.isEmpty()?cache.resolve(hash):verifiedPath;requestedAssets.remove(hash);
        trackReady(hash,assets[hash]);
        auto waiters=assetWaiters.find(hash);if(waiters!=assetWaiters.end()){
            for(const auto& id:waiters->second){auto p=peers.find(id);if(p!=peers.end())queue(*p->second,"asset.manifest",{{"assetId",hash},{"sizeBytes",u64(quint64(QFileInfo(assets[hash]).size()))}});}
            assetWaiters.erase(waiters);
        }
        if(hash==pendingGraphAsset){
            QFile file(assets[hash]);if(!file.open(QIODevice::ReadOnly)||file.size()>8*1024*1024){fail("演奏状態を読み込めません");return;}
            preparedGraph=QJsonDocument::fromJson(file.readAll()).object();pendingGraphAsset.clear();
            if(preserveWarmGraph){prepared=true;preserveWarmGraph=false;return;}
            auto source=peers.find(hosting?auth.owner:auth.host);if(source!=peers.end())requestAssets(*source->second);
        }
        tryPrepare();
    }
    void pumpAssets() {
        if(outgoing.isEmpty())return;
        // Handoff transfers go first. Background Junction Live copies are paced.
        qsizetype index=0;while(index<outgoing.size()&&outgoing[index].background)++index;
        if(index==outgoing.size()){if(ticks<backgroundPumpTick)return;index=0;backgroundPumpTick=ticks+kBackgroundChunkTicks;}
        auto& job=outgoing[index];auto peer=peers.find(job.peer);if(peer==peers.end()||!peer->second->transport){outgoing.removeAt(index);return;}
        // A failed bulk connection does not tear down audio/control while DJs
        // are gathering. Only when an actual asset transfer needs it do we ask
        // for a replacement exchange, keeping the queued transfer resumable.
        if(manual&&peer->second->transport->linkState(true)==LinkState::Failed){if(peer->second->manual.state!=ExchangeState::NeedsExchange)manualSetState(*peer->second,ExchangeState::NeedsExchange,"楽曲転送用の接続を復旧できませんでした。再接続の招待を作ってください","bulk_failed");return;}
        QFile f(job.path);if(!f.open(QIODevice::ReadOnly)){const bool background=job.background;outgoing.removeAt(index);if(!background)fail("転送元の音源を開けません");return;}
        f.seek(qint64(job.offset));auto bytes=f.read(32768);
        if(bytes.isEmpty()){queue(*peer->second,"asset.complete",{{"assetId",job.hash}});outgoing.removeAt(index);return;}
        QByteArray packet=job.hash.toLatin1();for(int n=7;n>=0;--n)packet.append(char(job.offset>>(n*8)));packet+=bytes;
        if(peer->second->transport->sendBulk(packet))job.offset+=quint64(bytes.size());
    }
    // --- Junction Live -----------------------------------------------------
    bool neededByHandoff(const QString& hash) const {return hash==pendingGraphAsset||assetWaiters.count(hash)>0||references(preparedGraph).contains(hash);}
    /// Used only by Junction Live: verified in slices, failures stay on the entry.
    bool trackOnly(const QString& hash) const {return sharedTracks.find(hash)&&!neededByHandoff(hash);}
    bool trackSender(const QString& id,const QString& hash) const {return !trackTransfer.isEmpty()&&hash==trackTransfer&&id==trackTransferSource;}
    /// Only the audible performer (or the remote first DJ being started) may describe what is playing.
    bool trackSource(const QString& id) const {return hosting&&id!=auth.local&&(id==auth.owner||(auth.committed&&id==auth.committed->newOwner)||(bootstrapStart&&lifecycle=="starting"&&id==auth.next));}
    double trackProgress(const SharedTrackList::Entry& entry) const {
        if(entry.state==SharedTrackList::State::Verifying&&trackDigest&&trackDigest->hash==entry.track.assetId)return trackDigest->size?double(trackDigest->done)/double(trackDigest->size):0;
        const auto bitmap=cache.receivedBitmap(entry.track.assetId);return bitmap.isEmpty()?0:double(bitmap.count(char(1)))/double(bitmap.size());
    }
    void holdTrackPin(const QString& hash){if(!trackPins.contains(hash)){trackPins.insert(hash);cache.pin(hash,true);}}
    void releaseTrackPin(const QString& hash){if(trackPins.remove(hash)&&!pinnedAssets.contains(hash))cache.pin(hash,false);}
    void trackReady(const QString hash,const QString path){
        auto* entry=sharedTracks.find(hash);if(!entry||path.isEmpty())return;
        entry->state=SharedTrackList::State::Ready;entry->path=path;entry->detail.clear();holdTrackPin(hash);droppedTransfers.remove(hash);
        if(trackTransfer==hash)trackTransfer.clear();++auth.revision;
    }
    /// Scoped to the list entry: a Junction Live copy never marks the session failed.
    void trackFailed(const QString hash,const QString& detail,bool retryable){
        if(trackDigest&&trackDigest->hash==hash)trackDigest.reset();
        if(trackTransfer==hash)trackTransfer.clear();
        if(!neededByHandoff(hash)){requestedAssets.remove(hash);if(!retryable){cache.discard(hash);droppedTransfers.insert(hash);}}
        if(auto* entry=sharedTracks.find(hash)){entry->state=retryable&&++entry->attempts<3?SharedTrackList::State::Pending:SharedTrackList::State::Failed;entry->detail=detail;}
        releaseTrackPin(hash);++auth.revision;
    }
    /// A copy this computer already holds: the Junction cache, or an audio file
    /// it exported itself (e.g. its own decks after handing off). Verified before use.
    QString heldTrackCopy(const QString& hash) const {
        const auto cached=cache.existing(hash);if(!cached.isEmpty())return cached;
        const auto held=assets.find(hash);if(held==assets.end()||assetKinds.count(hash))return {};
        const QFileInfo info(held->second);
        return info.isAbsolute()&&info.isFile()&&!info.absoluteFilePath().startsWith(QDir(identityDir.path()).absolutePath()+"/")?held->second:QString{};
    }
    void startTrackDigest(const QString hash,bool partial){
        if(trackDigest)return;
        auto job=std::make_unique<TrackDigest>();job->hash=hash;job->partial=partial;job->path=partial?cache.partialPath(hash):heldTrackCopy(hash);
        job->file.setFileName(job->path);
        if(job->path.isEmpty()||!job->file.open(QIODevice::ReadOnly)){trackFailed(hash,partial?QStringLiteral("受信した音源を開けません"):QStringLiteral("キャッシュの音源を開けません"),!partial);return;}
        job->size=quint64(job->file.size());holdTrackPin(hash);
        if(auto* entry=sharedTracks.find(hash)){entry->state=SharedTrackList::State::Verifying;entry->detail.clear();}
        trackDigest=std::move(job);
    }
    void pumpTrackDigest(){
        if(!trackDigest)return;auto& job=*trackDigest;
        const auto bytes=job.file.read(kTrackDigestSliceBytes);
        if(bytes.isEmpty()&&!job.file.atEnd()){const auto hash=job.hash;const bool partial=job.partial;trackFailed(hash,QStringLiteral("音源を読み込めません"),partial);return;}
        job.sha.addData(bytes);job.done+=quint64(bytes.size());if(!job.file.atEnd())return;
        const auto hash=job.hash,path=job.path;const bool partial=job.partial;const auto digest=QString::fromLatin1(job.sha.result().toHex());
        job.file.close();trackDigest.reset();
        if(!partial){
            if(digest==hash){assets[hash]=path;trackReady(hash,path);}
            else if(path==cache.existing(hash))trackFailed(hash,QStringLiteral("キャッシュの音源が破損しています"),false);
            else{assets.erase(hash);trackFailed(hash,QStringLiteral("手元のファイルが変更されていたため再受信します"),true);}
            return;
        }
        QString failure;
        if(cache.finalizeVerified(hash,digest,[this,hash](const QString& candidate){return validateAsset(hash,candidate);},&failure)){assetReceived(hash,cache.existing(hash));return;}
        const bool handoff=neededByHandoff(hash);trackFailed(hash,digest==hash?QStringLiteral("この音源形式は検証できません"):QStringLiteral("音源が破損しています"),false);if(handoff)fail(failure);
    }
    /// Coordinator: fetch the next missing track, playing decks first, one at a
    /// time and only while no handoff transfer needs the channel.
    void pumpSharedTracks(){
        if(!hosting||sharedTracks.empty()||trackDigest)return;
        if(!trackTransfer.isEmpty()){
            auto* entry=sharedTracks.find(trackTransfer);
            if(!entry||entry->state!=SharedTrackList::State::Receiving)trackTransfer.clear();
            else{
                const auto bitmap=cache.receivedBitmap(trackTransfer);
                if(!bitmap.isEmpty()&&!bitmap.contains(char(0)))finishAsset(trackTransfer);
                else if(monotonicNanos()-trackTransferAt>kTrackTransferStallNanos)trackFailed(trackTransfer,QStringLiteral("受信が止まりました。再試行します"),true);
                return;
            }
        }
        // Nothing is fetched while a start or handoff is being prepared: the
        // stream, clock and graph gates get the link to themselves. Tracks
        // announced during a remote first start are fetched once it is live.
        if(auth.phase!="playing"&&auth.phase!="switching")return;
        QSet<QString> sources;if(auth.owner!=auth.local)sources.insert(auth.owner);if(auth.committed&&auth.committed->newOwner!=auth.local)sources.insert(auth.committed->newOwner);if(bootstrapStart&&lifecycle=="starting"&&!auth.next.isEmpty())sources.insert(auth.next);
        sharedTracks.retainPendingFrom(sources);
        const auto* next=sharedTracks.next();if(!next)return;const auto hash=next->track.assetId;
        if(!heldTrackCopy(hash).isEmpty()){startTrackDigest(hash,false);return;}
        const auto received=cache.receivedBitmap(hash);if(!received.isEmpty()&&!received.contains(char(0))){startTrackDigest(hash,true);return;}
        auto peer=peers.find(next->sourcePeerId);if(peer==peers.end()||!peer->second->hello||!peer->second->transport)return;
        trackTransfer=hash;trackTransferSource=peer->first;trackTransferAt=monotonicNanos();droppedTransfers.remove(hash);
        if(auto* entry=sharedTracks.find(hash)){entry->state=SharedTrackList::State::Receiving;entry->detail.clear();}
        ++auth.revision;
        if(requestedAssets.contains(hash))return;
        requestedAssets.insert(hash);queue(*peer->second,"asset.request",{{"assetId",hash},{"background",true}});
    }
    /// Performer: send only the current and next tracks. Position/rate updates
    /// replace this small presentation snapshot; they never restore or seek a
    /// deck, and file copies continue independently on the bulk channel.
    void announceTracks(){
        if(hosting||!q->localDeckTracks||auth.phase=="recovery")return;
        if(auth.owner!=auth.local&&!(bootstrapStart&&lifecycle=="starting"&&auth.next==auth.local))return;
        auto host=peers.find(auth.host);if(host==peers.end()||!host->second->hello||!host->second->junctionTracksV1||!host->second->transport)return;
        if(deckHashJob.valid()){if(deckHashJob.wait_for(std::chrono::seconds(0))!=std::future_status::ready)return;for(const auto& [path,file]:deckHashJob.get())if(!file.hash.isEmpty())deckHashes[path]=file;}
        struct Candidate {QJsonObject deck;QString path;QFileInfo info;HashedFile file;};
        std::vector<Candidate> candidates;QStringList missing;
        for(const auto& value:q->localDeckTracks()){
            const auto deck=value.toObject();const auto path=deck["path"].toString();const QFileInfo info(path);
            if(!info.isAbsolute()||!info.isFile())continue;
            const auto known=deckHashes.find(path);
            if(known==deckHashes.end()||known->second.size!=info.size()||known->second.modified!=info.lastModified().toMSecsSinceEpoch()){missing.append(path);continue;}
            candidates.push_back({deck,path,info,known->second});
        }
        const auto rank=[](const Candidate& a,const Candidate& b){
            const bool ap=a.deck["playing"].toBool(),bp=b.deck["playing"].toBool();if(ap!=bp)return ap>bp;
            const double aa=a.deck["audibility"].toDouble(),ba=b.deck["audibility"].toDouble();if(std::abs(aa-ba)>.000001)return aa>ba;
            return a.deck["loadGeneration"].toDouble()>b.deck["loadGeneration"].toDouble();
        };
        std::stable_sort(candidates.begin(),candidates.end(),rank);
        std::vector<AnnouncedTrack> rows;
        for(size_t index=0;index<candidates.size()&&rows.size()<2;++index){
            // No current track exists while Program is silent. A loaded track
            // is still useful as `next`, ready for the next play transition.
            const auto& candidate=candidates[index];const auto& deck=candidate.deck;
            if(std::any_of(rows.begin(),rows.end(),[&](const AnnouncedTrack& value){return value.assetId==candidate.file.hash;}))continue;
            AnnouncedTrack row;row.role=rows.empty()&&deck["playing"].toBool()?QStringLiteral("current"):QStringLiteral("next");
            if(row.role=="next"&&std::any_of(rows.begin(),rows.end(),[](const AnnouncedTrack& value){return value.role=="next";}))continue;
            row.deck=deck["deck"].toString();row.assetId=candidate.file.hash;row.sizeBytes=quint64(candidate.info.size());
            row.title=deck["title"].toString().trimmed().left(200);if(row.title.isEmpty())row.title=candidate.info.completeBaseName().left(200);
            row.artist=deck["artist"].toString().left(200);row.musicalKey=deck["musicalKey"].toString().left(16);
            row.durationMs=std::clamp(deck["durationMs"].toDouble(),0.0,86400000.0);row.bpm=std::clamp(deck["bpm"].toDouble(),0.0,1000.0);
            row.positionMs=std::clamp(deck["positionMs"].toDouble(),-60000.0,86400000.0);row.rate=std::clamp(deck["rate"].toDouble(1),.25,4.0);
            row.audibility=std::clamp(deck["audibility"].toDouble(),0.0,16.0);row.playing=deck["playing"].toBool();
            assets[row.assetId]=candidate.path;rows.push_back(row);
        }
        if(!missing.isEmpty()){
            if(deckHashes.size()>256)deckHashes.clear();
            deckHashJob=std::async(std::launch::async,[missing]{std::vector<std::pair<QString,HashedFile>> result;for(const auto& path:missing){const QFileInfo info(path);result.emplace_back(path,HashedFile{info.size(),info.lastModified().toMSecsSinceEpoch(),AssetCache::hashFile(path)});}return result;});
        }
        const auto signature=json(trackAnnouncement(rows,{},0));
        if(signature==announcedTracks&&monotonicNanos()-announcedAt<5000000000LL)return;
        announcedTracks=signature;announcedAt=monotonicNanos();if(announceStream.isEmpty())announceStream=secureRandomHex(12);
        queue(*host->second,"tracks.announce",trackAnnouncement(rows,announceStream,++announceRevision));
    }
    void openProgram() {
        if(!hosting || programOpened)return;
        QString error;program.setTimeline(timeline.originNanos(),timeline.originFrame(),delay);
        if(program.open(programDevice,&error)){
            programOpened=true;programState="running";separateLocalMaster.store(true);
            const auto localName=backend->audio()["deviceId"].toString();
            for(const auto& v:backend->audioDevices()["devices"].toArray()){const auto device=v.toObject();if(device["id"].toString().section(':',-1)==QString::number(programDevice)&&device["name"].toString()==localName)separateLocalMaster.store(false);}
        }else {programState="error";fail(error);}
    }
    void beginRecovery(const QString& reason){
        if(auth.phase=="recovery")return;
        auth.phase="recovery";recoveryUntil=now()+48000;ready=false;validationStart=0;fail(reason);
        if(hosting){
            if(auth.committed){backupOwner=auth.committed->oldOwner;backupEpoch=auth.committed->oldEpoch;}
            for(auto& [frame,block]:backupPending)if(frame>=programEnqueuedThrough)programPending.try_emplace(frame,block);
            broadcast("session.recovery",{{"stage","active"},{"reason",reason},{"fallbackUntil",u64(recoveryUntil)}});
        }
        ++auth.revision;
    }
    void collectValidation(const float* samples,const PcmBlockInfo& info) {
        QString failure;
        if(!validationCapture.consume(info,samples,info.frameCount,&failure)&&validationStart&&!validationSent)fail(failure);
    }
    void route(PcmRing& ring,const QString& producer) {
        for(int count=0;count<8;++count){auto result=ring.popBlock(pcm.data(),4096);if(!result.frames)return;
            if(producer==auth.local)collectValidation(pcm.data(),result.info);
            if(hosting&&!inputPeer.isEmpty()&&producer==inputPeer)feedInput(pcm.data(),result.info);
            if(hosting&&producer==returnRelaySource&&!returnRelayTarget.isEmpty()){
                auto target=liteReturnRings.find(returnRelayTarget);if(target!=liteReturnRings.end()){
                    auto info=result.info;info.epoch=returnEpoch.load(std::memory_order_relaxed);info.generation=returnGeneration.load(std::memory_order_relaxed);info.sequence=returnSequence.fetch_add(1,std::memory_order_relaxed);
                    target->second->push(pcm.data(),info);
                }
            }
            if(!hosting||!programOpened)continue;
            auto info=result.info;
            if(producer==auth.owner&&info.epoch==auth.epoch){
                ownerAudioAt=monotonicNanos();
                // A returned live stream resumes its existing owner and epoch.
                // Only the explicit recovery action can select the host instead.
                if(manual&&auth.phase=="recovery"&&!auth.committed&&!recoveryResumeFrame&&info.mediaFrame+4800>=now()&&info.mediaFrame<=now()+4800){auth.phase="playing";recoveryUntil=0;problem.clear();reasons.clear();++auth.revision;broadcast("session.snapshot",wireState());}
            }
            if(auth.committed&&producer==auth.committed->oldOwner&&info.epoch==auth.committed->oldEpoch){
                backupPending.insert_or_assign(info.mediaFrame,ProgramBlock{info,std::vector<float>(pcm.data(),pcm.data()+info.frameCount*2)});
                while(backupPending.size()>384)backupPending.erase(backupPending.begin());
            }
            const auto allowed=[&](quint64 f){if(f<programEnqueuedThrough)return false;if(auth.phase=="recovery"){if(recoveryResumeFrame&&f>=recoveryResumeFrame)return producer==auth.host&&info.epoch==recoveryEpoch;return f<recoveryUntil&&producer==backupOwner&&info.epoch==backupEpoch;}if(auth.committed)return f<auth.cutoverFrame?producer==auth.committed->oldOwner&&info.epoch==auth.committed->oldEpoch:producer==auth.committed->newOwner&&info.epoch==auth.committed->newEpoch;if(seam){const auto boundary=seamFrame.load(std::memory_order_acquire);if(!boundary||f<boundary)return producer==seam->oldOwner&&info.epoch==seam->oldEpoch;}return producer==auth.owner&&info.epoch==auth.epoch;};
            quint32 begin=0,end=info.frameCount;
            while(begin<end&&!allowed(info.mediaFrame+mediaFrameAdvance(begin,info.sampleRateHz)))++begin;
            while(end>begin&&!allowed(info.mediaFrame+mediaFrameAdvance(end-1,info.sampleRateHz)))--end;
            if(begin==end)continue;
            info.mediaFrame+=mediaFrameAdvance(begin,info.sampleRateHz);info.sourceFrame+=begin;info.frameCount=end-begin;
            if(programPending.size()<512)programPending.try_emplace(info.mediaFrame,ProgramBlock{info,std::vector<float>(pcm.data()+begin*2,pcm.data()+end*2)});
        }
    }
    void requestValidationCapture(quint64 start){
        for(auto& [id,p]:peers)if(p->approved&&(id==auth.owner||id==auth.next))queue(*p,"validation.window",{{"stage","capture"},{"startMediaFrame",u64(start)},{"handoffId",auth.handoffId}});
    }
    void beginValidation(quint64 start) {
        if(qEnvironmentVariableIsSet("PLUMDECK_JUNCTION_TRACE"))qWarning()<<"junction validation begin"<<hosting<<start<<captureEpoch.load()<<captureGeneration.load();
        validationStart=start;validationEnd=start+12000;validationId=secureRandomHex(16);validationPcm.clear();validationReceiving.clear();validationOutgoing.clear();validationAwaitingAck.clear();
        validationSent=auth.local!=auth.owner&&auth.local!=auth.next;
        if(validationSent){validationCapture.cancel();return;}
        QString failure;if(!validationCapture.begin(start,12000,captureEpoch.load(),captureGeneration.load(),&failure))fail(failure);
    }
    void finishValidation() {
        if(!validationStart||validationSent)return;
        auto window=validationCapture.take();if(!window)return;
        validationSent=true;auto wire=std::move(window->samples);
        if(qEnvironmentVariableIsSet("PLUMDECK_JUNCTION_TRACE"))qWarning()<<"junction validation complete"<<hosting<<wire.size();
        if(hosting){validationPcm[auth.local]=wire;checkValidation();return;}
        auto p=peers.find(auth.host);if(p==peers.end())return;
        QByteArray bytes(reinterpret_cast<const char*>(wire.data()),qsizetype(wire.size()*sizeof(float)));
        const auto hash=sha256Hex(bytes);queue(*p->second,"validation.window",{{"windowId",validationId},{"handoffId",auth.handoffId},{"startMediaFrame",u64(validationStart)},{"sampleRateHz",48000},{"channels",2},{"frameCount",int(wire.size()/2)},{"payloadSha256",hash}});
        validationTarget=auth.host;validationAwaitingAck=hash;
        for(int offset=0;offset<bytes.size();offset+=32768){QByteArray chunk=hash.toLatin1();for(int n=7;n>=0;--n)chunk.append(char(quint64(offset)>>(n*8)));chunk+=bytes.mid(offset,32768);validationOutgoing.enqueue(chunk);}
    }
    void validationChunk(const QString& sender,const QByteArray& chunk) {
        if(chunk.size()<72||chunk.size()>32840)return;
        auto it=validationReceiving.find(QString::fromLatin1(chunk.constData(),64));if(it==validationReceiving.end()||it->second.sender!=sender)return;
        quint64 offset=0;for(int n=64;n<72;++n)offset=(offset<<8)|quint8(chunk[n]);auto& rx=it->second;const auto size=quint64(chunk.size()-72);
        if(offset%32768||offset>=quint64(rx.bytes.size())||size!=std::min(quint64(32768),quint64(rx.bytes.size())-offset)||offset/32768>=rx.chunks.size())return;
        std::memcpy(rx.bytes.data()+offset,chunk.constData()+72,size);rx.chunks[offset/32768]=true;
        if(std::all_of(rx.chunks.begin(),rx.chunks.end(),[](bool x){return x;})){
            const bool valid=sha256Hex(rx.bytes)==it->first;
            if(valid){std::vector<float> samples(size_t(rx.bytes.size()/4));std::memcpy(samples.data(),rx.bytes.constData(),size_t(rx.bytes.size()));validationPcm[sender]=std::move(samples);}
            validationReceiving.erase(it);if(valid)checkValidation();
        }
    }
    QString applyCommit(const HandoffCommitMessage& commit,const QString& sender) {
        auto next=auth;const auto failure=next.commit(commit,sender);if(!failure.isEmpty())return failure;
        const auto directory=QStandardPaths::writableLocation(QStandardPaths::CacheLocation)+"/junction";
        if(!QDir().mkpath(directory))return "引き継ぎ状態を保存できません";
        QSaveFile file(directory+"/"+auth.sessionId+".commit");
        const auto bytes=json(commit.toJson());
        if(!file.open(QIODevice::WriteOnly)||file.write(bytes)!=bytes.size()||!file.flush()||platform_file::sync(file.handle())!=0||!file.commit())return "引き継ぎ状態を保存できません";
        auth=std::move(next);return {};
    }
    void scheduleCaptureCommit(const HandoffCommitMessage& commit) {
        if(auth.local==commit.newOwner){scheduledEpoch.store(commit.newEpoch,std::memory_order_relaxed);scheduledFrame.store(commit.effectiveMediaFrame,std::memory_order_release);}
    }
    void alignValidation(int lag,quint64 start){
        const auto source=lastCaptureSourceEnd.load(std::memory_order_acquire);
        const auto media=q->mediaFrameForSource(source);
        if(lag>0&&media<quint64(lag))return;
        q->setCaptureAnchor(source,lag>=0?media-quint64(lag):media+quint64(-lag));
        QPointer<Runtime> safe=q;const auto handoff=auth.handoffId;
        QTimer::singleShot(50,q,[safe,start,handoff]{if(safe&&safe->d->auth.phase=="fenced"&&safe->d->auth.handoffId==handoff)safe->d->beginValidation(start);});
    }
    void abortBootstrap(const QString& failure) {
        const auto handoffId=auth.handoffId;
        if(!handoffId.isEmpty())broadcast("handoff.cancel",{{"handoffId",handoffId},{"reason","bootstrap_failed"}});
        // applyCommit persists through a copy, so a write failure deliberately
        // leaves Authority fenced. Cancel it before exposing lobby/retry state.
        if(auth.phase=="preparing"||auth.phase=="fenced")auth.cancel();
        restoreLobby();resetPreparation();fail(failure);broadcast("session.snapshot",wireState(false));
    }
    void finishBootstrap() {
        if(!hosting||!bootstrapStart||lifecycle!="starting"||auth.phase!="preparing")return;
        const auto fence=now()+4800;auto failure=auth.fence(fence,controlSeq);if(!failure.isEmpty()){abortBootstrap(failure);return;}
        broadcast("handoff.fence",{{"frame",u64(fence)},{"throughSeq",u64(auth.throughSeq)},{"handoffId",auth.handoffId},{"bootstrap",true}});
        HandoffCommitMessage commit;commit.sessionId=auth.sessionId;commit.handoffId=auth.handoffId;commit.oldEpoch=auth.epoch;commit.newEpoch=auth.epoch+1;commit.oldOwner=auth.owner;commit.newOwner=auth.next;commit.effectiveMediaFrame=now()+24000;commit.fencedAtMediaFrame=auth.fenceFrame;commit.lastAppliedSeq=auth.throughSeq;commit.capsuleRevision=auth.revision;
        failure=applyCommit(commit,auth.local);if(!failure.isEmpty()){abortBootstrap(failure);return;}
        ownerAudioAt=monotonicNanos();broadcast("handoff.commit",commit.toJson());ready=true;problem.clear();reasons.clear();startDeadline=0;
    }
    void checkValidation() {
        if(!hosting||validationPcm.count(auth.owner)==0||validationPcm.count(auth.next)==0)return;
        const auto match=compareAudio(validationPcm[auth.owner],validationPcm[auth.next]);
        if(qEnvironmentVariableIsSet("PLUMDECK_JUNCTION_TRACE"))qWarning()<<"junction validation match"<<match.ready<<match.lagFrames<<match.correlation<<match.levelDb<<match.fractionalLagFrames;
        if(!match.ready){
            fail(QStringLiteral("音声の位置を調整中です（ずれ %1 samples）").arg(match.lagFrames));
            if(validationRound++<3){
                const bool adjust=match.correlation>=.99&&std::abs(match.levelDb)<=.25&&match.lagFrames;
                const auto start=now()+(adjust?24000:48000);
                // Finite FX tails may still be warming. Recheck a later raw
                // window under the same gate and original fence deadline.
                if(adjust&&auth.next==auth.local)alignValidation(match.lagFrames,start);
                else{
                    if(adjust){auto target=peers.find(auth.next);if(target!=peers.end())queue(*target->second,"validation.window",{{"stage","align"},{"lagFrames",match.lagFrames},{"startMediaFrame",u64(start)},{"handoffId",auth.handoffId}});}
                    beginValidation(start);
                }
                for(auto& [id,p]:peers)if((id==auth.owner||id==auth.next)&&(!adjust||id!=auth.next)&&p->approved)queue(*p,"validation.window",{{"stage","capture"},{"startMediaFrame",u64(start)},{"handoffId",auth.handoffId}});
            }
            return;
        }
        HandoffCommitMessage commit;commit.sessionId=auth.sessionId;commit.handoffId=auth.handoffId;commit.oldEpoch=auth.epoch;commit.newEpoch=auth.epoch+1;commit.oldOwner=auth.owner;commit.newOwner=auth.next;commit.effectiveMediaFrame=now()+24000;commit.fencedAtMediaFrame=auth.fenceFrame;commit.lastAppliedSeq=auth.throughSeq;commit.capsuleRevision=auth.revision;
        const auto failure=applyCommit(commit,auth.local);if(!failure.isEmpty()){fail(failure);return;}
        ownerAudioAt=monotonicNanos();scheduleCaptureCommit(commit);broadcast("handoff.commit",commit.toJson());ready=true;problem.clear();reasons.clear();
    }
    void tick() {
        ++ticks;if(networkProbe)testNetwork(false);if(auth.sessionId.isEmpty())return;
        manualTick();
        if(hosting&&lifecycle=="starting"&&auth.phase=="preparing"&&startDeadline&&monotonicNanos()>=startDeadline){const auto handoffId=auth.handoffId;broadcast("handoff.cancel",{{"handoffId",handoffId},{"reason","start_timeout"}});auth.cancel();restoreLobby();resetPreparation();fail("最初のDJとの準備が時間内に完了しませんでした。接続を確認して、もう一度開始してください");broadcast("session.snapshot",wireState(false));}
#if defined(PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL)
        if(turnRefreshAt&&monotonicNanos()>=turnRefreshAt&&signal&&signal->isOpen()){
            turnRefreshAt=monotonicNanos()+5000000000LL;signalSend({{"type","turn.credentials"}});
        }
        if(hosting&&reconnectAt&&monotonicNanos()>=reconnectAt){
            reconnectAt=monotonicNanos()+qint64(std::min(8u,1u<<std::min(3u,reconnectAttempts++)))*1000000000LL;
            if(signal){signal->resetCallbacks();signal->forceClose();signal.reset();}
            const auto failure=connect(true);if(!failure.isEmpty())fail(failure);
        }
#endif
        if(recoveryResumeFrame&&now()>=recoveryResumeFrame){
            auth.owner=auth.host;auth.epoch=recoveryEpoch;auth.cutoverFrame=recoveryResumeFrame;auth.phase="switching";auth.committed.reset();auth.next.clear();auth.handoffId.clear();recoveryResumeFrame=0;recoveryUntil=0;backupPending.clear();problem.clear();reasons.clear();ownerAudioAt=monotonicNanos();++auth.revision;
        }
        auth.advance(now());auth.localPrep=localPrep();auth.sending=localSending();audible.store(auth.local==auth.owner||auth.localPrep);mainAudible.store(auth.local==auth.owner);
        // Retired when Program has actually been fed past the boundary, so a
        // stalled or silent old stream cannot strand it.
        if(seam){const auto boundary=seamFrame.load(std::memory_order_acquire);if(boundary&&programEnqueuedThrough>=boundary)seam.reset();}
        if(const auto wanted=junctionInputPeer();wanted!=inputPeer){inputPeer=wanted;resetInput();++auth.revision;}
        if(!releasingPeer.isEmpty()){
            // The new operator faded the outgoing DJ out: stop their stream.
            const auto channel=backend->junctionInputState();
            const bool localSource=releasingPeer==auth.local;
            const bool localReceiver=auth.owner==auth.local;
            const bool audible=localReceiver?(channel["available"].toBool()&&channel["audible"].toBool()):true;
            if(inputRelease.observe(audible,localSource||input.receiving(),monotonicNanos()))releaseInput();
        }
        const auto ownerMessage=ticks%200==0?liteOwnerMessage():QJsonObject{};
        for(auto& [id,p]:peers){if(!p->transport)continue;if(!p->lite){for(int n=0;n<8&&!p->pending.isEmpty();++n){if(!p->transport->sendControl(p->pending.head().bytes))break;p->pending.dequeue();}if(ticks%200==0)p->transport->sendKeepAlive();}else if(ticks%200==0)sendLite(*p,ownerMessage);if(hosting||(liteSession&&id==auth.host))route(p->transport->decodedRing(),id);}
        if(endingAt){bool acknowledged=hosting;for(const auto& [id,p]:peers)if(p->approved&&!p->endingAck)acknowledged=false;
            if(acknowledged||monotonicNanos()>=endingAt){signalSend({{"type",endingHost?"room.close":"peer.leave"}});stop();return;}
        }
        route(tap.localRing(),auth.local);
        const auto ownerPeer=peers.find(auth.owner);const bool liteOwner=ownerPeer!=peers.end()&&ownerPeer->second->lite;
        if(hosting&&!liteOwner&&auth.owner!=auth.local&&ownerAudioAt&&monotonicNanos()-ownerAudioAt>300000000LL&&auth.phase!="recovery"&&(!auth.committed||now()>auth.cutoverFrame+14400))beginRecovery("プレイ担当者の音声が届いていません。配信を復旧中です");
        if(ticks%kSnapshotIntervalTicks==0 && hosting)broadcast("session.snapshot",wireState(false));
        if(ticks%200==0){if(hosting)signalSend({{"type","host.heartbeat"}});else{auto p=peers.find(auth.host);if(p!=peers.end()&&p->second->transport)queue(*p->second,"clock.probe",{{"t1",QString::number(monotonicNanos())}});}}
        if(!hosting&&bootstrapStart&&lifecycle=="starting"&&auth.phase=="preparing"&&auth.next==auth.local&&clock.ready()&&ticks%20==0){auto host=peers.find(auth.host);if(host!=peers.end()&&host->second->hello&&host->second->transport&&host->second->transport->aggregateLinkState()==LinkState::Connected){if(!host->second->producing){q->setCaptureAnchor(UINT64_MAX,0);captureEpoch.store(auth.epoch);sendManifest(*host->second);return;}ready=true;reasons.clear();queue(*host->second,"handoff.ready",{{"ready",true},{"bootstrap",true},{"handoffId",auth.handoffId}});}}
        if(exportJob.valid() && exportJob.wait_for(std::chrono::seconds(0))==std::future_status::ready){auto result=exportJob.get();if(exportHandoff!=auth.handoffId)return;graph=result.first;assets.insert(result.second.begin(),result.second.end());
            if(auth.phase=="preparing"||auth.phase=="fenced"){if(hosting){if(auth.next==auth.local){preparedGraph=graph;tryPrepare();}else{auto p=peers.find(auth.next);if(p!=peers.end())sendGraph(*p->second,graph);}}else{auto p=peers.find(auth.host);if(p!=peers.end())sendGraph(*p->second,graph);}}}
        if(prepared && auth.next==auth.local && (auth.phase=="preparing"||auth.phase=="fenced") && backend->junctionGraphReady() && ticks%20==0 && (hosting||clock.ready())){
            if(!aligned){auto failure=backend->alignJunctionGraph(now());if(failure.isEmpty())aligned=true;else fail(failure);}
            else if(backend->junctionGraphReady()){
                ready=true;reasons={"最終の音声照合を待っています"};auto p=peers.find(auth.host);if(p!=peers.end())queue(*p->second,"handoff.ready",{{"ready",true},{"stage",auth.phase},{"throughSeq",preparedGraph["throughSeq"]},{"handoffId",auth.handoffId}});
                if(hosting&&auth.phase=="fenced"&&!validationStart){beginValidation(now()+24000);requestValidationCapture(validationStart);}
            }
        }
        // A remote first DJ starts from their own decks: the coordinator never exports its graph for that bootstrap.
        const bool remoteBootstrap=bootstrapStart&&lifecycle=="starting";
        if(!remoteBootstrap&&auth.phase=="preparing"&&auth.owner==auth.local&&graphDirty&&monotonicNanos()-lastSharedChange>=250000000&&!exportJob.valid()){graphDirty=false;graph={};startExport();}
        if(!remoteBootstrap&&(auth.phase=="preparing"||(auth.phase=="fenced"&&finalCheckpoint))&&auth.owner==auth.local&&graph.isEmpty()&&!exportJob.valid())startExport();
        if(!remoteBootstrap&&auth.phase=="fenced"&&auth.owner==auth.local&&now()>=auth.fenceFrame+960&&!finalCheckpoint){
            auth.throughSeq=controlSeq;finalCheckpoint=true;graph={};startExport();broadcast("handoff.fenced",{{"fencedAtMediaFrame",u64(auth.fenceFrame)},{"lastAppliedSeq",u64(controlSeq)},{"handoffId",auth.handoffId}});
        }
        if(auth.phase=="fenced"&&fenceDeadline&&monotonicNanos()>fenceDeadline){if(hosting){broadcast("handoff.cancel",{{"handoffId",auth.handoffId},{"reason","timeout"}});auth.cancel();const bool firstStart=lifecycle=="starting";if(firstStart)restoreLobby();fail(firstStart?QStringLiteral("最初のDJとの同期が時間内に完了しませんでした。もう一度開始してください"):QStringLiteral("音声の照合が時間内に完了しませんでした。演奏は継続しています"));}validationStart=0;}
        finishValidation();
        if(!validationOutgoing.isEmpty()&&validationAwaitingAck.isEmpty()){auto p=peers.find(validationTarget);if(p!=peers.end()&&p->second->transport&&p->second->transport->sendValidation(validationOutgoing.head()))validationOutgoing.dequeue();}
        while(!programPending.empty()&&programPending.begin()->first+delay/2<now()){
            auto it=programPending.begin();if(!program.inputRing().push(it->second.samples.data(),it->second.info))break;programEnqueuedThrough=it->second.info.mediaFrame+mediaFrameAdvance(it->second.info.frameCount,it->second.info.sampleRateHz);programPending.erase(it);
        }
        if(auth.phase=="switching"&&now()>auth.cutoverFrame+delay+48000){const auto previousOwner=auth.committed?auth.committed->oldOwner:QString{};const bool wasBootstrap=bootstrapStart;auth.phase="playing";if(lifecycle=="starting")lifecycle="live";startDeadline=0;bootstrapStart=false;if(hosting){if(!wasBootstrap&&!previousOwner.isEmpty()&&previousOwner!=auth.owner){finishedOrder.removeAll(previousOwner);finishedOrder.append(previousOwner);}finishedOrder.removeAll(auth.owner);QStringList ordered{auth.owner};for(const auto& id:rosterOrder)if(id!=auth.owner&&!finishedOrder.contains(id))ordered.append(id);for(const auto& id:finishedOrder)if(id==auth.local||peers.count(id))ordered.append(id);rosterOrder=ordered;}backupPending.clear();backupOwner.clear();scheduledFrame.store(UINT64_MAX);scheduledEpoch.store(0);if(auth.local!=auth.owner){for(auto& [id,p]:peers)if(p->producing){p->transport->startProducer(nullptr);p->producing=false;}tap.enable(false,hosting);if(!hosting)captureEnabled.store(false);}auth.committed.reset();auth.next.clear();auth.handoffId.clear();validationStart=0;finalCheckpoint=false;ready=false;prepared=false;reasons.clear();++auth.revision;}

        if(ticks%4==0)pumpAssets();
        // Lightweight current-position deltas at 10 Hz. The signature guard
        // reduces an unchanged/paused pair to a five-second heartbeat.
        if(ticks%20==0)announceTracks();
        if(ticks%20==0)pumpSharedTracks();
        pumpTrackDigest();
        if(ticks%2000==0)pruneCapsules();
    }
    void stop() {
        ++signalGeneration;
#ifdef __APPLE__
        if(sleepLease!=kIOPMNullAssertionID){IOPMAssertionRelease(sleepLease);sleepLease=kIOPMNullAssertionID;}
#elif defined(_WIN32)
        SetThreadExecutionState(ES_CONTINUOUS);
#endif
        captureEnabled.store(false);tap.enable(false,false);audible.store(true);mainAudible.store(true);separateLocalMaster.store(true);stopLiteReturns();backend->junctionInputMainMix(false);
        inputPeer.clear();releasingPeer.clear();inputRelease.clear();liteSenders.clear();seam.reset();seamFrame.store(0);takeoverAnchor.store(false);resetInput();
        // The list disappears with the session. Cached files stay under the
        // cache's own quota/eviction once no longer pinned by a listed track.
        for(const auto& hash:trackPins)if(!pinnedAssets.contains(hash))cache.pin(hash,false);
        trackPins.clear();droppedTransfers.clear();sharedTracks.clear();trackTransfer.clear();trackTransferSource.clear();trackTransferAt=0;trackDigest.reset();announceSeen.clear();announcedTracks.clear();announcedAt=0;announceStream.clear();
        peers.clear();if(!localReturnReaders.load(std::memory_order_acquire))liteReturnRings.clear();program.close();programOpened=false;programState="stopped";
#if defined(PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL)
        if(signal){signal->resetCallbacks();signal->forceClose();signal.reset();}
#endif
        endingAt=0;endingHost=false;reconnectAt=0;reconnectAttempts=0;turnRefreshAt=0;startDeadline=0;controlSeq=0;iceReady=false;iceServers.clear();deferredSignals.clear();identity.reset();manual=false;liteSession=false;manualDetail.clear();manualErrorCode.clear();hostCertificatePem.clear();pinnedHostFingerprint.clear();participantRoster={};rosterOrder.clear();finishedOrder.clear();turnRequests.clear();lifecycle="live";bootstrapStart=false;avatarDataUrl.clear();themeColor.clear();auth=Authority{};timeline=MediaTimeline{};clock.reset();q->setCaptureAnchor(UINT64_MAX,0);captureEpoch.store(1);scheduledFrame.store(UINT64_MAX);scheduledEpoch.store(0);sourceAnchor.store(UINT64_MAX);connection="disconnected";problem.clear();reasons.clear();invite.clear();ready=false;prepared=false;preparedGraph={};outgoing.clear();programEnqueuedThrough=0;backupPending.clear();backupOwner.clear();recoveryResumeFrame=0;recoveryUntil=0;ownerAudioAt=0;assetWaiters.clear();requestedAssets.clear();programPending.clear();validationReceiving.clear();validationOutgoing.clear();validationStart=0;validationCapture.reset();finalCheckpoint=false;aligned=false;graph={};
    }
};
Runtime::Runtime(PlaybackBackend* backend,QObject* parent):QObject(parent),d(std::make_unique<Impl>(this,backend)){}
Runtime::~Runtime(){if(d->backend)d->stop();}
void Runtime::detachBackend(){
    if(!d->backend)return;
    d->stop();
    d->backend->attachJunction(nullptr);
    d->backend=nullptr;
}
quint64 Runtime::currentAppliedSequence()const{return d->controlSeq;}
quint64 Runtime::currentMediaFrame()const{return d->now();}
void Runtime::setCaptureAnchor(quint64 source,quint64 media){
    d->pendingAnchorRevision.fetch_add(1,std::memory_order_acq_rel);d->pendingAnchorSource.store(source,std::memory_order_relaxed);d->pendingAnchorMedia.store(media,std::memory_order_relaxed);d->pendingAnchorRevision.fetch_add(1,std::memory_order_release);
}
quint64 Runtime::mediaFrameForSource(quint64 source,unsigned rate)const{
    const bool pending=d->pendingAnchorRevision.load(std::memory_order_acquire)>0;
    const auto anchor=pending?d->pendingAnchorSource.load():d->sourceAnchor.load();
    const auto media=pending?d->pendingAnchorMedia.load():d->mediaAnchor.load();
    if(anchor==UINT64_MAX)return d->now();
    if(source>=anchor)return media+mediaFrameAdvance(source-anchor,rate);
    const auto back=mediaFrameAdvance(anchor-source,rate);return media>back?media-back:0;
}
bool Runtime::active()const{return !d->auth.sessionId.isEmpty();}
QJsonObject Runtime::monitorTrack(const QString& assetId,QString* error)const{
    auto reject=[&](const QString& reason){if(error)*error=reason;return QJsonObject{};};
    if(!active())return reject("Junctionセッションに参加していません");
    // The performer changes the shared graph through ordinary, sequenced
    // deck.load; a DJ being prepared must not have its restored decks replaced.
    if(d->auth.owner==d->auth.local)return reject("プレイ担当中は通常のデッキロードを使ってください");
    if(d->auth.next==d->auth.local||d->recoveryResumeFrame)return reject("引き継ぎの準備中は確認用のロードを利用できません");
    const auto* entry=d->sharedTracks.find(assetId);
    if(!entry)return reject("Junction Liveに該当する曲がありません");
    if(entry->state!=SharedTrackList::State::Ready||entry->path.isEmpty()||!QFileInfo(entry->path).isFile())return reject("この曲はまだ受信・検証中です");
    return {{"assetId",assetId},{"path",entry->path},{"title",entry->track.title},{"artist",entry->track.artist},{"musicalKey",entry->track.musicalKey},{"bpm",entry->track.bpm},{"durationMs",entry->track.durationMs}};
}

QJsonObject Runtime::snapshot()const{return d->publicState();}
bool Runtime::localMasterAudible()const noexcept{return d->mainAudible.load(std::memory_order_relaxed)&&d->separateLocalMaster.load(std::memory_order_relaxed);}
bool Runtime::sharedAudible()const noexcept{return d->audible.load(std::memory_order_relaxed);}
void Runtime::readJunctionInput(float* out,unsigned frames)noexcept{d->input.read(out,frames);}
QString Runtime::authorize(const QString& op,const QJsonObject& p)const{if(op=="audio.config.set"&&d->auth.local!=d->auth.owner){const auto mic=p["microphone"].toObject();if(mic.size()==1&&mic["enabled"].isBool()&&!mic["enabled"].toBool())return {};}d->auth.advance(d->now());d->auth.localPrep=d->localPrep();d->auth.sending=d->localSending();return d->auth.authorize(op,p["_junction"].toObject(),d->now());}
void Runtime::applied(const QString& op,const QJsonObject& params){if(!active()||d->auth.local!=d->auth.owner||Authority::localOnly(op)||Authority::readOnlyQuery(op))return;++d->controlSeq;++d->auth.revision;if(d->auth.phase=="preparing"){d->ready=false;d->graphDirty=true;d->lastSharedChange=monotonicNanos();d->reasons={"演奏の変更に同期しています"};d->broadcast("graph.applied",{{"throughSeq",u64(d->controlSeq)}});}Q_UNUSED(params);}
void Runtime::capture(const float* pcm,unsigned frames,quint64 sourceFrame,unsigned rate)noexcept{
    if(!d->captureEnabled.load(std::memory_order_relaxed))return;
    const auto revision=d->pendingAnchorRevision.load(std::memory_order_acquire);
    if(!(revision&1)&&revision!=d->appliedAnchorRevision){
        const auto source=d->pendingAnchorSource.load(std::memory_order_relaxed),media=d->pendingAnchorMedia.load(std::memory_order_relaxed);
        if(revision==d->pendingAnchorRevision.load(std::memory_order_acquire)){
            d->sourceAnchor.store(source,std::memory_order_relaxed);d->mediaAnchor.store(media,std::memory_order_relaxed);d->appliedAnchorRevision=revision;d->captureGeneration.fetch_add(1,std::memory_order_relaxed);
        }
    }
    auto anchor=d->sourceAnchor.load(std::memory_order_relaxed);
    if(anchor==UINT64_MAX||sourceFrame<anchor){
        if(anchor!=UINT64_MAX)d->captureGeneration.fetch_add(1,std::memory_order_relaxed);
        d->sourceAnchor.store(sourceFrame,std::memory_order_relaxed);anchor=sourceFrame;
        const auto ns=monotonicNanos()-d->timelineAnchor.load(std::memory_order_relaxed);
        const quint64 elapsed=ns>0?quint64(ns/1000000000)*48000+quint64(ns%1000000000)*48000/1000000000:0;
        // Taking over from the JUNCTION deck: this very block of master is
        // carrying the frame the deck reported rendering in the same callback,
        // so the capture starts there. The flag is consumed either way, which
        // is what keeps it from reaching any later handoff.
        const bool takeover=d->takeoverAnchor.exchange(false,std::memory_order_acq_rel);
        const auto media=takeover?TakeoverAnchor::resolve(d->input.renderedMediaFrame(),elapsed):elapsed;
        d->mediaAnchor.store(media,std::memory_order_relaxed);
        // The boundary the outgoing DJ's direct stream plays up to: exactly
        // where this capture begins, so Program neither gaps nor doubles.
        if(takeover)d->seamFrame.store(media,std::memory_order_release);
    }
    d->lastCaptureSourceEnd.store(sourceFrame+frames,std::memory_order_release);
    PcmBlockInfo info;info.epoch=d->captureEpoch.load(std::memory_order_relaxed);info.sourceFrame=sourceFrame;info.mediaFrame=d->mediaAnchor.load(std::memory_order_relaxed)+mediaFrameAdvance(sourceFrame-anchor,rate);info.frameCount=frames;info.sampleRateHz=rate;info.channels=2;info.sequence=d->blockSequence.fetch_add(1,std::memory_order_relaxed);info.generation=d->captureGeneration.load(std::memory_order_relaxed);
    const auto cutover=d->scheduledFrame.load(std::memory_order_acquire);
    const auto nextEpoch=d->scheduledEpoch.load(std::memory_order_relaxed);
    if(nextEpoch&&info.mediaFrame+mediaFrameAdvance(frames,rate)>cutover){
        const quint32 before=info.mediaFrame>=cutover?0:std::min(quint64(frames),((cutover-info.mediaFrame)*rate+47999)/48000);
        if(before){auto old=info;old.frameCount=before;d->tap.capture(pcm,old);info.sequence=d->blockSequence.fetch_add(1,std::memory_order_relaxed);}
        info.epoch=nextEpoch;info.sourceFrame+=before;info.mediaFrame+=mediaFrameAdvance(before,rate);info.frameCount-=before;d->captureEpoch.store(nextEpoch,std::memory_order_relaxed);
        if(info.frameCount)d->tap.capture(pcm+before*2,info);
    }else d->tap.capture(pcm,info);
}
void Runtime::captureLocalReturn(const float* pcm,unsigned frames,quint64 sourceFrame,unsigned rate)noexcept{
    d->localReturnReaders.fetch_add(1,std::memory_order_acq_rel);
    auto* target=d->localReturnTarget.load(std::memory_order_acquire);
    if(target&&pcm&&frames&&frames<=target->maxBlockFrames()){
        PcmBlockInfo info;info.epoch=d->returnEpoch.load(std::memory_order_relaxed);info.generation=d->returnGeneration.load(std::memory_order_relaxed);
        info.mediaFrame=mediaFrameForSource(sourceFrame,rate);info.sourceFrame=sourceFrame;info.sequence=d->returnSequence.fetch_add(1,std::memory_order_relaxed);
        info.frameCount=frames;info.sampleRateHz=rate;info.channels=2;target->push(pcm,info);
    }
    d->localReturnReaders.fetch_sub(1,std::memory_order_release);
}
QJsonObject Runtime::command(const QString& op,const QJsonObject& p,QString* error){
    auto reject=[&](const QString& reason){if(error)*error=reason;return QJsonObject{};};
    if(op=="snapshot")return snapshot();
    if(op.startsWith("network.")){
        if(op=="network.get")return d->network.summary();
        if(op=="network.test"){
            if(p["cancel"].toBool()){d->networkProbe.reset();d->networkTestDeadline=0;d->networkTestResult={{"state","failure"},{"detail","接続テストを中止しました"}};return d->networkTestResult;}
            return d->testNetwork(!p["poll"].toBool());
        }
        if(op=="network.configure"||op=="network.clear"){
            auto failure=op=="network.clear"?d->network.clear():d->network.configure(p);if(!failure.isEmpty())return reject(failure);
            d->networkProbe.reset();d->networkTestResult={};return d->network.summary();
        }
    }
    if(op=="exchange.inspect"){
        QString failure,code;auto packet=decodeExchangePacket(p["text"].toString(),QDateTime::currentMSecsSinceEpoch(),&failure,&code);
        if(!packet||!failure.isEmpty())return reject(failure);
        if(!verifyExchangeSignature(*packet,packet->descriptionFingerprint(),&failure))return reject(failure);
        return packet->sanitized();
    }
    if(op=="lite.join"){
        if(active())return reject("参加中のセッションを終了してから操作してください");
        if(!d->backend->available()||!MediaTransport::available())return reject("音声エンジンとWebRTCの準備が必要です");
        const auto sessionId=p["sessionId"].toString(),local=p["localPeerId"].toString(),host=p["hostPeerId"].toString(),offer=p["sdp"].toString();
        if(!validOpaqueId(sessionId)||!validOpaqueId(local)||!validOpaqueId(host)||local==host||offer.isEmpty()||offer.size()>65536)return reject("PlumDeck Liteの接続情報が無効です");
        const auto fp=sdpFingerprint(offer);if(fp.size()!=64)return reject("PlumDeck Liteの接続情報に識別情報がありません");
        d->displayName=sanitizeDisplayName(p["djName"].toString(p["displayName"].toString()));if(d->displayName.isEmpty())return reject("DJ名を入力してください");
        d->name=p["sessionName"].toString(QStringLiteral("PlumDeck Lite Junction")).left(80);d->hosting=false;d->liteSession=true;d->lifecycle="live";d->auth.sessionId=sessionId;d->auth.local=local;d->auth.host=host;d->auth.owner=p["ownerPeerId"].toString(host);if(d->auth.owner!=host&&d->auth.owner!=local)d->auth.owner=host;d->auth.phase="playing";d->timeline.start(monotonicNanos());d->timelineAnchor.store(d->timeline.originNanos());d->connection="connecting";d->audible.store(d->auth.owner==local);d->captureEnabled.store(false);d->tap.enable(false,false);
        auto peer=std::make_unique<Impl::Peer>();peer->id=host;peer->name=sanitizeDisplayName(p["hostName"].toString(QStringLiteral("ホスト")));peer->approved=true;peer->lite=true;d->peers[host]=std::move(peer);d->rosterOrder={host,local};
        auto& created=*d->peers[host];const auto failure=d->buildLitePeer(created,false);if(!failure.isEmpty()){d->stop();return reject(failure);}QString remoteError;if(!created.transport->remoteDescription(false,offer,"offer",fp,&remoteError)){d->stop();return reject(remoteError);}
        if(d->auth.owner==local){d->captureEpoch.store(d->auth.epoch);created.transport->startProducer(&d->tap.networkRing());created.producing=true;d->captureEnabled.store(true);d->tap.enable(true,true);}
        return snapshot();
    }
    if(op=="lite.exchange"){
        if(!active())return reject("Junctionセッションに参加していません");const auto id=p["peerId"].toString(d->hosting?QString{}:d->auth.host);auto found=d->peers.find(id);if(found==d->peers.end()||!found->second->lite)return reject("PlumDeck Liteの参加者が見つかりません");
        auto& peer=*found->second;const auto stats=peer.transport?peer.transport->statistics():MediaTransport::Statistics{};return {{"state",peer.transport&&peer.transport->linkState(false)==LinkState::Connected?"connected":peer.liteSdp.isEmpty()?"gathering":"ready"},{"sdp",peer.liteSdp},{"type",peer.liteSdpType},{"receivedPackets",QString::number(stats.receivedPackets)},{"sentPackets",QString::number(stats.sentPackets)}};
    }
    if(op=="lite.guest.offer"){
        if(!active()||d->hosting||!d->liteSession)return reject("PlumDeck Liteのゲスト接続が必要です");const auto sdp=p["sdp"].toString(),fp=sdpFingerprint(sdp);auto found=d->peers.find(d->auth.host);if(found==d->peers.end()||!found->second->lite||fp.size()!=64||sdp.size()>65536)return reject("PlumDeck Liteの接続情報が無効です");auto& peer=*found->second;if(peer.transport)peer.transport->close();peer.transport.reset();peer.liteSdp.clear();peer.liteSdpType.clear();peer.hello=false;peer.producing=false;const auto failure=d->buildLitePeer(peer,false);if(!failure.isEmpty())return reject(failure);QString remoteError;if(!peer.transport->remoteDescription(false,sdp,"offer",fp,&remoteError))return reject(remoteError);if(d->auth.owner==d->auth.local){peer.transport->setBrowserSendEpoch(d->auth.epoch,d->now());peer.transport->startProducer(&d->tap.networkRing());peer.producing=true;d->captureEnabled.store(true);d->tap.enable(true,true);}return snapshot();
    }
    if(op=="lite.peer.ensure"){
        if(!active()||!d->hosting)return reject("ホストのJunctionセッションが必要です");const auto id=p["peerId"].toString();if(!validOpaqueId(id)||id==d->auth.local)return reject("PlumDeck Liteの参加者IDが無効です");auto found=d->peers.find(id);if(found==d->peers.end()){auto peer=std::make_unique<Impl::Peer>();peer->id=id;peer->name=sanitizeDisplayName(p["djName"].toString(p["displayName"].toString(QStringLiteral("DJ"))));peer->approved=true;peer->lite=true;d->peers[id]=std::move(peer);d->rosterOrder.append(id);found=d->peers.find(id);++d->auth.revision;}else if(!found->second->lite)return reject("同じ参加者IDが別の接続で使用されています");
        if(found->second->transport&&(found->second->transport->linkState(false)==LinkState::Failed||found->second->transport->linkState(false)==LinkState::Closed)){found->second->transport->close();found->second->transport.reset();found->second->liteSdp.clear();found->second->liteSdpType.clear();found->second->hello=false;}
        const auto failure=d->buildLitePeer(*found->second,true);if(!failure.isEmpty())return reject(failure);auto& peer=*found->second;const auto stats=peer.transport?peer.transport->statistics():MediaTransport::Statistics{};return {{"state",peer.transport&&peer.transport->linkState(false)==LinkState::Connected?"connected":peer.liteSdp.isEmpty()?"gathering":"ready"},{"sdp",peer.liteSdp},{"type",peer.liteSdpType},{"receivedPackets",QString::number(stats.receivedPackets)},{"sentPackets",QString::number(stats.sentPackets)}};
    }
    if(op=="lite.peer.answer"){
        if(!active()||!d->hosting)return reject("ホストのJunctionセッションが必要です");const auto id=p["peerId"].toString(),sdp=p["sdp"].toString();auto found=d->peers.find(id);if(found==d->peers.end()||!found->second->lite||!found->second->transport)return reject("PlumDeck Liteの参加者が見つかりません");const auto fp=sdpFingerprint(sdp);QString failure;if(fp.size()!=64||sdp.size()>65536||!found->second->transport->remoteDescription(false,sdp,"answer",fp,&failure))return reject(failure.isEmpty()?QStringLiteral("PlumDeck Liteの返答が無効です"):failure);return snapshot();
    }
    if(op=="lite.peer.remove"){
        if(!active()||!d->hosting)return reject("ホストのJunctionセッションが必要です");const auto id=p["peerId"].toString();auto found=d->peers.find(id);if(found!=d->peers.end()&&found->second->lite){if(d->auth.owner==id)d->selectLiteOwner(d->auth.local);if(d->releasingPeer==id)d->releaseInput();d->peers.erase(found);d->rosterOrder.removeAll(id);d->finishedOrder.removeAll(id);d->turnRequests.remove(id);++d->auth.revision;}return snapshot();
    }
    if(op=="lite.owner.set"){
        if(!active())return reject("Junctionセッションに参加していません");const auto owner=p["ownerPeerId"].toString();if(d->hosting&&!d->releasingPeer.isEmpty()&&owner!=d->auth.owner)return reject("JUNCTIONデッキを解放してから次のDJへ交代してください");if(d->hosting||d->liteSession)d->selectLiteOwner(owner);return snapshot();
    }
    if(op=="input.set"){
        if(!active())return reject("Junctionセッションに参加していません");
        if(d->localSending())return reject("交代後も音声を送出中のため、解放されるまで操作できません");
        const auto failure=d->backend->junctionInputSet(p);if(!failure.isEmpty())return reject(failure);
        return d->inputState();
    }
    if(op=="input.release"){
        if(!active())return reject("Junctionセッションに参加していません");
        d->releaseInput();return snapshot();
    }
    if(op=="lite.roster.set"){
        if(!active()||d->hosting||!d->liteSession||!p["members"].isArray())return reject("PlumDeck Liteのゲスト接続が必要です");const auto members=p["members"].toArray();if(members.size()>8)return reject("PlumDeck Liteの参加者一覧が不正です");QJsonArray roster;int order=0;for(const auto& value:members){const auto member=value.toObject();const auto id=member["peerId"].toString();if(!validOpaqueId(id))return reject("PlumDeck Liteの参加者一覧が不正です");if(member["status"]!="approved")continue;const auto memberName=sanitizeDisplayName(member["displayName"].toString(QStringLiteral("DJ")));roster.append(QJsonObject{{"peerId",id},{"displayName",memberName},{"djName",memberName},{"approved",true},{"isHost",id==d->auth.host},{"orderIndex",order++},{"status","connected"}});}d->participantRoster=roster;++d->auth.revision;return snapshot();
    }
    const auto input=p["text"].toString(p["invite"].toString()).trimmed();
    const bool manualCreate=op=="create"&&(p["exchangeMode"]=="manual"||p["signalingUrl"].toString().isEmpty());
    const bool manualJoin=op=="join"&&input.startsWith("PLUMDECK-JUNCTION-");
    if(manualCreate||manualJoin){
        if(active())return reject("参加中のセッションを終了してから操作してください");
        if(!d->backend->available()||!MediaTransport::available())return reject("音声エンジンとWebRTCの準備が必要です");
        const auto displayName=sanitizeDisplayName(p["djName"].toString(p["displayName"].toString()));if(displayName.isEmpty())return reject("DJ名を入力してください");
        const auto requestedAvatar=p["avatarDataUrl"].toString();const auto avatar=profileAvatar(requestedAvatar);if(!requestedAvatar.isEmpty()&&avatar.isEmpty())return reject("アイコン画像は4,096文字以下のPNG・JPEG・WebPを指定してください");
        QString failure,code;std::optional<ExchangePacket> packet;
        if(manualJoin){packet=decodeExchangePacket(input,QDateTime::currentMSecsSinceEpoch(),&failure,&code);if(!packet||!failure.isEmpty())return reject(failure);if(packet->kind!=ExchangeKind::Invite)return reject("ホストから届いた招待を取り込んでください");if(!verifyExchangeSignature(*packet,packet->hostFingerprint,&failure))return reject(failure);}
        if(manualCreate&&!p["startInLobby"].toBool()&&!p["adoptCurrent"].toBool()){
            bool sounding=d->backend->audio()["microphone"].toObject()["enabled"].toBool();for(int deck=0;deck<4;++deck)sounding|=d->backend->playing(deck);
            for(const auto& row:d->backend->samplerState()["slots"].toArray())sounding|=row.toObject()["playing"].toBool();
            if(sounding)return reject("現在の演奏を使う場合は「現在の演奏をこのセッションで使う」を選択してください");
        }
        d->displayName=displayName;d->avatarDataUrl=avatar;d->themeColor=profileColor(p["themeColor"].toString(),displayName);d->lifecycle=manualCreate&&!p["startInLobby"].toBool()?QStringLiteral("live"):QStringLiteral("lobby");d->name=p["sessionName"].toString(displayName+" のセッション").trimmed().left(80);if(d->name.isEmpty())return reject("セッション名を入力してください");
        bool ok=false;d->programDevice=p["programDevice"].toString().toInt(&ok);if(!ok)d->programDevice=-1;
        if(manualCreate)d->auth.sessionId=secureRandomHex(16);
        failure=d->startManual(manualCreate);if(failure.isEmpty()&&manualJoin)failure=d->acceptManualInvite(*packet,true);
        if(!failure.isEmpty()){d->stop();return reject(failure);}return snapshot();
    }
    if(d->manual&&active()){
        if(op=="invite.create"){
            if(!d->hosting)return reject("ホストだけが招待を作成できます");
            const auto requested=p["peerId"].toString();auto i=d->peers.find(requested);
            if(!requested.isEmpty()&&i==d->peers.end())return reject("参加者が見つかりません");
            if(requested.isEmpty()){
                if(d->peers.size()>=7){
                    auto unused=std::find_if(d->peers.begin(),d->peers.end(),[](const auto& item){return !item.second->approved&&exchangeStateTerminal(item.second->manual.state);});
                    if(unused!=d->peers.end()){d->rosterOrder.removeAll(unused->first);d->finishedOrder.removeAll(unused->first);d->turnRequests.remove(unused->first);d->peers.erase(unused);}
                    else return reject("このセッションはホストを含め8人までです。不要な招待を取り消してください");
                }
                auto peer=std::make_unique<Impl::Peer>();peer->id=secureRandomHex(16);peer->name=sanitizeDisplayName(p["djName"].toString(p["displayName"].toString()));if(peer->name.isEmpty())peer->name="招待中のDJ";const auto requestedAvatar=p["avatarDataUrl"].toString();peer->avatarDataUrl=profileAvatar(requestedAvatar);if(!requestedAvatar.isEmpty()&&peer->avatarDataUrl.isEmpty())return reject("アイコン画像が大きすぎるか、対応していない形式です");peer->themeColor=profileColor(p["themeColor"].toString(),peer->id);auto id=peer->id;i=d->peers.emplace(id,std::move(peer)).first;d->rosterOrder.append(id);
            }
            auto& peer=*i->second;if(!peer.approved){const auto requestedName=sanitizeDisplayName(p["djName"].toString(p["displayName"].toString()));if(!requestedName.isEmpty())peer.name=requestedName;if(p.contains("avatarDataUrl")){const auto requestedAvatar=p["avatarDataUrl"].toString();const auto avatar=profileAvatar(requestedAvatar);if(!requestedAvatar.isEmpty()&&avatar.isEmpty())return reject("アイコン画像が大きすぎるか、対応していない形式です");peer.avatarDataUrl=avatar;}if(p.contains("themeColor"))peer.themeColor=profileColor(p["themeColor"].toString(),peer.id);}QString failure;
            auto ice=d->network.credentials(d->auth.sessionId,peer.id,QDateTime::currentMSecsSinceEpoch(),&failure);if(!failure.isEmpty())return reject(failure);
            peer.manual.ice=ice;peer.manual.inviteId=secureRandomHex(16);++peer.manual.generation;++peer.manual.attempt;peer.manual.expiresAt=QDateTime::currentMSecsSinceEpoch()+900000;
            for(const auto& v:ice)if(v.toObject().contains("expiresAt"))peer.manual.expiresAt=std::min(peer.manual.expiresAt,qint64(v.toObject()["expiresAt"].toDouble()));
            failure=d->manualStartAttempt(peer,true,ice);if(!failure.isEmpty()){d->manualSetState(peer,ExchangeState::Failed,failure);return reject(failure);}d->broadcast("session.snapshot",d->wireState(true));return snapshot();
        }
        if(op=="exchange.import"){
            QString failure,code;auto packet=decodeExchangePacket(input,QDateTime::currentMSecsSinceEpoch(),&failure,&code);if(!packet||!failure.isEmpty())return reject(failure);
            if(packet->sessionId!=d->auth.sessionId||packet->hostPeerId!=d->auth.host)return reject("別のセッションへの接続情報です");
            if(!d->hosting&&packet->kind==ExchangeKind::Invite){failure=d->acceptManualInvite(*packet,false);if(!failure.isEmpty())return reject(failure);return snapshot();}
            auto i=d->peers.find(d->hosting?packet->peerId:d->auth.host);if(i==d->peers.end())return reject("対応する招待が見つかりません");auto& peer=*i->second;
            if(packet->inviteId!=peer.manual.inviteId||packet->generation!=peer.manual.generation||packet->attempt!=peer.manual.attempt)return reject("別の招待、または更新前の接続情報です。最新の招待への返答を取り込んでください");
            if(d->hosting){
                const auto expectedPeer=p["peerId"].toString();
                if(!expectedPeer.isEmpty()&&packet->peerId!=expectedPeer)return reject("別のDJへの返答です。返答を送ったDJの行で入力してください");
                if(packet->kind!=ExchangeKind::Response)return reject("DJから届いた返答を取り込んでください");
                if(packet->hostFingerprint!=d->identity->fingerprint||packet->expiresAt!=peer.manual.expiresAt)return reject("招待と返答の識別情報が一致しません");
                if(exchangeStateTerminal(peer.manual.state))return reject("この招待は取り消し済み、または無効です。新しい招待を作成してください");
                if(!verifyExchangeSignature(*packet,packet->peerFingerprint,&failure))return reject(failure);
                if(peer.approved&&!peer.fp.isEmpty()&&packet->peerFingerprint!=peer.fp)return reject("接続済みDJと異なる端末の返答です。別の参加者として招待してください");
                const auto digest=sha256Hex(packet->canonicalPayload());if(digest==peer.manual.answerDigest)return reject("この返答は取り込み済みです。参加者カードで次の操作を確認してください");
                if(peer.manual.state!=ExchangeState::InviteReady)return reject("この招待の返答はすでに取り込まれています。必要なら招待を作り直してください");
                peer.manual.answerDigest=digest;peer.manual.answerPending=true;peer.manual.answerName=packet->peerName;peer.name=packet->peerName;peer.manual.answerFingerprint=packet->peerFingerprint;
                for(int j=0;j<2;++j){peer.manual.answerSdp[j]=packet->description[j].sdp;peer.manual.answerType[j]=packet->description[j].type;}
                d->manualSetState(peer,ExchangeState::ApprovalPending,"表示名だけでは本人確認になりません。返答の送り主を確認して、参加を許可してください");return snapshot();
            }
            if(packet->kind!=ExchangeKind::Notice||packet->peerId!=d->auth.local)return reject("ホストから届いた招待・通知を取り込んでください");
            if(!verifyExchangeSignature(*packet,d->pinnedHostFingerprint,&failure))return reject(failure);
            d->discardAttempt(peer);d->manualSetState(peer,packet->noticeReason=="rejected"?ExchangeState::Rejected:ExchangeState::Cancelled,packet->noticeText);return snapshot();
        }
        if(op=="invite.cancel"||op=="peer.retry"||op=="peer.approve"){
            const auto id=d->hosting?p["peerId"].toString():d->auth.host;auto i=d->peers.find(id);if(i==d->peers.end())return reject("参加者が見つかりません");auto& peer=*i->second;
            if(op=="peer.retry"){
                if(peer.transport&&peer.transport->linkState(false)==LinkState::Connected&&peer.lastControlAt&&monotonicNanos()-peer.lastControlAt<=kControlSilenceNanos){d->discardAttempt(peer);d->manualSetState(peer,ExchangeState::Connected,"接続は継続しています");return snapshot();}
                if(peer.manual.state==ExchangeState::Interrupted&&peer.transport){d->queue(peer,"peer.hello",d->localProfile());peer.transport->sendKeepAlive();peer.manual.detail="同じ接続への自動再接続を続けています";++d->auth.revision;return snapshot();}
                d->discardAttempt(peer);d->manualSetState(peer,ExchangeState::NeedsExchange,"新しい接続情報の交換が必要です。ホストがこの参加者の招待を作り直してください");return snapshot();
            }
            if(op=="peer.approve"){
                if(!d->hosting)return reject("ホストだけが参加を承認できます");
                if(peer.manual.state!=ExchangeState::ApprovalPending)return reject("返答を取り込んでから参加を許可してください");
                if(!p["accept"].isBool())return reject("参加を許可するか指定してください");
                if(p["accept"].toBool()){
                    if(peer.manual.expiresAt<=QDateTime::currentMSecsSinceEpoch())return reject("招待の期限が切れました。作り直してください");
                    auto failure=d->manualApplyAnswer(peer);if(!failure.isEmpty()){d->discardAttempt(peer);d->manualSetState(peer,ExchangeState::Failed,failure);return reject(failure);}peer.approved=true;return snapshot();
                }
            }
            const bool rejection=op=="peer.approve";QString notice;
            if(d->hosting){auto failure=d->manualBuildNotice(peer,rejection?"rejected":"cancelled",rejection?"ホストが参加を許可しませんでした":"ホストがこの招待を取り消しました");if(!failure.isEmpty())return reject(failure);notice=peer.manual.noticeText;}
            d->discardAttempt(peer);peer.manual.noticeText=notice;d->manualSetState(peer,rejection?ExchangeState::Rejected:ExchangeState::Cancelled,d->hosting?"招待を取り消しました。相手には通知をコピーして送ってください":"接続操作を取り消しました。ホストから新しい招待を受け取ってください");return snapshot();
        }
    }
    if(op=="create"||op=="join"){
        if(active())return reject("参加中のセッションを終了してから操作してください");
        if(!d->backend->available()||!MediaTransport::available())return reject("音声エンジンとWebRTCの準備が必要です");
        d->displayName=sanitizeDisplayName(p["djName"].toString(p["displayName"].toString()));if(d->displayName.isEmpty())return reject("DJ名を入力してください");const auto requestedAvatar=p["avatarDataUrl"].toString();d->avatarDataUrl=profileAvatar(requestedAvatar);if(!requestedAvatar.isEmpty()&&d->avatarDataUrl.isEmpty())return reject("アイコン画像は4,096文字以下のPNG・JPEG・WebPを指定してください");d->themeColor=profileColor(p["themeColor"].toString(),d->displayName);
        d->origin=p["signalingUrl"].toString();d->hosting=op=="create";
        if(!d->hosting){QUrl u(p["invite"].toString());QUrlQuery query(u);if(u.scheme()!="plumdeck-junction"||u.host()!="join"||query.queryItemValue("version")!="1")return reject("招待が無効です");d->origin=query.queryItemValue("signaling");d->room=query.queryItemValue("room");d->token=query.queryItemValue("token");bool ok;d->inviteExpiry=query.queryItemValue("expiresAt").toLongLong(&ok);if(!ok||d->inviteExpiry<=QDateTime::currentMSecsSinceEpoch()||!validOpaqueId(d->room)||d->token.size()<22||d->token.size()>128)return reject("招待が無効、または期限切れです");d->pendingInvite={{"host",query.queryItemValue("host")}};if(d->pendingInvite["host"].toString().size()!=64)return reject("ホストの識別情報が無効です");}
        if(!validOrigin(QUrl(d->origin)))return reject("WSS接続先を設定してください。開発用WSはローカルホストで利用できます");
        d->lifecycle=d->hosting&&!p["startInLobby"].toBool()?QStringLiteral("live"):QStringLiteral("lobby");
        if(d->hosting&&d->lifecycle!="lobby"&&!p["adoptCurrent"].toBool()){
            bool sounding=d->backend->audio()["microphone"].toObject()["enabled"].toBool();
            for(int deck=0;deck<4;++deck)sounding|=d->backend->playing(deck);
            const auto sampler=d->backend->samplerState();for(const auto& row:sampler["slots"].toArray())sounding|=row.toObject()["playing"].toBool();
            if(sounding)return reject("現在の演奏を使う場合は「現在の演奏をこのセッションで使う」を選択してください");
        }
        d->name=p["sessionName"].toString(d->displayName+" のセッション").left(80);d->maxPeers=p["maxPeers"].toInt(8);if(d->maxPeers<2||d->maxPeers>64)return reject("参加人数は2〜64人で指定してください");
        bool deviceOk=false;d->programDevice=p["programDevice"].toString().toInt(&deviceOk);if(!deviceOk)d->programDevice=-1;
        if(d->hosting){d->room=secureRandomHex(16);d->token=secureRandomToken(24);d->recovery=secureRandomToken(32);d->inviteExpiry=QDateTime::currentMSecsSinceEpoch()+3600000;d->auth.sessionId=secureRandomHex(16);d->adopt=p["adoptCurrent"].toBool();}
        else{d->auth.sessionId="pending";d->audible.store(false);}
        const auto failure=d->connect(d->hosting);if(!failure.isEmpty()){d->stop();return reject(failure);}return snapshot();
    }
    if(!active())return reject("セッションに参加していません");
    if(op=="profile.update"){
        const auto djName=sanitizeDisplayName(p["djName"].toString(p["displayName"].toString()));if(djName.isEmpty())return reject("DJ名を入力してください");
        auto avatar=d->avatarDataUrl;if(p.contains("avatarDataUrl")){const auto requestedAvatar=p["avatarDataUrl"].toString();avatar=profileAvatar(requestedAvatar);if(!requestedAvatar.isEmpty()&&avatar.isEmpty())return reject("アイコン画像は4,096文字以下のPNG・JPEG・WebPを指定してください");}
        d->displayName=djName;d->avatarDataUrl=avatar;d->themeColor=profileColor(p["themeColor"].toString(d->themeColor),d->auth.local);++d->auth.revision;
        if(d->hosting){d->broadcast("peer.hello",d->localProfile());d->broadcast("session.snapshot",d->wireState(true));}else{auto host=d->peers.find(d->auth.host);if(host!=d->peers.end())d->queue(*host->second,"peer.hello",d->localProfile());}
        return snapshot();
    }
    if(op=="roster.reorder"){
        if(!d->hosting)return reject("セッション管理者だけがDJの順番を変更できます");if(!p["peerIds"].isArray())return reject("DJの順番を指定してください");
        QStringList requested;QSet<QString> unique;for(const auto& value:p["peerIds"].toArray()){const auto id=value.toString();if(!validOpaqueId(id)||unique.contains(id))return reject("DJの順番に重複または不正な項目があります");unique.insert(id);requested.append(id);}
        QSet<QString> expected;expected.insert(d->auth.local);for(const auto& [id,peer]:d->peers){Q_UNUSED(peer);expected.insert(id);}if(unique!=expected)return reject("DJ一覧が更新されています。最新の一覧でもう一度並び替えてください");
        if(d->lifecycle=="starting")return reject("開始準備中はDJの順番を変更できません");
        if(d->lifecycle=="live"){QStringList fixed=d->finishedOrder;if(!d->auth.owner.isEmpty())fixed.append(d->auth.owner);for(const auto& id:fixed)if(requested.indexOf(id)!=d->rosterOrder.indexOf(id))return reject("演奏中・演奏済みのDJは移動できません。待機中のDJだけ並び替えてください");}
        d->rosterOrder=requested;++d->auth.revision;d->broadcast("session.snapshot",d->wireState());return snapshot();
    }
    if(op=="session.start"){
        if(!d->hosting)return reject("セッション管理者だけが開始できます");if(d->lifecycle!="lobby")return reject("このセッションはすでに開始しています");
        auto target=p["performerPeerId"].toString(p["targetPeerId"].toString());if(target.isEmpty())for(const auto& id:d->rosterOrder){if(id==d->auth.local){target=id;break;}auto peer=d->peers.find(id);if(peer!=d->peers.end()&&peer->second->approved&&peer->second->hello){target=id;break;}}
        if(target.isEmpty())return reject("最初にプレイするDJを選択してください");auto peer=d->peers.find(target);if(target!=d->auth.local&&(peer==d->peers.end()||!peer->second->approved||!peer->second->hello))return reject("最初のDJとの接続が完了していません");
        // Validate the venue output before mutating the shared order: a refused start leaves the lobby as it was.
        if(d->programDevice<0)return reject("会場への音声出力が未選択です。「セッション設定」の「会場への音声出力」で出力先を反映してから開始してください");
        d->openProgram();if(d->programState=="error")return reject(d->problem.isEmpty()?QStringLiteral("会場の音声出力を開けません"):d->problem);
        if(target!=d->auth.local&&peer!=d->peers.end()&&peer->second->lite){d->lifecycle="live";d->selectLiteOwner(target);return snapshot();}
        const auto previousOrder=d->rosterOrder;const auto previousFinished=d->finishedOrder;const auto previousRequests=d->turnRequests;
        d->rosterOrder.removeAll(target);d->rosterOrder.prepend(target);d->finishedOrder.clear();d->turnRequests.remove(target);d->problem.clear();d->reasons.clear();
        if(target==d->auth.local){d->captureEnabled.store(true);d->tap.enable(false,true);d->lifecycle="live";++d->auth.revision;d->broadcast("session.snapshot",d->wireState());return snapshot();}
        // A remote first DJ starts from their own prepared decks. This is a
        // bootstrap, not a host-to-guest graph handoff: only their stream and
        // synchronized clock are gated before ownership becomes live.
        d->captureEnabled.store(false);d->tap.enable(false,false);peer->second->remoteStreamReady=false;d->lifecycle="starting";d->bootstrapStart=true;auto failure=d->auth.prepare(target);if(!failure.isEmpty()){d->restoreLobby();d->rosterOrder=previousOrder;d->finishedOrder=previousFinished;d->turnRequests=previousRequests;return reject(failure);}d->startDeadline=monotonicNanos()+120000000000LL;d->resetPreparation();d->broadcast("handoff.prepare",{{"targetPeerId",target},{"handoffId",d->auth.handoffId},{"bootstrap",true}});d->broadcast("session.snapshot",d->wireState());return snapshot();
    }
    if(op.startsWith("private.")){const auto failure=d->backend->privatePreviewCommand(op,p);if(!failure.isEmpty())return reject(failure);return snapshot();}
    if(op=="leave"||op=="end"){
        if(op=="leave"&&d->auth.owner==d->auth.local&&d->peers.size()>0)return reject("演奏を引き継いでから退出してください");
        if(op=="end"&&!d->hosting)return reject("ホストだけがセッションを終了できます");
        if(!d->endingAt){d->endingHost=d->hosting;d->endingAt=monotonicNanos()+(d->hosting?2000000000LL:200000000LL);d->connection="closing";d->broadcast(d->hosting?"session.end":"peer.leave",{});}return snapshot();
    }
    if(op=="peer.approve"){
        if(!d->hosting)return reject("ホストだけが参加を承認できます");auto i=d->peers.find(p["peerId"].toString());if(i==d->peers.end())return reject("参加希望が見つかりません");bool accept=p["accept"].toBool(true);d->signalSend({{"type","host.join_decision"},{"guestPeerId",i->first},{"accept",accept}});if(accept){i->second->approved=true;d->makePeer(*i->second,true);}else{d->rosterOrder.removeAll(i->first);d->finishedOrder.removeAll(i->first);d->turnRequests.remove(i->first);d->peers.erase(i);}++d->auth.revision;return snapshot();
    }
    if(op=="invite.rotate") {if(!d->hosting)return reject("ホストだけが招待を更新できます");d->token=secureRandomToken(24);d->inviteExpiry=QDateTime::currentMSecsSinceEpoch()+3600000;d->signalSend({{"type","host.rotate_invite"},{"inviteTokenHash",sha256Hex(d->token.toUtf8())},{"inviteExpiresAt",double(d->inviteExpiry)}});d->updateInvite();return snapshot();}
    if(op=="program.configure") {if(!d->hosting)return reject("配信出力はホストが設定します");bool ok;int device=p["programDevice"].toString().toInt(&ok);if(!ok)return reject("配信先デバイスを選択してください");d->program.close();d->programOpened=false;d->programState="idle";d->programDevice=device;if(d->lifecycle!="lobby")d->openProgram();if(p.contains("gain"))d->program.setGain(float(p["gain"].toDouble()));return snapshot();}
    if(op=="program.record.start"||op=="program.record.stop") {if(!d->hosting)return reject("配信録音はホストが操作します");QString failure;bool ok=op.endsWith("start")?d->program.startRecording(p["path"].toString(),&failure):d->program.stopRecording(&failure);if(!ok)return reject(failure);return snapshot();}
    if(op=="recovery.resume"){
        if(!d->hosting||d->auth.phase!="recovery")return reject("ホストが復旧中に操作できます");
        d->recoveryResumeFrame=d->now()+24000;d->recoveryEpoch=std::max(d->auth.epoch,d->auth.committed?d->auth.committed->newEpoch:quint64(0))+1;
        QSaveFile record(QStandardPaths::writableLocation(QStandardPaths::CacheLocation)+"/junction/"+d->auth.sessionId+".recovery");
        const auto bytes=json({{"sessionId",d->auth.sessionId},{"epoch",u64(d->recoveryEpoch)},{"frame",u64(d->recoveryResumeFrame)},{"owner",d->auth.host}});
        if(!record.open(QIODevice::WriteOnly)||record.write(bytes)!=bytes.size()||!record.flush()||platform_file::sync(record.handle())!=0||!record.commit()){d->recoveryResumeFrame=0;return reject("復旧状態を保存できません");}
        setCaptureAnchor(d->lastCaptureSourceEnd.load(),d->now());d->scheduledEpoch.store(d->recoveryEpoch);d->scheduledFrame.store(d->recoveryResumeFrame);d->captureEnabled.store(true);d->tap.enable(false,true);
        auto it=d->programPending.lower_bound(d->recoveryResumeFrame);d->programPending.erase(it,d->programPending.end());
        d->broadcast("session.recovery",{{"stage","scheduled"},{"reason","ホストの手元の演奏へ切り替えます"},{"frame",u64(d->recoveryResumeFrame)},{"epoch",u64(d->recoveryEpoch)}});return snapshot();
    }
    if(op=="handoff.request") {if(d->lifecycle!="live")return reject("最初のDJを選び、セッションを開始してください");auto target=p["targetPeerId"].toString(d->auth.local);if(d->hosting){if(!d->releasingPeer.isEmpty()&&target!=d->auth.owner)return reject("JUNCTIONデッキを解放してから次のDJへ交代してください");auto peer=d->peers.find(target);if(target!=d->auth.local&&(peer==d->peers.end()||!peer->second->approved))return reject("承認済みの参加者を選択してください");const auto current=d->peers.find(d->auth.owner);if((peer!=d->peers.end()&&peer->second->lite)||(current!=d->peers.end()&&current->second->lite)){d->selectLiteOwner(target);return snapshot();}auto failure=d->auth.prepare(target);if(!failure.isEmpty())return reject(failure);d->turnRequests.remove(target);d->finishedOrder.removeAll(target);d->rosterOrder.removeAll(target);const int ownerIndex=d->rosterOrder.indexOf(d->auth.owner);d->rosterOrder.insert(ownerIndex<0?0:ownerIndex+1,target);d->resetPreparation();d->broadcast("handoff.prepare",{{"targetPeerId",target},{"handoffId",d->auth.handoffId}});if(d->auth.owner==d->auth.local)d->startExport();}else{auto i=d->peers.find(d->auth.host);if(i==d->peers.end())return reject("ホストに接続していません");if(i->second->lite)d->sendLite(*i->second,{{"type","handoff-request"}});else d->queue(*i->second,"handoff.request",{});}return snapshot();}
    if(op=="handoff.cancel") {if(!d->hosting)return reject("ホストに取り消しを依頼してください");const auto handoffId=d->auth.handoffId;auto failure=d->auth.cancel();if(!failure.isEmpty())return reject(failure);d->broadcast("handoff.cancel",{{"handoffId",handoffId},{"reason","cancelled"}});if(d->lifecycle=="starting")d->restoreLobby();return snapshot();}
    if(op=="handoff.accept"){
        if(!d->hosting){auto h=d->peers.find(d->auth.host);if(h==d->peers.end())return reject("ホストに接続していません");d->queue(*h->second,"handoff.ready",{{"requestFence",true},{"handoffId",d->auth.handoffId}});return snapshot();}
        if(d->auth.phase!="preparing"||!d->ready)return reject("引き継ぎの準備を待っています");
        auto frame=d->now()+4800;d->auth.fence(frame,d->controlSeq);d->fenceDeadline=monotonicNanos()+6000000000LL;d->finalCheckpoint=false;d->validationStart=0;d->broadcast("handoff.fence",{{"frame",u64(frame)},{"handoffId",d->auth.handoffId}});return snapshot();
    }
    return reject("対応していないセッション操作です");
}
}
