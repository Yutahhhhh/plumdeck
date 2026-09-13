#include "harness.h"
#include <limits>
#include "junction/authority.h"
#include "junction/validation.h"
using namespace junction;
namespace {
Authority authority(){Authority a;a.sessionId="session-test";a.local=a.host=a.owner="peer-host";return a;}
QJsonObject ticket(const Authority&a,quint64 epoch){return {{"sessionId",a.sessionId},{"actorPeerId",a.local},{"epoch",u64(epoch)}};}
HandoffCommitMessage commit(const Authority&a){HandoffCommitMessage m;m.sessionId=a.sessionId;m.handoffId=a.handoffId;m.oldEpoch=a.epoch;m.newEpoch=a.epoch+1;m.oldOwner=a.owner;m.newOwner=a.next;m.fencedAtMediaFrame=a.fenceFrame;m.effectiveMediaFrame=2000;m.lastAppliedSeq=a.throughSeq;m.capsuleRevision=a.revision;return m;}
}
JTEST("authority","fence rejects queued mutations but preserves local PFL"){
 auto a=authority();CHECK(a.prepare("peer-next").isEmpty());CHECK(a.fence(1000,7).isEmpty());
 CHECK(a.authorize("deck.play",ticket(a,1),999).isEmpty());
 CHECK(!a.authorize("deck.play",ticket(a,1),1000).isEmpty());
 CHECK(a.authorize("mixer.channel.pfl",{},1000).isEmpty());
 CHECK(!a.authorize("audio.config.set",ticket(a,1),1000).isEmpty());
 CHECK(!a.authorize("deck.play",ticket(a,0),999).isEmpty());
}
JTEST("authority","future commit is immutable and transfers authority only at H"){
 auto a=authority();CHECK(a.prepare("peer-next").isEmpty());CHECK(a.fence(1000,7).isEmpty());auto m=commit(a);
 CHECK(!a.commit(m,"peer-other").isEmpty());CHECK(a.commit(m,a.host).isEmpty());CHECK(a.commit(m,a.host).isEmpty());
 auto changed=m;changed.effectiveMediaFrame++;CHECK(!a.commit(changed,a.host).isEmpty());CHECK(!a.cancel().isEmpty());
 a.advance(1999);CHECK_EQ(a.owner,QString("peer-host"));CHECK_EQ(a.epoch,quint64(1));
 a.advance(2000);CHECK_EQ(a.owner,QString("peer-next"));CHECK_EQ(a.epoch,quint64(2));
 CHECK(!a.authorize("deck.play",ticket(a,1),2000).isEmpty());a.local="peer-next";
 CHECK(a.authorize("deck.play",ticket(a,2),2000).isEmpty());
}
JTEST("authority","watermark mismatch cannot commit and cancellation preserves performer"){
 auto a=authority();CHECK(a.prepare("peer-next").isEmpty());CHECK(a.fence(1000,8).isEmpty());auto m=commit(a);m.lastAppliedSeq=7;
 CHECK(!a.commit(m,a.host).isEmpty());CHECK(a.cancel().isEmpty());CHECK_EQ(a.owner,QString("peer-host"));CHECK_EQ(a.epoch,quint64(1));
 CHECK(a.authorize("deck.play",ticket(a,1),3000).isEmpty());
}
JTEST("validation","correlation cannot hide wrong level timing or nonfinite PCM"){
 std::vector<float>a(24000),b(24000);for(size_t i=0;i<a.size()/2;++i){a[i*2]=.2f*std::sin(double(i)*.017);a[i*2+1]=.17f*std::sin(double(i)*.031);}
 CHECK(compareAudio(a,a).ready);b=a;for(auto&v:b)v*=.5f;CHECK(!compareAudio(a,b).ready);
 b=a;for(size_t i=4;i<b.size();++i)b[i]=a[i-4];CHECK(!compareAudio(a,b).ready);
 b=a;b[2000]=std::numeric_limits<float>::quiet_NaN();CHECK(!compareAudio(a,b).ready);
 std::fill(a.begin(),a.end(),0);CHECK(compareAudio(a,a).ready);
}

JTEST("validation", "fractional sample alignment preserves the correlation level and lag gates") {
 std::vector<float> original(24000),shifted(24000),wrong(24000);
 const auto sample=[](double frame,int channel){double value=0;for(int tone=0;tone<48;tone++){const double omega=.04+tone*.047;value+=.012*std::sin(frame*omega+tone*tone*.31+channel*.6);}return float(value);};
 for(int frame=0;frame<12000;frame++)for(int c=0;c<2;c++){original[frame*2+c]=sample(frame,c);shifted[frame*2+c]=sample(frame-.25,c);wrong[frame*2+c]=sample(frame-1.25,c);}
 const auto aligned=compareAudio(original,shifted);CHECK(aligned.ready);CHECK_NEAR(aligned.fractionalLagFrames,.25,.0625);CHECK(aligned.correlation>=.999);CHECK(!compareAudio(original,wrong).ready);
 for(auto& value:shifted)value*=1.1;CHECK(!compareAudio(original,shifted).ready);
 CHECK(!compareAudio(original,original,-1).ready);CHECK(!compareAudio(original,original,100000).ready);
}

JTEST("authority","waiting DJ can query waveform and timing without mutation authority"){
 auto a=authority();a.local="waiting-dj";
 for(const auto* op:{"waveform.ensure","waveform.manifest","engine.clock.probe","deck.timing.trace"})CHECK(a.authorize(op,{},100).isEmpty());
 CHECK(!a.authorize("deck.seek",ticket(a,a.epoch),100).isEmpty());CHECK(!a.authorize("deck.load",ticket(a,a.epoch),100).isEmpty());
}
JTEST("authority","a waiting DJ can read waveform files of its own loaded deck but not change shared decks"){
 auto a=authority();a.owner="peer-other";
 for(const auto* op:{"waveform.ensure","waveform.manifest","waveform.acquireReadLease","waveform.releaseReadLease","waveform.requestRange","waveform.cancelRequest","waveform.invalidate"}){CHECK(Authority::readOnlyQuery(op));CHECK(a.authorize(op,{},10).isEmpty());}
 CHECK(!a.authorize("deck.load",{},10).isEmpty());CHECK(!a.authorize("deck.load",ticket(a,1),10).isEmpty());CHECK(!a.authorize("junction.tracks.load",{},10).isEmpty());
}
