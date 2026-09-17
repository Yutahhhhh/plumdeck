#include "authority.h"
namespace junction {
bool Authority::localOnly(const QString& op) {
    static const QSet<QString> allowed={"engine.ping","state.snapshot","audio.devices.list","audio.config.get","sampler.state","sampler.bank","sampler.pfl","mixer.channel.pfl","mixer.headphone.gain","mixer.headphone.mix","deck.timing.trace","recording.directory.set","recording.format.set","recording.start","recording.stop"};
    return allowed.contains(op);
}
bool Authority::readOnlyQuery(const QString& op) {
    // Waveform leases/ranges read this computer's analysis cache for the deck
    // it already has loaded; they never touch playback or the shared graph.
    static const QSet<QString> allowed={"engine.clock.probe","waveform.ensure","waveform.manifest","waveform.acquireReadLease","waveform.releaseReadLease","waveform.requestRange","waveform.cancelRequest","waveform.invalidate","meters.subscribe","deck.timing.trace","state.snapshot"};
    return allowed.contains(op);
}
QString Authority::authorize(const QString& op,const QJsonObject& ticket,quint64 frame,const QJsonObject& params) const {
    if(sessionId.isEmpty() || localOnly(op) || readOnlyQuery(op)) return {};
    if(faderStart) return sending?turn::TailLock::check(op,params,tailDecks):QString{};
    if(ticket["sessionId"].toString()!=sessionId || ticket["actorPeerId"].toString()!=local) return "共有セッションの操作情報が更新されています";
    const auto requested=parseU64(ticket["epoch"]);
    if(!requested || *requested!=epoch) return "交代前の操作は適用できません";
    if(local!=owner&&(!localPrep||sending)) return "現在のプレイ担当者だけが操作できます";
    if(phase=="recovery") return "配信を復旧中です";
    if((phase=="fenced" || phase=="committed") && frame>=fenceFrame && (!committed || frame<cutoverFrame)) return "引き継ぎ中です。操作をもう一度行ってください";
    return {};
}
QString Authority::prepare(const QString& target) {
    if(phase!="playing" || target.isEmpty() || target==owner) return "次のDJを選択できません";
    next=target;handoffId=secureRandomHex(16);phase="preparing";++revision;return {};
}
QString Authority::fence(quint64 frame,quint64 watermark) {
    if(phase!="preparing") return "準備が完了していません";
    fenceFrame=frame;throughSeq=watermark;phase="fenced";++revision;return {};
}
QString Authority::commit(const HandoffCommitMessage& m,const QString& sender) {
    if(sender!=host || m.sessionId!=sessionId || !m.valid()) return "無効な引き継ぎ通知です";
    if(committed && committed->handoffId==m.handoffId && committed->newEpoch==m.newEpoch)
        return committed->toJson()==m.toJson()?QString{}:QStringLiteral("引き継ぎ通知が矛盾しています");
    if(phase!="fenced" || m.oldEpoch!=epoch || m.oldOwner!=owner || m.newOwner!=next || m.handoffId!=handoffId || m.lastAppliedSeq!=throughSeq || m.fencedAtMediaFrame!=fenceFrame) return "引き継ぎの状態が一致しません";
    committed=m;cutoverFrame=m.effectiveMediaFrame;phase="committed";++revision;return {};
}
QString Authority::cancel() {
    if(committed || (phase!="preparing" && phase!="fenced")) return "確定後の引き継ぎは取り消せません";
    phase="playing";next.clear();handoffId.clear();fenceFrame=0;++revision;return {};
}
void Authority::advance(quint64 frame) {
    if(phase=="committed" && committed && frame>=cutoverFrame) {owner=committed->newOwner;epoch=committed->newEpoch;phase="switching";++revision;}
}
QString Authority::recover(const QString& producer,quint64 newEpoch,quint64 frame) {
    if(local!=host || newEpoch<=epoch || (committed && newEpoch<=committed->newEpoch)) return "復旧には新しい操作権が必要です";
    owner=producer;epoch=newEpoch;cutoverFrame=frame;phase="recovery";next.clear();++revision;return {};
}
}
