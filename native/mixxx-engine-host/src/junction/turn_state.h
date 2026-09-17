#pragma once
#include <QJsonObject>
#include <QSet>
#include <QString>
#include <QStringList>
#include <algorithm>
#include <cmath>
#include <optional>
namespace junction::turn {
/// Fader-start turn model: the booth handover as DJs know it.
///
/// Nothing here is a second source of truth. Each value is derived from the
/// roles the runtime already keeps (owner, next, releasingPeer, rosterOrder)
/// and from measurements (meters, latency), so the UI signal light and the
/// engine gate can never disagree.
constexpr auto kCapability="junction-fader-start-v1";

enum class Signal {Off,Standby,Ready,OnAir,Outgoing};
inline QString signalName(Signal s) {
    switch(s){
    case Signal::Standby:return QStringLiteral("standby");
    case Signal::Ready:return QStringLiteral("ready");
    case Signal::OnAir:return QStringLiteral("onair");
    case Signal::Outgoing:return QStringLiteral("outgoing");
    case Signal::Off:break;
    }
    return QStringLiteral("off");
}

struct Roles {
    QString local,owner,next,outgoing;
    bool active=false;
};
/// `ready`: every READY condition holds for the local DJ (see `Blocker`).
inline Signal derive(const Roles& r,bool ready) {
    if(!r.active||r.local.isEmpty())return Signal::Off;
    if(r.local==r.owner)return Signal::OnAir;
    if(r.local==r.outgoing)return Signal::Outgoing;
    if(r.local==r.next)return ready?Signal::Ready:Signal::Standby;
    return Signal::Off;
}

/// Why the next DJ is not READY. Ordered by what the DJ must fix first; only
/// the first applicable one is shown.
enum class Blocker {
    None,
    UpdateRequired,     ///< A peer on the path cannot negotiate fader start.
    ProgramClosed,      ///< The host's venue output is not open.
    WaitingJunction,    ///< The previous DJ's master is not arriving on J yet.
    JunctionUnstable,   ///< J underran recently.
    LatencyUnknown,     ///< The path delay has not been measured yet.
    LatencyBudget,      ///< The path is slower than the venue delay allows.
    HostWaitingStream,  ///< The host does not receive this DJ's standby stream.
    JunctionNotUnity,   ///< J must start at unity, flat EQ, THRU.
    AlreadyAudible,     ///< Local sound is already up: fader start needs silence first.
};
struct ReadyInputs {
    bool compatible=true;
    bool programOpen=true;
    /// The first DJ of a session starts from a silent J.
    bool junctionRequired=true;
    bool junctionReceiving=false;
    bool junctionStable=false;
    bool latencyMeasured=false;
    double pathLatencyMs=0,budgetMs=0;
    /// False when the host itself is the receiver (no standby stream needed).
    bool standbyStreamRequired=false;
    bool standbyStreamReceived=false;
    bool junctionUnity=false;
    bool localSilent=false;
};
inline Blocker readyBlocker(const ReadyInputs& in) {
    if(!in.compatible)return Blocker::UpdateRequired;
    if(!in.programOpen)return Blocker::ProgramClosed;
    if(in.junctionRequired){
        if(!in.junctionReceiving)return Blocker::WaitingJunction;
        if(!in.junctionStable)return Blocker::JunctionUnstable;
    }
    if(!in.latencyMeasured)return Blocker::LatencyUnknown;
    if(in.pathLatencyMs>in.budgetMs)return Blocker::LatencyBudget;
    if(in.standbyStreamRequired&&!in.standbyStreamReceived)return Blocker::HostWaitingStream;
    if(!in.junctionUnity)return Blocker::JunctionNotUnity;
    if(!in.localSilent)return Blocker::AlreadyAudible;
    return Blocker::None;
}
inline QString blockerCode(Blocker b) {
    switch(b){
    case Blocker::UpdateRequired:return QStringLiteral("update_required");
    case Blocker::ProgramClosed:return QStringLiteral("program_closed");
    case Blocker::WaitingJunction:return QStringLiteral("waiting_junction");
    case Blocker::JunctionUnstable:return QStringLiteral("junction_unstable");
    case Blocker::LatencyUnknown:return QStringLiteral("latency_unknown");
    case Blocker::LatencyBudget:return QStringLiteral("latency_budget");
    case Blocker::HostWaitingStream:return QStringLiteral("host_waiting_stream");
    case Blocker::JunctionNotUnity:return QStringLiteral("junction_not_unity");
    case Blocker::AlreadyAudible:return QStringLiteral("already_audible");
    case Blocker::None:break;
    }
    return {};
}
inline QString blockerText(Blocker b) {
    switch(b){
    case Blocker::UpdateRequired:return QStringLiteral("相手のアプリを更新してください");
    case Blocker::ProgramClosed:return QStringLiteral("会場への音声出力がまだ開いていません");
    case Blocker::WaitingJunction:return QStringLiteral("前のDJの音を受信するまでお待ちください");
    case Blocker::JunctionUnstable:return QStringLiteral("前のDJの音が途切れています。回線が安定するまでお待ちください");
    case Blocker::LatencyUnknown:return QStringLiteral("回線の遅延を測定しています");
    case Blocker::LatencyBudget:return QStringLiteral("回線の遅延が大きく、会場の音に間に合いません");
    case Blocker::HostWaitingStream:return QStringLiteral("ホストがあなたの音を受信するまでお待ちください");
    case Blocker::JunctionNotUnity:return QStringLiteral("JUNCTION MASTERを初期位置に戻しています");
    case Blocker::AlreadyAudible:return QStringLiteral("一度フェーダーを下げてください");
    case Blocker::None:break;
    }
    return {};
}

/// Local sound onset. Fader start fires only on a silence-to-sound edge: the
/// DJ's own sound (every local channel, samplers, microphone; never J) must
/// first stay below the threshold for `kArmNanos`, then rise above it for
/// `kOnsetNanos`. A DJ who arrives with a fader already up is never taken on
/// air by accident.
class FaderStart {
public:
    /// About -50 dBFS: above dithered silence and an idle microphone gate,
    /// far below any fader a DJ means to be heard.
    static constexpr float kThreshold=.00316f;
    static constexpr qint64 kOnsetNanos=20000000LL;   // 20 ms of sound
    static constexpr qint64 kArmNanos=300000000LL;    // 300 ms of silence first
    void reset() {sound_.reset();silent_.reset();armed_=false;}
    bool armed() const {return armed_;}
    bool silent() const {return silent_.has_value();}
    /// `level`: the latest post-fader local peak; `junctionMoved`: J left unity.
    /// Returns true exactly once per edge.
    bool observe(float level,bool junctionMoved,qint64 now) {
        const bool sound=junctionMoved||level>=kThreshold;
        if(!sound){sound_.reset();if(!silent_)silent_=now;if(now-*silent_>=kArmNanos)armed_=true;return false;}
        silent_.reset();
        if(!armed_)return false;
        if(!sound_)sound_=now;
        if(now-*sound_<kOnsetNanos)return false;
        armed_=false;sound_.reset();return true;
    }
private:
    std::optional<qint64> sound_,silent_;
    bool armed_=false;
};

/// The J deck is where the receiver found it at STANDBY start: unity, flat,
/// THRU. Anything else is either a deliberate mix move (fader start) or a
/// reason the receiver is not READY yet.
inline bool junctionUnity(const QJsonObject& channel) {
    const auto near=[&](const char* key){return std::abs(channel[key].toDouble(1)-1)<=.02;};
    return channel["available"].toBool()&&near("volume")&&near("eqLow")&&near("eqMid")&&near("eqHigh")&&int(std::lround(channel["orientation"].toDouble(1)))==1;
}

/// J path latency the next DJ adds before the venue: previous DJ -> next DJ
/// (measured as how far J renders behind the session clock) plus the standby
/// stream back to the host. The venue delay must cover both with margin.
struct LatencyBudget {
    static constexpr double kMarginMs=60;
    static double pathMs(double junctionLagMs,double hostRttMs,bool hostIsReceiver) {
        return hostIsReceiver?junctionLagMs:junctionLagMs+hostRttMs/2+kMarginMs;
    }
    /// Program enqueues a block `delay/2` after its media frame.
    static double budgetMs(quint32 delayFrames48k) {return double(delayFrames48k)/2/48.0;}
};

/// Tail lock table for the OUTGOING DJ. The tail is the set of deck channels
/// that were on Program when the turn changed; they keep sounding on the new
/// DJ's J, so only loops may still change them. Every other deck is local
/// preparation and never reaches anyone else.
struct TailLock {
    static int deckIndex(const QString& name) {
        const auto n=name.trimmed().toUpper();
        return n=="A"?0:n=="B"?1:n=="C"?2:n=="D"?3:-1;
    }
    /// Empty when allowed; otherwise the DJ-facing reason.
    static QString check(const QString& op,const QJsonObject& params,const QSet<int>& tailDecks) {
        // Program-wide controls shape the tail's post-fader level.
        if(op=="mixer.crossfader"||op=="mixer.master.gain"||op=="mixer.beatfx.set")
            return QStringLiteral("残りの曲を送出中はマスター系を操作できません");
        if(op=="deck.sync.set"&&params["leader"].isString()&&tailDecks.contains(deckIndex(params["leader"].toString())))
            return QStringLiteral("残りの曲をSYNCのリーダーにはできません");
        const bool deckOp=op.startsWith("deck.")||op.startsWith("mixer.channel.")||op=="mixer.eq.set"||op=="mixer.filter.set"||op=="mixer.trim.set"||op=="mixer.fx.set"||op=="mixer.colorfx.set";
        if(!deckOp)return {};
        const int deck=deckIndex(params["deck"].toString());
        if(deck<0||!tailDecks.contains(deck))return {};
        if(op.startsWith("deck.loop.")||op=="mixer.channel.pfl"||op=="deck.timing.trace")return {};
        return QStringLiteral("このデッキは残りの曲として送出中です。ループ以外は操作できません");
    }
};

/// Ends the tail on the sender when there is nothing left to hand over: every
/// tail deck stopped or ran out, and the masked send stayed silent.
class TailEnded {
public:
    static constexpr qint64 kSilentNanos=3000000000LL;
    void reset() {silent_.reset();}
    bool observe(bool anyTailDeckPlaying,float sendPeak,qint64 now) {
        if(anyTailDeckPlaying||sendPeak>=FaderStart::kThreshold){silent_.reset();return false;}
        if(!silent_){silent_=now;return false;}
        return now-*silent_>=kSilentNanos;
    }
private:
    std::optional<qint64> silent_;
};

/// The timetable. `order` is rosterOrder and always lists every participant:
/// the ON AIR DJ first, then the queue, then DJs who are out of the queue
/// (`out`: finished without repeat). `eligible` answers whether a peer can
/// take a turn now (connected and able to negotiate fader start).
template<class Eligible>
inline QString nextInOrder(const QStringList& order,const QString& owner,const QString& outgoing,const QStringList& out,Eligible eligible) {
    for(const auto& id:order){if(id.isEmpty()||id==owner||id==outgoing||out.contains(id))continue;if(eligible(id))return id;}
    return {};
}
/// The new ON AIR DJ moves to the head; the previous one goes behind the
/// queue. Whether they are out of the queue (finished) or queued again (B2B
/// repeat) is the caller's `out` list.
inline QStringList afterTurn(QStringList order,const QString& newOwner,const QString& previous,const QStringList& out) {
    order.removeAll(newOwner);order.prepend(newOwner);
    if(previous.isEmpty()||previous==newOwner)return order;
    order.removeAll(previous);
    if(out.contains(previous)){order.append(previous);return order;}
    int at=order.size();while(at>1&&out.contains(order[at-1]))--at;
    order.insert(at,previous);return order;
}
/// Joining the queue: behind every queued DJ, ahead of those out of it.
inline QStringList joined(QStringList order,const QString& owner,const QString& id,const QStringList& out) {
    if(id.isEmpty()||id==owner)return order;
    order.removeAll(id);int at=order.size();while(at>0&&out.contains(order[at-1])&&order[at-1]!=owner)--at;
    order.insert(at,id);return order;
}
/// Skip: the next DJ goes to the back of the queue.
inline QStringList skipped(QStringList order,const QString& owner,const QString& skippedId,const QStringList& out) {
    if(skippedId.isEmpty()||skippedId==owner||!order.contains(skippedId))return order;
    return joined(order,owner,skippedId,out);
}
}
