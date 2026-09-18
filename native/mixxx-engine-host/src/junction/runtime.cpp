#include "runtime.h"
#include "media_transport.h"
#include "program_output.h"
#include "junction_input.h"
#include "program_mixer.h"
#include "manual_exchange.h"
#include "ice_servers.h"
#include "network_settings.h"
#include "turn_state.h"
#include "../backend.h"
#include <QJsonDocument>
#include <QDebug>
#include <QUrl>
#include <QUrlQuery>
#include <QRegularExpression>
#include <QTemporaryDir>
#include <QDateTime>
#include <QtEndian>
#ifdef __APPLE__
#include <IOKit/pwr_mgt/IOPMLib.h>
#endif
#include <QPointer>
#include <QQueue>
#include <algorithm>
#include <cmath>
#include <array>
#include <deque>
#include <map>
#include <vector>
#if defined(PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL)
#include <rtc/rtc.hpp>
#endif
#ifdef _WIN32
// SetThreadExecutionState keeps the machine awake during a Junction session.
// Lean and after rtc.hpp, so winsock2.h is never preceded by the old winsock.h.
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#endif
namespace junction {
namespace {
// Desktop screen sharing, power management and a busy audio engine can all
// pause delivery for several seconds. A missing heartbeat is a degradation,
// not permission to destroy a connection which WebRTC may still recover.
constexpr qint64 kControlSilenceNanos=10000000000LL;
constexpr quint64 kSnapshotIntervalTicks=100; // 500 ms at the 5 ms session tick.
QByteArray json(const QJsonObject& v) {return QJsonDocument(v).toJson(QJsonDocument::Compact);}
// This identifies the minimum wire-compatible Junction protocol, not the app
// release. Additive features are negotiated independently in peer.hello so a
// newer plumdeck can still connect to an older compatible release.
QString fingerprint() {return QStringLiteral("mixxx-3ebac449e7e5fe2a0186596657696e87ce8b0e56-junction-5");}
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
    ProducerTap tap;ProgramOutput program;
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
    struct Peer {QString id,name,fp,avatarDataUrl,themeColor;bool approved=false,hello=false,faderStartV1=false,producing=false,endingAck=false,lite=false;bool relaying=false;QString liteSdp,liteSdpType;QJsonArray liteDecks;qint64 liteDecksAt=0;std::unique_ptr<MediaTransport> transport;QQueue<PendingControl> pending;
        qint64 lastControlAt=0,healthAt=0,bulkDisconnectedAt=0;quint64 serial=0,candidateSerial=0;std::unique_ptr<MediaTransport> candidate,retiring; qint64 retireAt=0;ManualAttempt manual;HealthReportMessage health;bool hasHealth=false;
        MediaTransport::Statistics healthStats;bool healthStatsReady=false,probeReady=false;double lastProbeRttMs=0,smoothedRttMs=0,smoothedJitterMs=0;};
    std::map<QString,std::unique_ptr<Peer>> peers;
    // --- Fader-start turns -------------------------------------------------
    // Roles stay in Authority (owner, next) and releasingPeer (OUTGOING); the
    // timetable is rosterOrder with finishedOrder out of the queue. Everything
    // below is measurement or one-shot bookkeeping, never a second role.
    bool turnRepeat=false,autoFailover=false;
    turn::FaderStart faderStart;turn::TailEnded tailEnded;
    QSet<int> tailDecks;bool tailApplied=false;
    /// Guest OUTGOING: capture sends the masked LOCAL NEXT bus, not master.
    std::atomic<bool> tailSend{false};
    /// Local edge seen; the anchored capture block has not measured it yet.
    bool onAirPending=false;qint64 onAirArmedAt=0;
    /// The ON AIR stream is accepted from this capture generation on.
    quint64 ownerMinGeneration=0;
    qint64 standbyAudioAt=0,relayAudioAt=0,relayHoldSince=0,underrunAt=0,junctionSince=0,decksSentAt=0,reportSentAt=0;quint64 lastUnderruns=0;
    struct TurnReport {bool ready=false;QString status;qint64 at=0;};TurnReport nextReport;
    QJsonObject hostTurn; // guest: the host's published turn state
    turn::Blocker localBlocker=turn::Blocker::None;bool localReady=false,localAudible=false;
    double pathLatencyMs=-1;QString lastReportSignature;
    QString cueKind,cueFrom;qint64 cueAt=0;
    struct SeamReport {quint64 frame=0,generation=0;};
#if defined(PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL)
    std::shared_ptr<rtc::WebSocket> signal;
#endif
    QTimer timer;quint64 ticks=0;QString origin,room,name,displayName,avatarDataUrl,themeColor,token,recovery,invite,problem,connection="disconnected",programState="idle";
    QStringList iceServers;bool iceReady=false;QQueue<QJsonObject> deferredSignals;QJsonObject pendingInvite;QJsonArray participantRoster;
    QString lifecycle=QStringLiteral("live");QStringList rosterOrder,finishedOrder;
    bool hosting=false,adopt=false,programOpened=false,liteSession=false;
    // Venue delay. Fixed for the whole session so the venue timeline never
    // jumps; one second covers a guest receiver's J relay plus its standby
    // stream back to the host (READY checks the measured path against half).
    quint32 delay=48000;int programDevice=-1,maxPeers=8;
    qint64 inviteExpiry=0,turnRefreshAt=0,reconnectAt=0,endingAt=0;bool endingHost=false;unsigned reconnectAttempts=0;quint64 signalGeneration=0;
    struct ProgramBlock {PcmBlockInfo info;std::vector<float> samples;};
    std::map<quint64,ProgramBlock> programPending;
    quint64 programEnqueuedThrough=0; qint64 ownerAudioAt=0;quint64 recoveryResumeFrame=0,recoveryEpoch=0;
    // --- JUNCTION deck: another DJ's audio as a local mixer channel --------
    // The previous Lite owner keeps sounding here after an operator switch
    // until the new operator fades it out (`releasingPeer`).
    JunctionInput input;QString inputPeer,releasingPeer;InputRelease inputRelease;
    // Mirrors what this runtime asked the backend: JUNCTION MASTER in main.
    bool inputMainMix=false;
    QSet<QString> liteSenders; // Lite guest: peers the host still wants to hear.
    // Each Lite connection owns one outbound SPSC ring for its entire session.
    // A ring is never reassigned to another transport worker or freed while
    // the audio callback may still hold its pointer.
    std::map<QString,std::unique_ptr<PcmRing>> liteReturnRings;
    std::atomic<PcmRing*> localReturnTarget{nullptr};
    std::atomic<unsigned> localReturnReaders{0};
    std::atomic<quint64> returnEpoch{0},returnGeneration{1},returnSequence{0};
    QString returnRelaySource,returnRelayTarget,returnTargetPeer;
    // The Lite->Mac seam. `seam` names the outgoing DJ whose direct stream must
    // keep reaching Program; `seamFrame` is the boundary it reaches, measured
    // by the first captured block instead of guessed from the clock, and 0
    // until that block lands. `takeoverAnchor` arms that one measurement.
    struct InputSeam {QString oldOwner;quint64 oldEpoch=0;};std::optional<InputSeam> seam;
    struct LaneBucket {float peak=0;QString deck;};std::deque<LaneBucket> lane;float lanePeak=0;quint32 laneFrames=0;

    std::atomic<bool> captureEnabled{false},separateLocalMaster{true};
    std::atomic<quint64> lastCaptureSourceEnd{0};
    std::atomic<quint64> blockSequence{0},captureEpoch{1},sourceAnchor{UINT64_MAX},mediaAnchor{0};
    std::atomic<qint64> timelineAnchor{0};
    std::atomic<quint64> pendingAnchorSource{0},pendingAnchorMedia{0},pendingAnchorRevision{0};
    quint64 appliedAnchorRevision=0;
    std::atomic<quint64> captureGeneration{1},scheduledEpoch{0},scheduledFrame{UINT64_MAX},seamFrame{0};
    std::atomic<bool> takeoverAnchor{false};
    std::array<float,8192> pcm{};
    // --- manual (signalling-free) exchange -------------------------------
    bool manual=false;QByteArray hostCertificatePem;QString pinnedHostFingerprint;
    quint64 serialCounter=0;
    QString manualDetail,manualErrorCode;
    std::unique_ptr<MediaTransport> networkProbe;
    QJsonObject networkTestResult;
    qint64 networkTestDeadline=0;
    NetworkSettings network;
    explicit Impl(Runtime* owner,PlaybackBackend* b):q(owner),backend(b) {
        timer.setInterval(5);QObject::connect(&timer,&QTimer::timeout,q,[this]{tick();});timer.start();
        probeTimer.setInterval(10);QObject::connect(&probeTimer,&QTimer::timeout,q,[this]{probeTick();});
    }
    // Audio probe (PLUMDECK_JUNCTION_AUDIO_PROBE only, never during a session):
    // a synthetic JUNCTION MASTER written through the same JunctionInput a
    // decoded P2P stream uses, so tests measure the real engine path.
    QTimer probeTimer;qint64 probeStart=0;quint64 probeWritten=0,probeSequence=0;double probeFrequency=440,probeAmplitude=0;
    void probeTick() {
        if(!auth.sessionId.isEmpty()){probeTimer.stop();resetInput();return;}
        const auto elapsed=monotonicNanos()-probeStart;
        const quint64 due=elapsed>0?quint64(elapsed/1000)*kWireSampleRate/1000000:0;
        std::array<float,1920> block;
        while(probeWritten+960<=due){
            for(size_t i=0;i<960;++i){const auto s=float(probeAmplitude*std::sin(2*3.141592653589793*probeFrequency*double(probeWritten+i)/kWireSampleRate));block[i*2]=s;block[i*2+1]=s;}
            PcmBlockInfo info;info.epoch=1;info.generation=1;info.mediaFrame=probeWritten;info.sourceFrame=probeWritten;info.sequence=probeSequence++;info.frameCount=960;info.sampleRateHz=kWireSampleRate;info.channels=2;
            input.write(block.data(),info);probeWritten+=960;
        }
    }
    quint64 now() const {return hosting?timeline.now():timeline.frameAt(clock.toHostNanos(monotonicNanos()));}
    template<class F> void post(F fn) {QMetaObject::invokeMethod(q,std::move(fn),Qt::QueuedConnection);}
    void fail(const QString& text) {if(qEnvironmentVariableIsSet("PLUMDECK_JUNCTION_TRACE"))qWarning()<<"junction failure"<<hosting<<text;problem=text;}
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
    QJsonObject localProfile() const {
        return {{"fingerprint",fingerprint()},{"displayName",displayName},{"djName",displayName},{"avatarDataUrl",avatarDataUrl},{"themeColor",themeColor},{"junctionCapabilities",QJsonArray{QString::fromLatin1(turn::kCapability)}}};
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
        if(lifecycle=="live"&&!releasingPeer.isEmpty()&&id==releasingPeer)return QStringLiteral("outgoing");
        if(id==auth.next){
            const bool nextReady=id==auth.local?localReady:hosting?nextReport.ready&&monotonicNanos()-nextReport.at<3000000000LL:rosterMetadata(id)["rosterStatus"].toString()=="ready";
            return nextReady?QStringLiteral("ready"):QStringLiteral("next");
        }
        if(finishedOrder.contains(id))return QStringLiteral("finished");
        if(!hosting&&rosterMetadata(id)["rosterStatus"].toString()=="finished")return QStringLiteral("finished");
        if(local)return connection=="connected"?QStringLiteral("waiting"):connection;
        // A Lite peer connects through liteSdp/hello; its idle manual exchange
        // must not hide a connected DJ from the coordinator's candidates.
        if(manual&&peer&&!peer->lite){
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
        if(peer&&!peer->manual.inviteId.isEmpty())row["invitationId"]=peer->manual.inviteId;
        row["rosterStatus"]=rosterStatus(id,peer,local);
        auto measured=quality(peer,local);if(!hosting&&!local&&peer&&!peer->hasHealth&&synced["connectionQuality"].isObject())measured=synced["connectionQuality"].toObject();
        row["connectionQuality"]=measured;
    }
    /// `wire` strips everything a remote peer must not see. Exchange packets
    /// carry another DJ's invite/response/notice text and never go on the wire.
    QJsonObject publicState(bool wire=false) const {
        std::vector<QJsonObject> rows;const bool isLive=lifecycle=="live";
        if(!auth.local.isEmpty()){QJsonObject row{{"peerId",auth.local},{"approved",true},{"isHost",auth.local==auth.host},{"isPerformer",isLive&&auth.owner==auth.local},{"isNextUp",auth.next==auth.local},{"status",connection}};enrichRosterRow(row,auth.local,nullptr,true);rows.push_back(row);}
        for(const auto& [id,p]:peers){
            QJsonObject row{{"peerId",id},{"approved",p->approved},{"isHost",id==auth.host},{"isPerformer",isLive&&id==auth.owner},{"isNextUp",id==auth.next},{"status",p->hello?"connected":p->approved?"connecting":"pending"}};enrichRosterRow(row,id,p.get());
            // A Lite peer connects through liteSdp/hello, never the manual
            // packet exchange, so its idle exchange state must not be shown.
            if(manual&&!wire&&!p->lite)row["exchange"]=manualExchangeJson(*p);
            if(p->lite)row["client"]="lite";
            rows.push_back(row);
        }
        if(!hosting)for(const auto& value:participantRoster){const auto row=value.toObject();const auto id=row["peerId"].toString();if(!validOpaqueId(id)||id==auth.local||peers.count(id))continue;
            QJsonObject copy{{"peerId",id},{"displayName",sanitizeDisplayName(row["displayName"].toString())},{"djName",sanitizeDisplayName(row["djName"].toString(row["displayName"].toString()))},{"avatarDataUrl",profileAvatar(row["avatarDataUrl"].toString())},{"themeColor",profileColor(row["themeColor"].toString(),id)},{"approved",row["approved"].toBool()},{"isHost",id==auth.host},{"isPerformer",isLive&&id==auth.owner},{"isNextUp",id==auth.next},{"status",row["status"].toString()},{"slotId",id},{"orderIndex",row["orderIndex"].toInt(999)},{"rosterStatus",row["rosterStatus"].toString()},{"connectionQuality",row["connectionQuality"].toObject()},{"isPlaceholder",row["isPlaceholder"].toBool()}};
            if(row["invitationId"].isString())copy["invitationId"]=row["invitationId"];
            rows.push_back(copy);
        }
        std::stable_sort(rows.begin(),rows.end(),[](const QJsonObject& a,const QJsonObject& b){return a["orderIndex"].toInt(999)<b["orderIndex"].toInt(999);});
        QJsonArray participants;for(const auto& row:rows)participants.append(row);
        QJsonObject state{{"active",!auth.sessionId.isEmpty()},{"sessionId",auth.sessionId},{"localPeerId",auth.local},{"hostPeerId",auth.host},{"coordinatorPeerId",auth.host},{"performerPeerId",isLive?auth.owner:QString{}},{"nextPeerId",auth.next},{"epoch",u64(auth.epoch)},{"revision",double(auth.revision)},{"sessionName",name},{"lifecycle",lifecycle},{"handoffState",auth.phase},{"participants",participants},{"readiness",QJsonObject{{"ready",localReady},{"reasons",localBlocker!=turn::Blocker::None?QJsonArray{turn::blockerText(localBlocker)}:QJsonArray{}}}},{"turn",turnJson(wire)},{"connection",QJsonObject{{"state",connection},{"detail",problem}}},{"program",QJsonObject{{"state",programState},{"captureActive",captureEnabled.load()},{"outputDevice",QString::number(programDevice)},{"recording",program.recording()},{"underruns",u64(program.underruns())},{"meter",double(program.peak())},{"rms",double(program.rms())},{"sampleRateHz",int(program.sampleRate())},{"deviceLatencySeconds",program.deviceLatencySeconds()}}},{"invite",hosting?invite:QString{}},{"privatePreview",backend->privatePreviewState()},{"exchange",wire?QJsonObject{{"mode",manual?QStringLiteral("manual"):QStringLiteral("server")}}:exchangeState()}};
        // Junction Live cache paths are local-only. Only the coordinator while
        // another peer performs needs the presentation pair.
        if(!wire){state["localPrep"]=localPrep();state["junctionInput"]=inputState();
            state["operatorPeerId"]=isLive?auth.owner:QString{};
            state["programMixer"]=programMixer().toJson(inputPeer,releasingPeer,returnRelayTarget.isEmpty()?returnTargetPeer:returnRelayTarget);}
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
        else if(type=="peer.gone") {const auto id=m["peerId"].toString();if(id==auth.owner)ownerLost("プレイ担当者との接続が切れました");peers.erase(id);rosterOrder.removeAll(id);finishedOrder.removeAll(id);++auth.revision;}
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
        if(!auth.next.isEmpty()&&auth.next!=auth.owner)senders.append(auth.next);
        return {{"type","owner"},{"ownerPeerId",auth.owner},{"senderPeerIds",senders},{"turn",turnJson(true)},{"capabilities",QJsonArray{QString::fromLatin1(turn::kCapability)}}};
    }
    PcmRing* returnRing(const QString& id) {
        auto& slot=liteReturnRings[id];if(!slot)slot=std::make_unique<PcmRing>(128,4096,2);return slot.get();
    }
    void setInputMainMix(bool enabled) {inputMainMix=enabled;if(backend)backend->junctionInputMainMix(enabled);}
    /// Program Master routing for this computer. The optional arguments decide
    /// a return that is about to start instead of the one currently running.
    ProgramMixerRoute programMixer(std::optional<bool> returnRequested={},std::optional<bool> relayed={}) const {
        ProgramMixerInputs in;
        in.hosting=hosting;in.programOpen=programOpened;in.localOperator=!auth.sessionId.isEmpty()&&auth.owner==auth.local;
        in.junctionMasterPresent=!inputPeer.isEmpty();in.junctionMasterInMain=inputMainMix;in.releasing=!releasingPeer.isEmpty();
        in.returnRequested=returnRequested.value_or(localReturnTarget.load(std::memory_order_acquire)!=nullptr||!returnRelayTarget.isEmpty());
        in.returnRelayed=relayed.value_or(!returnRelaySource.isEmpty());
        in.localReturnBus=backend&&backend->junctionLocalReturnBus();
        return ProgramMixerRoute::resolve(in);
    }
    void stopLiteReturns() {
        localReturnTarget.store(nullptr,std::memory_order_release);returnRelaySource.clear();returnRelayTarget.clear();returnTargetPeer.clear();relayAudioAt=0;
        if(!hosting)return;
        for(auto& [id,p]:peers){Q_UNUSED(id);if(!p->transport)continue;if(p->lite)p->transport->startProducer(nullptr);else if(p->relaying){p->transport->startProducer(nullptr);p->relaying=false;}}
    }
    QString returnSource() const {return !returnRelaySource.isEmpty()?returnRelaySource:!returnTargetPeer.isEmpty()?auth.local:QString{};}
    QString returnTarget() const {return !returnRelayTarget.isEmpty()?returnRelayTarget:returnTargetPeer;}
    /// Host: J for a remote receiver. `previous` is whose sound the receiver
    /// mixes (this computer's LOCAL NEXT bus, or another DJ's stream relayed
    /// as it arrives); `target` is the receiver. A running feed between the
    /// same two DJs is kept as is, so the receiver's J never restarts at the
    /// moment the turn changes.
    void startLiteReturn(const QString& previous,const QString& target) {
        if(hosting&&!target.isEmpty()&&returnTarget()==target&&returnSource()==previous)return;
        stopLiteReturns();
        if(!hosting||target==auth.local||previous.isEmpty()||previous==target)return;
        auto found=peers.find(target);if(found==peers.end()||!found->second->transport)return;
        auto* ring=returnRing(target);const auto generation=returnGeneration.fetch_add(1,std::memory_order_relaxed)+1;
        StreamManifest manifest{secureRandomHex(12),auth.local,auth.epoch,generation,0,1,0};auto random=secureRandomBytes(8);
        manifest.ssrc=qFromBigEndian<quint32>(reinterpret_cast<const uchar*>(random.constData()));if(!manifest.ssrc)manifest.ssrc=1;
        manifest.rtpTimestampOrigin=qFromBigEndian<quint32>(reinterpret_cast<const uchar*>(random.constData()+4));
        if(!found->second->lite){found->second->transport->enableAutomaticManifest(true);found->second->relaying=true;}
        found->second->transport->setSendManifest(manifest);found->second->transport->restartProducer(ring);
        returnEpoch.store(auth.epoch,std::memory_order_relaxed);
        // Only this computer's local play is returned, never JUNCTION MASTER.
        if(previous==auth.local){if(programMixer(true,false).localReturnAllowed()){localReturnTarget.store(ring,std::memory_order_release);returnTargetPeer=target;}}
        else if(peers.count(previous)){returnRelaySource=previous;returnRelayTarget=target;}
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
    bool localPrep() const {return !auth.sessionId.isEmpty()&&auth.owner!=auth.local&&!localSending();}
    /// An outgoing Lite DJ that the host still wants to hear after the switch.
    bool localSending() const {
        if(auth.owner==auth.local)return false;
        // A browser guest learns this from the host's senderPeerIds. The
        // desktop host has the same obligation when its own master is the
        // outgoing source being returned to the new Lite operator.
        return (liteSession&&!hosting&&liteSenders.contains(auth.local))
            || (!releasingPeer.isEmpty()&&releasingPeer==auth.local);
    }
    /// Whose sound this computer's J carries: the ON AIR DJ while this DJ is
    /// next (STANDBY/READY), then that DJ's tail after this DJ went on air. A
    /// guest always hears it through the host.
    QString junctionInputPeer() const {
        if(auth.sessionId.isEmpty()||lifecycle!="live")return {};
        const bool tail=auth.owner==auth.local&&!releasingPeer.isEmpty()&&releasingPeer!=auth.local;
        const bool standby=auth.next==auth.local&&!auth.owner.isEmpty()&&auth.owner!=auth.local&&releasingPeer.isEmpty();
        if(!hosting)return tail||standby?auth.host:QString{};
        return tail?releasingPeer:standby?auth.owner:QString{};
    }
    QString releaseRequested;qint64 releaseRequestedAt=0;
    void releaseInput() {
        if(releasingPeer.isEmpty())return;
        const auto released=releasingPeer;
        if(!hosting){
            // The host ends the tail for everyone; this receiver asks for it.
            auto host=peers.find(auth.host);
            if(host!=peers.end()){if(host->second->lite)sendLite(*host->second,{{"type","input-released"},{"senderPeerId",released}});else queue(*host->second,"turn",{{"kind","release"}});}
            releaseRequested=released;releaseRequestedAt=monotonicNanos();
            releasingPeer.clear();inputRelease.clear();if(tailApplied)applyTail(false);++auth.revision;
            if(auth.next!=auth.local)setInputMainMix(false);
            return;
        }
        stopLiteReturns();
        releasingPeer.clear();inputRelease.clear();relayHoldSince=0;++auth.revision;
        if(released==auth.local)applyTail(false);
        if(auth.next!=auth.local)setInputMainMix(false);
        sendLiteOwner();broadcast("session.snapshot",wireState(false));
    }
    /// OUTGOING: the decks on Program at the turn change keep sounding on the
    /// new DJ's J, alone, at a fixed level and tempo. Everything else here is
    /// local preparation again.
    void applyTail(bool on) {
        if(on==tailApplied)return;
        tailApplied=on;tailEnded.reset();tailDecks.clear();
        if(on){
            QList<int> decks;
            if(q->localDeckTracks)for(const auto& value:q->localDeckTracks()){const auto deck=value.toObject();const int index=turn::TailLock::deckIndex(deck["deck"].toString());if(index>=0&&deck["playing"].toBool()&&deck["audibility"].toDouble()>.001){tailDecks.insert(index);decks.append(index);}}
            if(decks.isEmpty())decks.append(-1);
            if(backend)backend->junctionTail(decks);
            tailSend.store(!hosting,std::memory_order_release);
        }else{if(backend)backend->junctionTail({});tailSend.store(false,std::memory_order_release);}
        auth.tailDecks=tailDecks;++auth.revision;
    }
    QString audibleDeck(const Peer& p) const {
        if(p.liteDecks.isEmpty()||monotonicNanos()-p.liteDecksAt>3000000000LL)return {};
        QString best;double loudest=.001;
        for(const auto& value:p.liteDecks){const auto deck=value.toObject();const double level=deck["audibility"].toDouble()*(deck["playing"].toBool()?1:.5);if(level>loudest){loudest=level;best=deck["deck"].toString();}}
        return best;
    }
    static QJsonArray sanitizeDecks(const QJsonArray& input) {
        QJsonArray decks;
        for(const auto& value:input){if(decks.size()>=4)break;const auto deck=value.toObject();const auto name=deck["deck"].toString();if(name!="A"&&name!="B"&&name!="C"&&name!="D")continue;
            const auto number=[&](const char* key,double low,double high){const double v=deck[key].toDouble();return std::isfinite(v)?std::clamp(v,low,high):0.0;};
            QJsonObject row{{"deck",name},{"role",deck["role"].toString()=="current"?"current":"next"},{"title",deck["title"].toString().left(200)},{"artist",deck["artist"].toString().left(200)},
                {"bpm",number("bpm",0,999)},{"positionMs",number("positionMs",0,86400000)},{"durationMs",number("durationMs",0,86400000)},{"rate",number("rate",0,4)},{"audibility",number("audibility",0,1)},{"playing",deck["playing"].toBool()},{"beatsPerBar",std::clamp(deck["beatsPerBar"].toInt(4),1,16)}};
            if(deck["firstBeatMs"].isDouble())row["firstBeatMs"]=number("firstBeatMs",-60000,86400000);
            decks.append(row);}
        return decks;
    }
    /// This computer's decks as J metadata for the next DJ: never a path,
    /// never a file.
    QJsonArray localDecksJson() const {
        QJsonArray rows;if(!q->localDeckTracks)return rows;
        const auto tracks=q->localDeckTracks();QString current;double loudest=.001;
        for(const auto& value:tracks){const auto deck=value.toObject();const double level=deck["audibility"].toDouble()*(deck["playing"].toBool()?1:.5);if(level>loudest){loudest=level;current=deck["deck"].toString();}}
        for(const auto& value:tracks){auto deck=value.toObject();deck.remove("pfl");deck["role"]=deck["deck"].toString()==current?"current":"next";rows.append(deck);}
        return sanitizeDecks(rows);
    }
    QJsonArray displayedLiteDecks(const Peer& p,double lagMs=0) const {
        const auto ageNanos=monotonicNanos()-p.liteDecksAt;
        if(p.liteDecks.isEmpty()||ageNanos<0||ageNanos>3000000000LL)return {};
        // What J renders now left the other DJ's decks `lagMs` ago.
        const double elapsedMs=double(ageNanos)/1000000.0-lagMs;
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
            double lagMs=0;if(const auto rendered=input.renderedMediaFrame()){const auto current=now();if(current>*rendered)lagMs=double(current-*rendered)/48.0;}
            state["lagMs"]=std::round(lagMs);
            const auto decks=displayedLiteDecks(*source->second,lagMs);if(!decks.isEmpty())state["decks"]=decks;
            state["audibleDeck"]=audibleDeck(*source->second);
        }
        QJsonArray peaks,decks;for(const auto& bucket:lane){peaks.append(std::round(bucket.peak*1000)/1000);decks.append(bucket.deck);}
        state["lane"]=QJsonObject{{"bucketMs",40},{"peaks",peaks},{"decks",decks}};
        return state;
    }
    /// The turn changes: `target` is ON AIR from now. The previous DJ becomes
    /// OUTGOING and keeps sounding on the new DJ's J until released.
    ///
    /// Program switches at a seam where only J was sounding, so the venue
    /// never gaps or doubles: this computer measures it on its first anchored
    /// capture block when it is the new DJ; a remote new DJ measured it the
    /// same way and `report` carries the frame and the capture generation
    /// from which its stream is valid.
    void selectOwner(const QString& target,std::optional<SeamReport> report={}) {
        if(target!=auth.local){auto found=peers.find(target);if((found==peers.end()||!found->second->approved)&&!(liteSession&&validOpaqueId(target)))return;}
        const auto previous=auth.owner;if(previous==target)return;
        const bool previousLite=litePeer(previous);const auto previousEpoch=auth.epoch;Q_UNUSED(previousLite);
        auth.owner=target;auth.next.clear();auth.phase="playing";++auth.epoch;++auth.revision;
        recoveryResumeFrame=0;finishedOrder.removeAll(target);
        if(!previous.isEmpty()&&previous!=target&&!turnRepeat&&!finishedOrder.contains(previous))finishedOrder.append(previous);
        if(hosting)rosterOrder=turn::afterTurn(rosterOrder,target,previous,finishedOrder);
        else{rosterOrder.removeAll(target);rosterOrder.prepend(target);}
        nextReport={};standbyAudioAt=0;faderStart.reset();onAirPending=false;
        ownerAudioAt=0;captureEpoch.store(auth.epoch);scheduledEpoch.store(0);scheduledFrame.store(UINT64_MAX);
        ownerMinGeneration=report&&target!=auth.local?report->generation:0;
        // The new DJ keeps J exactly where they mixed it: the fader move that
        // made them ON AIR must not be undone. J was set to unity at STANDBY.
        const bool takeOver=target==auth.local&&!previous.isEmpty()&&previous!=auth.local;
        releasingPeer=!previous.isEmpty()&&previous!=target?previous:QString{};
        relayHoldSince=releasingPeer.isEmpty()?0:monotonicNanos();
        setInputMainMix(takeOver);
        if(takeOver){const auto channel=backend->junctionInputState();inputRelease.begin(monotonicNanos(),channel["available"].toBool()&&channel["audible"].toBool());}else inputRelease.clear();
        if(hosting)startLiteReturn(previous,target);
        seam.reset();seamFrame.store(0,std::memory_order_relaxed);
        const bool measureSeam=takeOver&&programOpened;
        const bool remoteSeam=hosting&&report&&target!=auth.local;
        if(hosting&&!previous.isEmpty()&&(measureSeam||remoteSeam))seam=InputSeam{previous,previousEpoch};
        if(remoteSeam)seamFrame.store(report->frame,std::memory_order_release);
        takeoverAnchor.store(measureSeam,std::memory_order_release);
        const bool stillSending=localSending();
        if(target==auth.local){q->setCaptureAnchor(UINT64_MAX,0);captureEnabled.store(true);tap.enable(liteSession,true);}else if(!stillSending){captureEnabled.store(false);tap.enable(false,false);}
        for(auto& [id,p]:peers){
            if(p->lite&&p->transport){if((hosting&&id==target)||(!hosting&&id==auth.host&&target==auth.local))p->transport->setBrowserReceive(auth.epoch,now());if(!hosting&&id==auth.host){if(target==auth.local){p->transport->setBrowserSendEpoch(auth.epoch,now());p->transport->startProducer(&tap.networkRing());p->producing=true;}else if(!stillSending){p->transport->startProducer(nullptr);p->producing=false;}}}
        }
        applyTail(previous==auth.local&&stillSending);
        sendLiteOwner();broadcast("session.snapshot",wireState(false));
    }
    bool turnEligible(const QString& id) const {
        if(id==auth.local)return true;
        auto found=peers.find(id);if(found==peers.end())return false;const auto& peer=*found->second;
        if(!peer.approved||!peer.hello||!peer.faderStartV1)return false;
        return peer.lite||(peer.transport&&peer.transport->aggregateLinkState()==LinkState::Connected);
    }
    /// STANDBY for `id`: J carries the ON AIR DJ's sound to them from now.
    void setNext(const QString& id) {
        if(id==auth.next)return;
        auth.next=id;nextReport={};standbyAudioAt=0;++auth.revision;
        if(id==auth.local){faderStart.reset();backend->junctionInputTakeOver();setInputMainMix(true);}
        else if(auth.owner!=auth.local||releasingPeer.isEmpty())setInputMainMix(false);
        if(id.isEmpty()||id==auth.local||auth.owner.isEmpty())stopLiteReturns();
        else startLiteReturn(auth.owner,id);
        sendLiteOwner();broadcast("session.snapshot",wireState(false));
    }
    /// Host: the next DJ is the first eligible DJ in the timetable, and only
    /// after the previous tail was released (never two J feeds at once).
    void updateTurn() {
        if(!hosting||lifecycle!="live")return;
        const auto current=auth.next;
        const auto wanted=releasingPeer.isEmpty()?turn::nextInOrder(rosterOrder,auth.owner,releasingPeer,finishedOrder,[this](const QString& id){return turnEligible(id);}):QString{};
        if(wanted==current)return;
        if(!current.isEmpty()&&current!=auth.owner&&!turnEligible(current)&&peers.count(current)==0)problem=QStringLiteral("次のDJとの接続が切れたため、順番を繰り上げました");
        setNext(wanted);
    }
    void joinQueue(const QString& id) {
        if(!hosting||id.isEmpty()||id==auth.owner)return;
        finishedOrder.removeAll(id);rosterOrder=turn::joined(rosterOrder,auth.owner,id,finishedOrder);
        ++auth.revision;broadcast("session.snapshot",wireState(false));
    }
    void leaveQueue(const QString& id) {
        if(!hosting||id.isEmpty()||id==auth.owner)return;
        if(!finishedOrder.contains(id))finishedOrder.append(id);rosterOrder.removeAll(id);rosterOrder.append(id);
        if(auth.next==id)setNext({});
        ++auth.revision;broadcast("session.snapshot",wireState(false));
    }
    void setCue(const QString& kind,const QString& from) {
        static const QSet<QString> kinds{"one_more","go_ahead","hold","ok"};
        if(!kinds.contains(kind))return;
        cueKind=kind;cueFrom=from;cueAt=QDateTime::currentMSecsSinceEpoch();++auth.revision;
        if(hosting)broadcast("session.snapshot",wireState(false));
    }
    /// A READY DJ goes on air. The host switches at once; a guest first
    /// anchors its capture to J so its stream names the seam frame.
    void goOnAir() {
        if(auth.sessionId.isEmpty()||lifecycle!="live"||auth.next!=auth.local||auth.owner==auth.local||!releasingPeer.isEmpty()||onAirPending)return;
        if(hosting){selectOwner(auth.local);return;}
        auto host=peers.find(auth.host);if(host==peers.end()||!host->second->producing)return;
        if(host->second->lite){sendLite(*host->second,{{"type","onair"}});onAirSentAt=monotonicNanos();return;}
        seamFrame.store(0,std::memory_order_release);q->setCaptureAnchor(UINT64_MAX,0);takeoverAnchor.store(true,std::memory_order_release);
        onAirPending=true;onAirArmedAt=monotonicNanos();
    }
    qint64 onAirSentAt=0;
    /// The ON AIR DJ is gone. A READY next DJ replaces them when the host
    /// allows it; otherwise the existing recovery keeps the venue served.
    void ownerLost(const QString& reason) {
        if(hosting&&autoFailover&&!auth.next.isEmpty()&&releasingPeer.isEmpty()){
            const bool nextReady=auth.next==auth.local?localReady:nextReport.ready&&monotonicNanos()-nextReport.at<3000000000LL;
            if(nextReady){
                problem=reason;
                if(auth.next==auth.local)selectOwner(auth.local);
                else{auto next=peers.find(auth.next);if(next!=peers.end())queue(*next->second,"turn",{{"kind","force"}});}
                return;
            }
        }
        beginRecovery(reason);
    }
    QString nextStatus() const {
        if(!hosting)return hostTurn["nextStatus"].toString();
        if(auth.next.isEmpty())return {};
        if(auth.next==auth.local)return localReady?QStringLiteral("ready"):localStatus();
        if(!nextReport.at||monotonicNanos()-nextReport.at>3000000000LL)return {};
        return nextReport.ready?QStringLiteral("ready"):nextReport.status;
    }
    QString localStatus() const {
        if(localReady)return QStringLiteral("ready");
        bool loaded=false,cueing=false;
        if(q->localDeckTracks)for(const auto& value:q->localDeckTracks()){const auto deck=value.toObject();loaded=true;cueing|=deck["pfl"].toBool();}
        if(cueing)return QStringLiteral("cueing");
        if(loaded)return QStringLiteral("loaded");
        return !inputPeer.isEmpty()&&input.receiving()?QStringLiteral("receiving"):QString{};
    }
    /// This DJ's own READY evaluation and fader start. Runs on every computer
    /// from measurements only it can make: its J, its meters, its latency.
    void updateLocalTurn() {
        const auto nowNanos=monotonicNanos();
        const bool live=!auth.sessionId.isEmpty()&&lifecycle=="live";
        const bool isNext=live&&auth.next==auth.local&&auth.owner!=auth.local&&releasingPeer.isEmpty();
        const auto underruns=input.underruns();if(underruns!=lastUnderruns){lastUnderruns=underruns;underrunAt=nowNanos;}
        const bool receiving=!inputPeer.isEmpty()&&input.receiving();
        if(!receiving)junctionSince=0;else if(!junctionSince)junctionSince=nowNanos;
        const bool hasJunction=!auth.owner.isEmpty();
        const auto channel=backend->junctionInputState();
        const bool unity=turn::junctionUnity(channel);
        const bool microphone=backend->audio()["microphone"].toObject()["enabled"].toBool();
        const float level=microphone?1.f:backend->junctionLocalPeak();
        const bool moved=isNext&&hasJunction&&channel["available"].toBool()&&!unity;
        localAudible=level>=turn::FaderStart::kThreshold||moved;
        auto hostPeer=peers.find(auth.host);const bool hostKnown=hostPeer!=peers.end();
        turn::ReadyInputs in;
        in.compatible=hosting||(hostKnown&&hostPeer->second->faderStartV1);
        in.programOpen=hosting?programOpened:hostTurn["programOpen"].toBool();
        in.junctionRequired=hasJunction;
        in.junctionReceiving=receiving;
        in.junctionStable=receiving&&nowNanos-junctionSince>=1000000000LL&&(!underrunAt||nowNanos-underrunAt>=1500000000LL);
        double lagMs=0;bool lagKnown=!hasJunction;
        if(hasJunction&&receiving){if(const auto rendered=input.renderedMediaFrame()){const auto current=now();lagMs=current>*rendered?double(current-*rendered)/48.0:0;lagKnown=true;}}
        double rtt=0;bool rttKnown=hosting;
        if(!hosting&&hostKnown&&hostPeer->second->probeReady){rtt=hostPeer->second->smoothedRttMs;rttKnown=true;}
        in.latencyMeasured=lagKnown&&rttKnown;
        const quint32 delayFrames=hosting?delay:quint32(std::clamp(hostTurn["programDelayFrames"].toInt(int(delay)),4800,480000));
        in.pathLatencyMs=turn::LatencyBudget::pathMs(lagMs,rtt,hosting);in.budgetMs=turn::LatencyBudget::budgetMs(delayFrames);
        pathLatencyMs=in.latencyMeasured?in.pathLatencyMs:-1;
        in.standbyStreamRequired=!hosting;in.standbyStreamReceived=hostTurn["standbyReceived"].toBool();
        in.junctionUnity=!hasJunction||unity;
        in.localSilent=faderStart.armed();
        const auto before=turn::readyBlocker(in);
        const bool edge=isNext&&faderStart.observe(level,moved,nowNanos);
        if(edge&&before==turn::Blocker::None)goOnAir();
        if(!isNext)faderStart.reset();
        in.localSilent=faderStart.armed();
        const bool going=onAirPending||(onAirSentAt&&nowNanos-onAirSentAt<3000000000LL);
        localBlocker=isNext&&!going?turn::readyBlocker(in):turn::Blocker::None;
        localReady=isNext&&localBlocker==turn::Blocker::None;
        if(!isNext)onAirSentAt=0;
        if(hosting||!hostKnown||!hostPeer->second->transport)return;
        const bool liteHost=hostPeer->second->lite;
        // Tell the host how this DJ's preparation looks, for the ON AIR DJ.
        if(isNext){
            const auto status=localStatus();
            const auto signature=QStringLiteral("%1|%2|%3").arg(localReady).arg(turn::blockerCode(localBlocker),status);
            if(signature!=lastReportSignature||nowNanos-reportSentAt>=1000000000LL){lastReportSignature=signature;reportSentAt=nowNanos;
                if(liteHost)sendLite(*hostPeer->second,{{"type","report"},{"ready",localReady},{"blocker",turn::blockerCode(localBlocker)},{"status",status}});
                else queue(*hostPeer->second,"turn",{{"kind","report"},{"ready",localReady},{"blocker",turn::blockerCode(localBlocker)},{"status",status}});}
        }else lastReportSignature.clear();
        // J metadata for whoever mixes this DJ's sound next.
        const bool source=live&&((auth.owner==auth.local&&!auth.next.isEmpty())||releasingPeer==auth.local);
        if(source&&nowNanos-decksSentAt>=500000000LL){decksSentAt=nowNanos;if(liteHost)sendLite(*hostPeer->second,{{"type","decks"},{"decks",localDecksJson()}});else queue(*hostPeer->second,"turn",{{"kind","decks"},{"decks",localDecksJson()}});}
    }
    void turnMessage(Peer& p,const QJsonObject& m) {
        const auto kind=m["kind"].toString();const auto id=p.id;
        if(hosting&&p.approved){
            if(kind=="report"&&id==auth.next){nextReport={m["ready"].toBool(),m["status"].toString().left(16),monotonicNanos()};++auth.revision;return;}
            if(kind=="onair"&&id==auth.next&&releasingPeer.isEmpty()&&lifecycle=="live"&&programOpened){const auto frame=parseU64(m["seamFrame"]),generation=parseU64(m["generation"]);if(frame&&generation)selectOwner(id,SeamReport{*frame,*generation});return;}
            if(kind=="release"&&id==auth.owner&&!releasingPeer.isEmpty()){releaseInput();return;}
            if(kind=="tail_ended"&&id==releasingPeer){releaseInput();return;}
            if(kind=="decks"&&m["decks"].isArray()&&(id==auth.owner||id==releasingPeer)){p.liteDecks=sanitizeDecks(m["decks"].toArray());p.liteDecksAt=monotonicNanos();return;}
            if(kind=="cue"){setCue(m["cue"].toString(),id);return;}
            if(kind=="join"){joinQueue(id);return;}
            if(kind=="leave"){leaveQueue(id);return;}
            return;
        }
        if(!hosting&&id==auth.host){
            if(kind=="decks"&&m["decks"].isArray()){p.liteDecks=sanitizeDecks(m["decks"].toArray());p.liteDecksAt=monotonicNanos();return;}
            if(kind=="force"&&auth.next==auth.local){goOnAir();return;}
        }
    }
    /// Turn state for snapshots. The wire copy carries only shared facts; the
    /// local copy adds this DJ's signal, reason, tail and measurements.
    QJsonObject turnJson(bool wire) const {
        QJsonObject t;
        if(hosting){
            QJsonArray out;for(const auto& id:finishedOrder)out.append(id);
            QJsonArray incompatible;for(const auto& [id,peer]:peers)if(peer->approved&&peer->hello&&!peer->faderStartV1)incompatible.append(id);
            t={{"nextPeerId",auth.next},{"outgoingPeerId",releasingPeer},{"repeat",turnRepeat},{"outOfQueue",out},{"incompatiblePeerIds",incompatible},{"autoFailover",autoFailover},
               {"programOpen",programOpened},{"programDelayFrames",int(delay)},{"standbyEpoch",u64(auth.epoch+1)},{"nextStatus",nextStatus()},
               {"standbyReceived",!auth.next.isEmpty()&&standbyAudioAt&&monotonicNanos()-standbyAudioAt<1000000000LL}};
            if(cueAt)t["cue"]=QJsonObject{{"kind",cueKind},{"fromPeerId",cueFrom},{"at",double(cueAt)}};
        }else{
            t=hostTurn;t["nextPeerId"]=auth.next;t["outgoingPeerId"]=releasingPeer;
            for(const char* key:{"repeat","autoFailover"})if(!t.contains(key))t[key]=false;
            for(const char* key:{"outOfQueue","incompatiblePeerIds"})if(!t[key].isArray())t[key]=QJsonArray{};
            if(!t.contains("nextStatus"))t["nextStatus"]=QString{};
        }
        if(wire)return t;
        const bool active=!auth.sessionId.isEmpty()&&lifecycle=="live";
        const auto signal=turn::derive({auth.local,auth.owner,auth.next,releasingPeer,active},localReady);
        t["signal"]=turn::signalName(signal);
        t["blocker"]=QJsonObject{{"code",turn::blockerCode(localBlocker)},{"text",turn::blockerText(localBlocker)}};
        QJsonArray tail;for(int deck=0;deck<4;++deck)if(tailDecks.contains(deck))tail.append(QString(QChar('A'+deck)));
        t["tailDecks"]=tail;t["localAudible"]=localAudible;
        if(pathLatencyMs>=0)t["latency"]=QJsonObject{{"pathMs",std::round(pathLatencyMs)},{"budgetMs",turn::LatencyBudget::budgetMs(hosting?delay:quint32(t["programDelayFrames"].toInt(int(delay))))}};
        for(const char* key:{"standbyEpoch","programDelayFrames","standbyReceived"})t.remove(key);
        return t;
    }
    void liteControl(const QString& id,const QByteArray& bytes) {
        auto found=peers.find(id);if(found==peers.end()||!found->second->lite||bytes.size()>4096)return;
        QJsonParseError parse;const auto document=QJsonDocument::fromJson(bytes,&parse);if(parse.error!=QJsonParseError::NoError||!document.isObject())return;
        const auto message=document.object();const auto type=message["type"].toString();
        if(type=="hello"&&message["capabilities"].isArray()){const auto capabilities=message["capabilities"].toArray();found->second->faderStartV1=capabilities.size()<=32&&capabilities.contains(QString::fromLatin1(turn::kCapability));++auth.revision;return;}
        if(hosting&&found->second->approved&&found->second->faderStartV1){
            // PlumDeck Lite speaks the same turn events on its data channel.
            // Browsers have no frame-accurate seam: the switch is timed.
            if(type=="onair"&&id==auth.next&&releasingPeer.isEmpty()&&lifecycle=="live"&&programOpened){selectOwner(id);return;}
            if(type=="report"&&id==auth.next){nextReport={message["ready"].toBool(),message["status"].toString().left(16),monotonicNanos()};++auth.revision;return;}
            if(type=="tail-ended"&&id==releasingPeer){releaseInput();return;}
            if(type=="cue"){setCue(message["cue"].toString(),id);return;}
            if(type=="join"){joinQueue(id);return;}
            if(type=="leave"){leaveQueue(id);return;}
        }
        if(type=="handoff-request"&&hosting&&found->second->approved){joinQueue(id);return;}
        if(type=="input-released"&&hosting&&found->second->approved&&(auth.owner==id||releasingPeer==id)&&!releasingPeer.isEmpty()){releaseInput();return;}
        if(type=="decks"&&hosting&&found->second->approved&&message["decks"].isArray()){found->second->liteDecks=sanitizeDecks(message["decks"].toArray());found->second->liteDecksAt=monotonicNanos();return;}
        if(type=="owner"&&!hosting&&id==auth.host){
            const auto owner=message["ownerPeerId"].toString();if(!validOpaqueId(owner))return;
            QSet<QString> senders{owner};if(message["senderPeerIds"].isArray())for(const auto& value:message["senderPeerIds"].toArray())if(validOpaqueId(value.toString()))senders.insert(value.toString());
            liteSenders=senders;
            const bool withTurn=message["turn"].isObject();
            if(withTurn){hostTurn=message["turn"].toObject();if(message["capabilities"].isArray()&&message["capabilities"].toArray().contains(QString::fromLatin1(turn::kCapability)))found->second->faderStartV1=true;}
            const auto previousNext=auth.next;
            selectOwner(owner);
            if(withTurn){
                // The Lite host owns the timetable: its next and outgoing DJs are authoritative.
                auth.next=hostTurn["nextPeerId"].toString();
                const auto outgoing=hostTurn["outgoingPeerId"].toString();
                if(outgoing!=releasingPeer){releasingPeer=outgoing;if(!outgoing.isEmpty()&&auth.owner==auth.local)inputRelease.begin(monotonicNanos(),onAirSentAt!=0);else inputRelease.clear();}
                applyTail(!releasingPeer.isEmpty()&&releasingPeer==auth.local&&auth.owner!=auth.local);
                const bool isNext=auth.next==auth.local&&auth.owner!=auth.local;
                if(isNext&&previousNext!=auth.local){faderStart.reset();backend->junctionInputTakeOver();setInputMainMix(true);}
                if(!isNext&&!(auth.owner==auth.local&&!releasingPeer.isEmpty())&&inputMainMix)setInputMainMix(false);
                if(liteSenders.contains(auth.local)&&!found->second->producing&&found->second->transport){found->second->transport->setBrowserSendEpoch(auth.epoch,now());found->second->transport->startProducer(&tap.networkRing());found->second->producing=true;captureEnabled.store(true);tap.enable(true,true);}
                ++auth.revision;
            }
            // Released after the new operator faded this computer out.
            if(owner!=auth.local&&!liteSenders.contains(auth.local)&&found->second->producing){found->second->transport->startProducer(nullptr);found->second->producing=false;captureEnabled.store(false);tap.enable(false,false);++auth.revision;}
        }
    }
    QString buildLitePeer(Peer& p,bool host) {
        if(p.transport)return {};
        QPointer<Runtime> safe=q;const auto id=p.id;MediaTransport::Callbacks callbacks;
        callbacks.localDescription=[safe,id](bool,QString sdp,QString type,QString){if(safe)QMetaObject::invokeMethod(safe,[safe,id,sdp,type]{if(!safe)return;auto it=safe->d->peers.find(id);if(it==safe->d->peers.end()||!it->second->lite)return;it->second->liteSdp=sdp;it->second->liteSdpType=type;++safe->d->auth.revision;},Qt::QueuedConnection);};
        callbacks.linkState=[safe,id](bool,LinkState state){if(safe)QMetaObject::invokeMethod(safe,[safe,id,state]{if(!safe)return;auto it=safe->d->peers.find(id);if(it==safe->d->peers.end()||!it->second->lite)return;it->second->hello=state==LinkState::Connected;if(it->second->hello){safe->d->connection="connected";safe->d->problem.clear();safe->d->sendLiteOwner();if(!safe->d->hosting)safe->d->sendLite(*it->second,{{"type","hello"},{"capabilities",QJsonArray{QString::fromLatin1(turn::kCapability)}}});}++safe->d->auth.revision;},Qt::QueuedConnection);};
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
        p.hello=false;p.producing=false;
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
            name=packet.sessionName;pinnedHostFingerprint=packet.hostFingerprint;connection="pending";
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
        auto s=publicState(true);s.remove("invite");s.remove("privatePreview");s.remove("program");
        s["timelineOriginNanos"]=QString::number(timeline.originNanos());s["timelineOriginFrame"]=u64(timeline.originFrame());s["programDelayFrames"]=int(delay);s["engineFingerprint"]=fingerprint();
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
        if(envelope.type==MessageType::PeerHello){if(payload["fingerprint"]!=fingerprint()){p.hello=false;if(manual){discardAttempt(p);manualSetState(p,ExchangeState::Failed,"Junctionの基礎プロトコルに互換性がありません","version_mismatch");}else fail("Junctionの基礎プロトコルに互換性がありません");return;}const bool firstHello=!p.hello;p.hello=true;const auto capabilities=payload["junctionCapabilities"].toArray();if(capabilities.size()<=32&&capabilities.contains(QString::fromLatin1(turn::kCapability)))p.faderStartV1=true;if(firstHello)queue(p,"peer.hello",localProfile());if(payload.contains("djName")||payload.contains("displayName"))p.name=sanitizeDisplayName(payload["djName"].toString(payload["displayName"].toString(p.name)));if(payload.contains("avatarDataUrl"))p.avatarDataUrl=profileAvatar(payload["avatarDataUrl"].toString());if(payload.contains("themeColor"))p.themeColor=profileColor(payload["themeColor"].toString(),id);if(!manual||p.transport->aggregateLinkState()==LinkState::Connected)connection="connected";
            if(payload["stream"].isObject()){auto m=readStream(payload["stream"].toObject());const bool hostRelay=!hosting&&id==auth.host;const bool standby=id==auth.next&&m&&m->epoch==auth.epoch+1;const bool outgoing=!releasingPeer.isEmpty()&&id==releasingPeer;
            if(m && m->producerPeerId==id && (hostRelay || ((id==auth.owner || id==auth.next || outgoing) && (m->epoch==auth.epoch || standby || outgoing)))){p.transport->setReceiveManifest(*m);queue(p,"peer.hello",{{"fingerprint",fingerprint()},{"streamAck",m->streamId}});}}
            else if(payload["streamAck"].isString())p.transport->acknowledgeSendManifest(payload["streamAck"].toString());
            else if(!hosting && (auth.owner==auth.local || auth.next==auth.local))sendManifest(p);
            if(hosting&&(firstHello||payload.contains("djName")||payload.contains("displayName")||payload.contains("avatarDataUrl")||payload.contains("themeColor")))broadcast("session.snapshot",wireState(true));
        } else if(envelope.type==MessageType::SessionSnapshot && id==auth.host && !hosting) {
            const auto epoch=parseU64(payload["epoch"]);if(!epoch||*epoch<auth.epoch)return;
            const auto previousNext=auth.next,previousOwner=auth.owner;
            const auto incomingLifecycle=payload["lifecycle"].toString("live");if(incomingLifecycle=="lobby"||incomingLifecycle=="live")lifecycle=incomingLifecycle;
            if(manual&&auth.phase=="recovery"&&payload["handoffState"]=="playing")problem.clear();
            auth.epoch=*epoch;const auto publicOwner=payload["performerPeerId"].toString();auth.owner=publicOwner.isEmpty()&&lifecycle!="live"?auth.host:publicOwner;auth.next=payload["nextPeerId"].toString();auth.phase=payload["handoffState"].toString("playing");
            name=payload["sessionName"].toString();const auto incomingRoster=payload["participants"].toArray();if(incomingRoster.size()<=64){QJsonArray merged;for(const auto& value:incomingRoster){auto row=value.toObject();if(!row.contains("avatarDataUrl")){const auto previous=rosterMetadata(row["peerId"].toString());if(previous.contains("avatarDataUrl"))row["avatarDataUrl"]=previous["avatarDataUrl"];}merged.append(row);}participantRoster=merged;}
            bool valid=false;const auto t0=payload["timelineOriginNanos"].toString().toLongLong(&valid);auto f0=parseU64(payload["timelineOriginFrame"]);if(valid&&f0)timeline.adopt(t0,*f0);
            if(payload["turn"].isObject()){
                hostTurn=payload["turn"].toObject();turnRepeat=hostTurn["repeat"].toBool();autoFailover=hostTurn["autoFailover"].toBool();
                const auto outgoing=hostTurn["outgoingPeerId"].toString();
                if(!releaseRequested.isEmpty()&&(outgoing!=releaseRequested||monotonicNanos()-releaseRequestedAt>3000000000LL))releaseRequested.clear();
                if(outgoing!=releasingPeer&&(releaseRequested.isEmpty()||outgoing!=releaseRequested)){
                    releasingPeer=outgoing;
                    // Confirmation arrives after this DJ may already have started
                    // fading J: a fader start began from an audible J.
                    const auto channel=backend->junctionInputState();
                    if(!outgoing.isEmpty()&&auth.owner==auth.local)inputRelease.begin(monotonicNanos(),onAirSentAt!=0||(channel["available"].toBool()&&channel["audible"].toBool()));else inputRelease.clear();
                }
                if(const auto cue=hostTurn["cue"].toObject();!cue.isEmpty()){cueKind=cue["kind"].toString();cueFrom=cue["fromPeerId"].toString();cueAt=qint64(cue["at"].toDouble());}
            }
            const bool isNext=auth.next==auth.local&&auth.owner!=auth.local;
            const bool sendingTail=!releasingPeer.isEmpty()&&releasingPeer==auth.local&&auth.owner!=auth.local;
            const bool receivingTail=auth.owner==auth.local&&!releasingPeer.isEmpty();
            // STANDBY begins: J at unity, flat, THRU, in this DJ's mix; the
            // standby stream is stamped with the epoch this DJ will go on air with.
            if(isNext&&previousNext!=auth.local){faderStart.reset();backend->junctionInputTakeOver();setInputMainMix(true);}
            if(!isNext&&!receivingTail&&inputMainMix)setInputMainMix(false);
            if(auth.owner==auth.local&&previousOwner!=auth.local){onAirPending=false;onAirSentAt=0;faderStart.reset();}
            if(isNext){const auto standbyEpoch=parseU64(hostTurn["standbyEpoch"]);if(standbyEpoch&&captureEpoch.load()!=*standbyEpoch&&!onAirPending)captureEpoch.store(*standbyEpoch);}
            else if(!captureEnabled.load())captureEpoch.store(auth.epoch);
            applyTail(sendingTail);
            if(!p.hello)queue(p,"peer.hello",localProfile());
            const bool producer=auth.owner==auth.local||isNext||sendingTail;
            if(producer&&!p.producing){if(isNext)q->setCaptureAnchor(UINT64_MAX,0);sendManifest(p);}
            else if(!producer&&p.producing){p.transport->startProducer(nullptr);p.producing=false;if(!hosting){captureEnabled.store(false);tap.enable(false,false);}}
            ++auth.revision;
        } else if(envelope.type==MessageType::ClockProbeRequest) {queue(p,"clock.reply",{{"t1",payload["t1"]},{"t2",QString::number(monotonicNanos())},{"t3",QString::number(monotonicNanos())}});}
        else if(envelope.type==MessageType::ClockProbeReply && id==auth.host) {bool a,b,c;auto t1=payload["t1"].toString().toLongLong(&a),t2=payload["t2"].toString().toLongLong(&b),t3=payload["t3"].toString().toLongLong(&c);const auto t4=monotonicNanos();const ClockProbe probe{t1,t2,t3,t4};if(a&&b&&c&&clock.add(probe)){timelineAnchor.store(clock.toLocalNanos(timeline.originNanos()));const double sampleRttMs=double((t4-t1)-(t3-t2))/1000000.0;const auto stats=p.transport?p.transport->statistics():MediaTransport::Statistics{};quint64 received=stats.receivedPackets,nacks=stats.nacksSent;if(p.healthStatsReady&&stats.receivedPackets>=p.healthStats.receivedPackets&&stats.nacksSent>=p.healthStats.nacksSent){received-=p.healthStats.receivedPackets;nacks-=p.healthStats.nacksSent;}const auto total=received+nacks;HealthReportMessage report;if(p.probeReady){p.smoothedJitterMs=.75*p.smoothedJitterMs+.25*std::abs(sampleRttMs-p.lastProbeRttMs);p.smoothedRttMs=.65*p.smoothedRttMs+.35*sampleRttMs;}else{p.smoothedRttMs=sampleRttMs;p.smoothedJitterMs=0;p.probeReady=true;}p.lastProbeRttMs=sampleRttMs;p.healthStats=stats;p.healthStatsReady=true;report.rttMs=std::clamp(p.smoothedRttMs,0.0,600000.0);report.jitterMs=std::clamp(p.smoothedJitterMs,0.0,600000.0);report.lossFraction=total?std::clamp(double(nacks)/double(total),0.0,1.0):0;report.queueFrames=0;report.audioClockErrorMs=0;report.acceptable=report.rttMs<=250.0&&report.lossFraction<=.05;p.health=report;p.hasHealth=true;p.healthAt=t4;queue(p,"health.report",report.toJson());}}
        else if(envelope.type==MessageType::HealthReport && hosting&&p.approved){QString reason;const auto report=HealthReportMessage::fromJson(payload,&reason);if(report){p.health=*report;p.hasHealth=true;p.healthAt=monotonicNanos();++auth.revision;}}
        else if(envelope.type==MessageType::Turn){turnMessage(p,payload);}
        else if(envelope.type==MessageType::SessionRecovery&&id==auth.host){
            if(manual&&payload["stage"]=="active"&&auth.owner==auth.local){q->setCaptureAnchor(lastCaptureSourceEnd.load(),now());captureEnabled.store(true);sendManifest(p);}
            if(payload["stage"]=="scheduled"){auto frame=parseU64(payload["frame"]),epoch=parseU64(payload["epoch"]);if(frame&&epoch&&*epoch>auth.epoch){recoveryResumeFrame=*frame;recoveryEpoch=*epoch;}}
            auth.phase="recovery";fail(payload["reason"].toString("配信を復旧中です"));
        }
        else if(envelope.type==MessageType::SessionEnd){
            if(hosting&&endingAt&&payload["ack"].toBool())p.endingAck=true;
            else if(id==auth.host&&!hosting){queue(p,"session.end",{{"ack",true}});connection="closing";endingAt=monotonicNanos()+200000000LL;}
        }
        else if(envelope.type==MessageType::PeerLeave){if(id==auth.owner)ownerLost("プレイ担当者が退出しました");peers.erase(id);rosterOrder.removeAll(id);finishedOrder.removeAll(id);}
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
        auth.phase="recovery";fail(reason);
        if(hosting)broadcast("session.recovery",{{"stage","active"},{"reason",reason}});
        ++auth.revision;
    }
    void route(PcmRing& ring,const QString& producer) {
        for(int count=0;count<8;++count){auto result=ring.popBlock(pcm.data(),4096);if(!result.frames)return;
            if(!inputPeer.isEmpty()&&producer==inputPeer)feedInput(pcm.data(),result.info);
            if(hosting&&!auth.next.isEmpty()&&producer==auth.next)standbyAudioAt=monotonicNanos();
            if(hosting&&!releasingPeer.isEmpty()&&producer==releasingPeer)relayAudioAt=monotonicNanos();
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
                if(manual&&auth.phase=="recovery"&&!recoveryResumeFrame&&info.mediaFrame+JunctionInput::kMaxSeamLag48k>=now()&&info.mediaFrame<=now()+4800){auth.phase="playing";problem.clear();++auth.revision;broadcast("session.snapshot",wireState());}
            }
            const auto allowed=[&](quint64 f){if(f<programEnqueuedThrough)return false;if(auth.phase=="recovery")return recoveryResumeFrame&&f>=recoveryResumeFrame&&producer==auth.host&&info.epoch==recoveryEpoch;if(seam){const auto boundary=seamFrame.load(std::memory_order_acquire);if(!boundary||f<boundary)return producer==seam->oldOwner&&info.epoch==seam->oldEpoch;}return producer==auth.owner&&info.epoch==auth.epoch&&(producer==auth.local||info.generation>=ownerMinGeneration);};
            quint32 begin=0,end=info.frameCount;
            while(begin<end&&!allowed(info.mediaFrame+mediaFrameAdvance(begin,info.sampleRateHz)))++begin;
            while(end>begin&&!allowed(info.mediaFrame+mediaFrameAdvance(end-1,info.sampleRateHz)))--end;
            if(begin==end)continue;
            info.mediaFrame+=mediaFrameAdvance(begin,info.sampleRateHz);info.sourceFrame+=begin;info.frameCount=end-begin;
            if(programPending.size()<512)programPending.try_emplace(info.mediaFrame,ProgramBlock{info,std::vector<float>(pcm.data()+begin*2,pcm.data()+end*2)});
        }
    }
    void tick() {
        ++ticks;if(networkProbe)testNetwork(false);if(auth.sessionId.isEmpty())return;
        manualTick();
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
        // Recovery resumes on the host's own mix at the scheduled frame; the
        // capture epoch split is kept until that frame is safely behind.
        if(recoveryResumeFrame&&auth.phase=="recovery"&&now()>=recoveryResumeFrame){
            const auto previous=auth.owner;
            auth.owner=auth.host;auth.epoch=recoveryEpoch;auth.phase="playing";auth.next.clear();
            if(hosting){
                if(!previous.isEmpty()&&previous!=auth.host&&!turnRepeat&&!finishedOrder.contains(previous))finishedOrder.append(previous);
                finishedOrder.removeAll(auth.host);rosterOrder=turn::afterTurn(rosterOrder,auth.host,previous,finishedOrder);
            }
            problem.clear();ownerAudioAt=monotonicNanos();++auth.revision;
        }
        if(recoveryResumeFrame&&auth.phase!="recovery"&&now()>recoveryResumeFrame+delay+48000){recoveryResumeFrame=0;scheduledFrame.store(UINT64_MAX);scheduledEpoch.store(0);}
        auth.sending=localSending();
        if(ticks%4==0){updateTurn();updateLocalTurn();}
        if(onAirPending){
            auto host=peers.find(auth.host);
            if(!takeoverAnchor.load(std::memory_order_acquire)&&seamFrame.load(std::memory_order_acquire)&&host!=peers.end()){
                queue(*host->second,"turn",{{"kind","onair"},{"seamFrame",u64(seamFrame.load(std::memory_order_acquire))},{"generation",u64(captureGeneration.load(std::memory_order_acquire))}});
                onAirPending=false;onAirSentAt=monotonicNanos();
            }else if(monotonicNanos()-onAirArmedAt>2000000000LL){onAirPending=false;takeoverAnchor.store(false,std::memory_order_release);}
        }
        if(tailApplied&&releasingPeer==auth.local&&auth.owner!=auth.local){
            bool playing=false;for(int deck:tailDecks)playing|=backend->playing(deck);
            if(tailEnded.observe(playing,backend->junctionLocalPeak(),monotonicNanos())){
                tailEnded.reset();
                if(hosting)releaseInput();else{auto host=peers.find(auth.host);if(host!=peers.end()){if(host->second->lite)sendLite(*host->second,{{"type","tail-ended"}});else queue(*host->second,"turn",{{"kind","tail_ended"}});}}
            }
        }
        // Host: J metadata for a remote receiver, relayed from its source.
        if(hosting&&ticks%100==0){
            const QString receiver=!releasingPeer.isEmpty()?(auth.owner!=auth.local?auth.owner:QString{}):(!auth.next.isEmpty()&&auth.next!=auth.local&&!auth.owner.isEmpty()?auth.next:QString{});
            const QString source=!releasingPeer.isEmpty()?releasingPeer:auth.owner;
            auto target=peers.find(receiver);
            if(target!=peers.end()&&target->second->transport&&!target->second->lite){
                QJsonArray decks;if(source==auth.local)decks=localDecksJson();else{auto from=peers.find(source);if(from!=peers.end())decks=displayedLiteDecks(*from->second);}
                if(!decks.isEmpty())queue(*target->second,"turn",{{"kind","decks"},{"decks",decks}});
            }
        }
        // Retired when Program has actually been fed past the boundary, so a
        // stalled or silent old stream cannot strand it.
        if(seam){const auto boundary=seamFrame.load(std::memory_order_acquire);if(boundary&&programEnqueuedThrough>=boundary)seam.reset();}
        if(const auto wanted=junctionInputPeer();wanted!=inputPeer){inputPeer=wanted;resetInput();++auth.revision;}
        if(!releasingPeer.isEmpty()){
            // The new operator faded the outgoing DJ out: stop their stream.
            const auto channel=backend->junctionInputState();
            const bool localSource=releasingPeer==auth.local;
            const bool localReceiver=auth.owner==auth.local;
            if(localReceiver){
                const bool audible=channel["available"].toBool()&&channel["audible"].toBool();
                if(inputRelease.observe(audible,localSource||input.receiving(),monotonicNanos()))releaseInput();
            }else if(hosting&&relayHoldSince){
                // A remote receiver releases by itself; the host only ends a
                // tail whose stream is gone or that outlived any mix.
                const auto nowNanos=monotonicNanos();
                const bool delivering=localSource||(relayAudioAt&&nowNanos-relayAudioAt<InputRelease::kStreamEndedNanos)||nowNanos-relayHoldSince<InputRelease::kStreamEndedNanos;
                if(!delivering||nowNanos-relayHoldSince>=InputRelease::kMaxHoldNanos)releaseInput();
            }
        }
        const auto ownerMessage=ticks%200==0?liteOwnerMessage():QJsonObject{};
        for(auto& [id,p]:peers){if(!p->transport)continue;if(!p->lite){for(int n=0;n<8&&!p->pending.isEmpty();++n){if(!p->transport->sendControl(p->pending.head().bytes))break;p->pending.dequeue();}if(ticks%200==0)p->transport->sendKeepAlive();}else if(ticks%200==0)sendLite(*p,ownerMessage);if(hosting||id==auth.host)route(p->transport->decodedRing(),id);}
        if(endingAt){bool acknowledged=hosting;for(const auto& [id,p]:peers)if(p->approved&&!p->endingAck)acknowledged=false;
            if(acknowledged||monotonicNanos()>=endingAt){signalSend({{"type",endingHost?"room.close":"peer.leave"}});stop();return;}
        }
        route(tap.localRing(),auth.local);
        const auto ownerPeer=peers.find(auth.owner);const bool liteOwner=ownerPeer!=peers.end()&&ownerPeer->second->lite;
        if(hosting&&!liteOwner&&auth.owner!=auth.local&&ownerAudioAt&&monotonicNanos()-ownerAudioAt>300000000LL&&auth.phase!="recovery")ownerLost("プレイ担当者の音声が届いていません。配信を復旧中です");
        if(ticks%kSnapshotIntervalTicks==0 && hosting)broadcast("session.snapshot",wireState(false));
        if(ticks%200==0){if(hosting)signalSend({{"type","host.heartbeat"}});else{auto p=peers.find(auth.host);if(p!=peers.end()&&p->second->transport)queue(*p->second,"clock.probe",{{"t1",QString::number(monotonicNanos())}});}}
        while(!programPending.empty()&&programPending.begin()->first+delay/2<now()){
            auto it=programPending.begin();if(!program.inputRing().push(it->second.samples.data(),it->second.info))break;programEnqueuedThrough=it->second.info.mediaFrame+mediaFrameAdvance(it->second.info.frameCount,it->second.info.sampleRateHz);programPending.erase(it);
        }
    }
    void stop() {
        ++signalGeneration;
#ifdef __APPLE__
        if(sleepLease!=kIOPMNullAssertionID){IOPMAssertionRelease(sleepLease);sleepLease=kIOPMNullAssertionID;}
#elif defined(_WIN32)
        SetThreadExecutionState(ES_CONTINUOUS);
#endif
        captureEnabled.store(false);tap.enable(false,false);separateLocalMaster.store(true);stopLiteReturns();setInputMainMix(false);
        inputPeer.clear();releasingPeer.clear();inputRelease.clear();liteSenders.clear();seam.reset();seamFrame.store(0);takeoverAnchor.store(false);resetInput();
        peers.clear();if(!localReturnReaders.load(std::memory_order_acquire))liteReturnRings.clear();program.close();programOpened=false;programState="stopped";
#if defined(PLUMDECK_JUNCTION_WITH_LIBDATACHANNEL)
        if(signal){signal->resetCallbacks();signal->forceClose();signal.reset();}
#endif
        tailApplied=false;tailDecks.clear();tailSend.store(false);if(backend)backend->junctionTail({});tailEnded.reset();faderStart.reset();onAirPending=false;onAirSentAt=0;ownerMinGeneration=0;standbyAudioAt=0;relayAudioAt=0;relayHoldSince=0;junctionSince=0;underrunAt=0;lastUnderruns=0;nextReport={};hostTurn={};localReady=false;localAudible=false;localBlocker=turn::Blocker::None;pathLatencyMs=-1;lastReportSignature.clear();cueKind.clear();cueFrom.clear();cueAt=0;turnRepeat=false;autoFailover=false;releaseRequested.clear();
        endingAt=0;endingHost=false;reconnectAt=0;reconnectAttempts=0;turnRefreshAt=0;iceReady=false;iceServers.clear();deferredSignals.clear();identity.reset();manual=false;liteSession=false;manualDetail.clear();manualErrorCode.clear();hostCertificatePem.clear();pinnedHostFingerprint.clear();participantRoster={};rosterOrder.clear();finishedOrder.clear();lifecycle="live";avatarDataUrl.clear();themeColor.clear();auth=Authority{};timeline=MediaTimeline{};clock.reset();q->setCaptureAnchor(UINT64_MAX,0);captureEpoch.store(1);scheduledFrame.store(UINT64_MAX);scheduledEpoch.store(0);sourceAnchor.store(UINT64_MAX);connection="disconnected";problem.clear();invite.clear();programEnqueuedThrough=0;recoveryResumeFrame=0;ownerAudioAt=0;programPending.clear();
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
QJsonObject Runtime::snapshot()const{return d->publicState();}
bool Runtime::localMasterAudible()const noexcept{return d->separateLocalMaster.load(std::memory_order_relaxed);}
void Runtime::readJunctionInput(float* out,unsigned frames)noexcept{d->input.read(out,frames);}
QString Runtime::authorize(const QString& op,const QJsonObject& p)const{if(op=="audio.config.set"&&d->auth.local!=d->auth.owner){const auto mic=p["microphone"].toObject();if(mic.size()==1&&mic["enabled"].isBool()&&!mic["enabled"].toBool())return {};}d->auth.sending=d->localSending();return d->auth.authorize(op,p);}
void Runtime::capture(const float* master,const float* local,unsigned frames,quint64 sourceFrame,unsigned rate)noexcept{
    if(!d->captureEnabled.load(std::memory_order_relaxed))return;
    // OUTGOING guest: the stream carries only the tail, never samplers, the
    // microphone or preparation on other decks.
    const float* pcm=d->tailSend.load(std::memory_order_relaxed)&&local?local:master;
    if(!pcm)return;
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
        d->name=p["sessionName"].toString(QStringLiteral("PlumDeck Lite Junction")).left(80);d->hosting=false;d->liteSession=true;d->lifecycle="live";d->auth.sessionId=sessionId;d->auth.local=local;d->auth.host=host;d->auth.owner=p["ownerPeerId"].toString(host);if(d->auth.owner!=host&&d->auth.owner!=local)d->auth.owner=host;d->auth.phase="playing";d->timeline.start(monotonicNanos());d->timelineAnchor.store(d->timeline.originNanos());d->connection="connecting";d->captureEnabled.store(false);d->tap.enable(false,false);
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
        if(!active()||!d->hosting)return reject("ホストのJunctionセッションが必要です");const auto id=p["peerId"].toString();auto found=d->peers.find(id);if(found!=d->peers.end()&&found->second->lite){if(d->auth.owner==id)d->ownerLost("プレイ担当者が退出しました");if(d->releasingPeer==id)d->releaseInput();d->peers.erase(found);d->rosterOrder.removeAll(id);d->finishedOrder.removeAll(id);++d->auth.revision;}return snapshot();
    }
    if(op=="lite.owner.set"){
        if(!active())return reject("Junctionセッションに参加していません");const auto owner=p["ownerPeerId"].toString();if(d->hosting&&!d->releasingPeer.isEmpty()&&owner!=d->auth.owner)return reject("前のDJをJUNCTION MASTERから解放してから次のDJへ交代してください");if(d->hosting||d->liteSession)d->selectOwner(owner);return snapshot();
    }
    if(op=="input.set"){
        if(!active())return reject("Junctionセッションに参加していません");
        if(d->localSending())return reject("交代後も音声を送出中のため、解放されるまで操作できません");
        const auto failure=d->backend->junctionInputSet(p);if(!failure.isEmpty())return reject(failure);
        return d->inputState();
    }
    if(op=="input.probe"){
        if(!qEnvironmentVariableIsSet("PLUMDECK_JUNCTION_AUDIO_PROBE"))return reject("音声プローブは無効です");
        if(active())return reject("セッション中は音声プローブを使えません");
        if(p.contains("tone")){
            const auto tone=p["tone"].toObject();d->probeAmplitude=std::clamp(tone["amplitude"].toDouble(),0.0,1.0);d->probeFrequency=std::clamp(tone["frequencyHz"].toDouble(440),20.0,20000.0);
            if(d->probeAmplitude<=0){d->probeTimer.stop();d->resetInput();}
            else if(!d->probeTimer.isActive()){d->resetInput();d->probeStart=monotonicNanos();d->probeWritten=0;d->probeSequence=0;d->probeTimer.start();}
        }
        if(p.contains("mainMix"))d->setInputMainMix(p["mainMix"].toBool());
        if(const auto channel=p["channel"].toObject();!channel.isEmpty()){const auto failure=d->backend->junctionInputSet(channel);if(!failure.isEmpty())return reject(failure);}
        auto state=d->inputState();state["programMixer"]=d->programMixer().toJson({},{},{});return state;
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
                    if(unused!=d->peers.end()){d->rosterOrder.removeAll(unused->first);d->finishedOrder.removeAll(unused->first);d->peers.erase(unused);}
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
        else d->auth.sessionId="pending";
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
        if(d->lifecycle=="live"){if(!d->auth.owner.isEmpty()&&requested.indexOf(d->auth.owner)!=d->rosterOrder.indexOf(d->auth.owner))return reject("演奏中のDJは移動できません。ほかのDJを並び替えてください");}
        d->rosterOrder=requested;++d->auth.revision;d->broadcast("session.snapshot",d->wireState());return snapshot();
    }
    if(op=="session.start"){
        if(!d->hosting)return reject("セッション管理者だけが開始できます");if(d->lifecycle!="lobby")return reject("このセッションはすでに開始しています");
        auto target=p["performerPeerId"].toString(p["targetPeerId"].toString());if(target.isEmpty())for(const auto& id:d->rosterOrder){if(id==d->auth.local){target=id;break;}auto peer=d->peers.find(id);if(peer!=d->peers.end()&&peer->second->approved&&peer->second->hello){target=id;break;}}
        if(target.isEmpty())return reject("最初にプレイするDJを選択してください");auto peer=d->peers.find(target);if(target!=d->auth.local&&(peer==d->peers.end()||!peer->second->approved||!peer->second->hello))return reject("最初のDJとの接続が完了していません");
        // Validate the venue output before mutating the shared order: a refused start leaves the lobby as it was.
        if(d->programDevice<0)return reject("会場への音声出力が未選択です。「セッション設定」の「会場への音声出力」で出力先を反映してから開始してください");
        d->openProgram();if(d->programState=="error")return reject(d->problem.isEmpty()?QStringLiteral("会場の音声出力を開けません"):d->problem);
        // Fader start from a silent J: the first DJ is simply next in the
        // timetable and goes on air by raising a fader, like any later turn.
        d->rosterOrder.removeAll(target);d->rosterOrder.prepend(target);d->finishedOrder.clear();d->problem.clear();
        d->captureEnabled.store(false);d->tap.enable(false,false);d->auth.owner.clear();d->auth.next.clear();d->lifecycle="live";++d->auth.revision;
        d->broadcast("session.snapshot",d->wireState());d->updateTurn();return snapshot();
    }
    if(op.startsWith("private.")){const auto failure=d->backend->privatePreviewCommand(op,p);if(!failure.isEmpty())return reject(failure);return snapshot();}
    if(op=="leave"||op=="end"){
        if(op=="leave"&&d->auth.owner==d->auth.local&&d->peers.size()>0)return reject("演奏を引き継いでから退出してください");
        if(op=="end"&&!d->hosting)return reject("ホストだけがセッションを終了できます");
        if(!d->endingAt){d->endingHost=d->hosting;d->endingAt=monotonicNanos()+(d->hosting?2000000000LL:200000000LL);d->connection="closing";d->broadcast(d->hosting?"session.end":"peer.leave",{});}return snapshot();
    }
    if(op=="peer.approve"){
        if(!d->hosting)return reject("ホストだけが参加を承認できます");auto i=d->peers.find(p["peerId"].toString());if(i==d->peers.end())return reject("参加希望が見つかりません");bool accept=p["accept"].toBool(true);d->signalSend({{"type","host.join_decision"},{"guestPeerId",i->first},{"accept",accept}});if(accept){i->second->approved=true;d->makePeer(*i->second,true);}else{d->rosterOrder.removeAll(i->first);d->finishedOrder.removeAll(i->first);d->peers.erase(i);}++d->auth.revision;return snapshot();
    }
    if(op=="invite.rotate") {if(!d->hosting)return reject("ホストだけが招待を更新できます");d->token=secureRandomToken(24);d->inviteExpiry=QDateTime::currentMSecsSinceEpoch()+3600000;d->signalSend({{"type","host.rotate_invite"},{"inviteTokenHash",sha256Hex(d->token.toUtf8())},{"inviteExpiresAt",double(d->inviteExpiry)}});d->updateInvite();return snapshot();}
    if(op=="program.configure") {if(!d->hosting)return reject("配信出力はホストが設定します");bool ok;int device=p["programDevice"].toString().toInt(&ok);if(!ok)return reject("配信先デバイスを選択してください");d->program.close();d->programOpened=false;d->programState="idle";d->programDevice=device;if(d->lifecycle!="lobby")d->openProgram();if(p.contains("gain"))d->program.setGain(float(p["gain"].toDouble()));return snapshot();}
    if(op=="program.record.start"||op=="program.record.stop") {if(!d->hosting)return reject("配信録音はホストが操作します");QString failure;bool ok=op.endsWith("start")?d->program.startRecording(p["path"].toString(),&failure):d->program.stopRecording(&failure);if(!ok)return reject(failure);return snapshot();}
    if(op=="recovery.resume"){
        if(!d->hosting||d->auth.phase!="recovery")return reject("ホストが復旧中に操作できます");
        d->recoveryResumeFrame=d->now()+24000;d->recoveryEpoch=d->auth.epoch+1;
        setCaptureAnchor(d->lastCaptureSourceEnd.load(),d->now());d->scheduledEpoch.store(d->recoveryEpoch);d->scheduledFrame.store(d->recoveryResumeFrame);d->captureEnabled.store(true);d->tap.enable(false,true);
        auto it=d->programPending.lower_bound(d->recoveryResumeFrame);d->programPending.erase(it,d->programPending.end());
        d->broadcast("session.recovery",{{"stage","scheduled"},{"reason","ホストの手元の演奏へ切り替えます"},{"frame",u64(d->recoveryResumeFrame)},{"epoch",u64(d->recoveryEpoch)}});return snapshot();
    }
    if(op.startsWith("turn.")){
        if(d->lifecycle!="live"&&op!="turn.repeat"&&op!="turn.failover"&&op!="turn.join"&&op!="turn.leave")return reject("セッションを開始してから操作してください");
        const auto hostPeer=d->peers.find(d->auth.host);
        const auto toHost=[&](const QJsonObject& message){if(hostPeer==d->peers.end())return false;if(hostPeer->second->lite)d->sendLite(*hostPeer->second,message);else d->queue(*hostPeer->second,"turn",message);return true;};
        if(op=="turn.join"||op=="turn.leave"){
            const auto target=p["peerId"].toString(d->auth.local);
            if(!d->hosting){if(target!=d->auth.local)return reject("ホストだけが他のDJの順番を変更できます");if(!toHost({{"kind",op=="turn.join"?"join":"leave"},{"type",op=="turn.join"?"join":"leave"}}))return reject("ホストに接続していません");return snapshot();}
            if(target!=d->auth.local&&!d->peers.count(target))return reject("参加者が見つかりません");
            if(target==d->auth.owner)return reject("ON AIRのDJは順番を変更できません");
            if(op=="turn.join")d->joinQueue(target);else d->leaveQueue(target);return snapshot();
        }
        if(op=="turn.repeat"){if(!d->hosting)return reject("ホストだけがB2Bの繰り返しを設定できます");if(!p["enabled"].isBool())return reject("繰り返しを指定してください");d->turnRepeat=p["enabled"].toBool();++d->auth.revision;d->broadcast("session.snapshot",d->wireState());return snapshot();}
        if(op=="turn.failover"){if(!d->hosting)return reject("ホストだけが設定できます");if(!p["auto"].isBool())return reject("自動か確認かを指定してください");d->autoFailover=p["auto"].toBool();++d->auth.revision;d->broadcast("session.snapshot",d->wireState());return snapshot();}
        if(op=="turn.onair"){
            if(d->auth.next!=d->auth.local||d->auth.owner==d->auth.local)return reject("順番が来てからON AIRにできます");
            if(!d->localReady)return reject(QStringLiteral("まだ本番に出ていません：")+turn::blockerText(d->localBlocker));
            d->goOnAir();return snapshot();
        }
        if(op=="turn.force"){
            if(!d->hosting)return reject("ホストだけが強制交代できます");
            if(d->auth.next.isEmpty())return reject("次のDJがいません");
            if(!d->releasingPeer.isEmpty())return reject("前のDJの曲が残っています。解放してから交代してください");
            if(!d->programOpened)return reject("会場への音声出力が開いていません");
            if(d->auth.next==d->auth.local){d->selectOwner(d->auth.local);return snapshot();}
            auto next=d->peers.find(d->auth.next);if(next==d->peers.end())return reject("次のDJが見つかりません");
            if(next->second->lite){d->selectOwner(d->auth.next);return snapshot();}
            d->queue(*next->second,"turn",{{"kind","force"}});return snapshot();
        }
        if(op=="turn.skip"){
            if(!d->hosting)return reject("ホストだけがスキップできます");if(d->auth.next.isEmpty())return reject("次のDJがいません");
            const auto skipped=d->auth.next;d->rosterOrder=turn::skipped(d->rosterOrder,d->auth.owner,skipped,d->finishedOrder);d->setNext({});d->updateTurn();
            if(d->auth.next==skipped)d->setNext({});return snapshot();
        }
        if(op=="turn.release"){
            if(d->releasingPeer.isEmpty())return reject("解放する前のDJはいません");
            if(!d->hosting&&d->auth.owner!=d->auth.local)return reject("ON AIRのDJかホストが解放できます");
            d->releaseInput();return snapshot();
        }
        if(op=="turn.cue"){
            const auto kind=p["kind"].toString();static const QSet<QString> kinds{"one_more","go_ahead","hold","ok"};if(!kinds.contains(kind))return reject("合図の種類が不正です");
            if(d->hosting)d->setCue(kind,d->auth.local);else if(!toHost({{"kind","cue"},{"type","cue"},{"cue",kind}}))return reject("ホストに接続していません");
            return snapshot();
        }
        return reject("対応していない順番の操作です");
    }
    return reject("対応していないセッション操作です");
}
}
