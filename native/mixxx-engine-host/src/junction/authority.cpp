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
QString Authority::authorize(const QString& op,const QJsonObject& params) const {
    if(sessionId.isEmpty() || localOnly(op) || readOnlyQuery(op) || !sending) return {};
    return turn::TailLock::check(op,params,tailDecks);
}
}
