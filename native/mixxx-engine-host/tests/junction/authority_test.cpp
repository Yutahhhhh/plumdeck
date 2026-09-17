#include "harness.h"
#include "junction/authority.h"
using namespace junction;
namespace {
Authority authority(){Authority a;a.sessionId="session-test";a.local=a.host=a.owner="peer-host";return a;}
}
JTEST("authority","waiting DJ can query waveform and timing"){
 auto a=authority();a.local="waiting-dj";a.sending=true;a.tailDecks={0};
 for(const auto* op:{"waveform.ensure","waveform.manifest","waveform.acquireReadLease","waveform.releaseReadLease","waveform.requestRange","waveform.cancelRequest","waveform.invalidate","engine.clock.probe","deck.timing.trace"}){CHECK(Authority::readOnlyQuery(op));CHECK(a.authorize(op,QJsonObject{{"deck","A"}}).isEmpty());}
}
JTEST("authority","fader start never refuses a DJ's own mixer and locks only the outgoing tail"){
 auto a=authority();a.local="peer-b";a.owner="peer-a";
 // A turn change moves the epoch; the fader move that caused it must keep applying.
 CHECK(a.authorize("mixer.channel.gain",QJsonObject{{"deck","A"},{"gain",.8}}).isEmpty());
 CHECK(a.authorize("deck.play",QJsonObject{{"deck","B"}}).isEmpty());
 a.phase="recovery";CHECK(a.authorize("deck.load",QJsonObject{{"deck","C"}}).isEmpty());
 a.phase="playing";a.sending=true;a.tailDecks={0};
 CHECK(!a.authorize("deck.play",QJsonObject{{"deck","A"}}).isEmpty());
 CHECK(a.authorize("deck.loop.enable",QJsonObject{{"deck","A"},{"enabled",false}}).isEmpty());
 CHECK(a.authorize("deck.load",QJsonObject{{"deck","B"}}).isEmpty());
 CHECK(!a.authorize("mixer.crossfader",QJsonObject{{"position",0.0}}).isEmpty());
 CHECK(a.authorize("mixer.channel.pfl",QJsonObject{{"deck","A"}}).isEmpty());
 // Released: nothing is locked any more.
 a.sending=false;CHECK(a.authorize("deck.play",QJsonObject{{"deck","A"}}).isEmpty());CHECK(a.authorize("mixer.crossfader",QJsonObject{{"position",0.0}}).isEmpty());
 // No session: nothing is Junction's to refuse.
 a.sending=true;a.sessionId.clear();CHECK(a.authorize("deck.play",QJsonObject{{"deck","A"}}).isEmpty());
}
