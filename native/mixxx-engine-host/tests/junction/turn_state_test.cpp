#include "harness.h"
#include "junction/turn_state.h"
using namespace junction::turn;
namespace {
Roles roles(const QString& local){Roles r;r.active=true;r.local=local;r.owner="a";r.next="b";r.outgoing="z";return r;}
ReadyInputs allReady(){ReadyInputs in;in.junctionReceiving=true;in.junctionStable=true;in.latencyMeasured=true;in.pathLatencyMs=80;in.budgetMs=250;in.standbyStreamRequired=true;in.standbyStreamReceived=true;in.junctionUnity=true;in.localSilent=true;return in;}
QJsonObject unity(){return {{"available",true},{"volume",1.0},{"eqLow",1.0},{"eqMid",1.0},{"eqHigh",1.0},{"orientation",1}};}
}
JTEST("turn","signal light follows the existing roles"){
 CHECK(derive(roles("a"),false)==Signal::OnAir);
 CHECK(derive(roles("b"),false)==Signal::Standby);
 CHECK(derive(roles("b"),true)==Signal::Ready);
 CHECK(derive(roles("z"),true)==Signal::Outgoing);
 CHECK(derive(roles("c"),true)==Signal::Off);
 auto inactive=roles("a");inactive.active=false;CHECK(derive(inactive,true)==Signal::Off);
 CHECK_EQ(signalName(Signal::Ready),QString("ready"));CHECK_EQ(signalName(Signal::OnAir),QString("onair"));
}
JTEST("turn","ready shows exactly one blocker in fix-first order"){
 CHECK(readyBlocker(allReady())==Blocker::None);
 auto in=allReady();in.localSilent=false;in.junctionUnity=false;CHECK(readyBlocker(in)==Blocker::JunctionNotUnity);
 in.junctionUnity=true;CHECK(readyBlocker(in)==Blocker::AlreadyAudible);CHECK_EQ(blockerText(readyBlocker(in)),QString("一度フェーダーを下げてください"));
 in=allReady();in.junctionReceiving=false;in.compatible=false;CHECK(readyBlocker(in)==Blocker::UpdateRequired);
 in.compatible=true;CHECK(readyBlocker(in)==Blocker::WaitingJunction);
 in.junctionRequired=false;CHECK(readyBlocker(in)==Blocker::None);
 in=allReady();in.pathLatencyMs=251;CHECK(readyBlocker(in)==Blocker::LatencyBudget);
 in=allReady();in.standbyStreamReceived=false;CHECK(readyBlocker(in)==Blocker::HostWaitingStream);in.standbyStreamRequired=false;CHECK(readyBlocker(in)==Blocker::None);
 in=allReady();in.programOpen=false;in.latencyMeasured=false;CHECK(readyBlocker(in)==Blocker::ProgramClosed);
 for(auto b:{Blocker::UpdateRequired,Blocker::ProgramClosed,Blocker::WaitingJunction,Blocker::JunctionUnstable,Blocker::LatencyUnknown,Blocker::LatencyBudget,Blocker::HostWaitingStream,Blocker::JunctionNotUnity,Blocker::AlreadyAudible}){CHECK(!blockerCode(b).isEmpty());CHECK(!blockerText(b).isEmpty());}
}
JTEST("turn","fader start fires once on a silence to sound edge"){
 CHECK_NEAR(FaderStart::kThreshold,.00316,.00001);CHECK_EQ(FaderStart::kOnsetNanos,20000000LL);CHECK_EQ(FaderStart::kArmNanos,300000000LL);
 FaderStart f;const qint64 ms=1000000;
 // Arriving with a fader up never fires, however long it stays up.
 for(qint64 t=0;t<=1000;t+=5)CHECK(!f.observe(.5f,false,t*ms));
 CHECK(!f.armed());
 for(qint64 t=1005;t<1300;t+=5)CHECK(!f.observe(0,false,t*ms));
 CHECK(!f.armed());CHECK(!f.observe(0,false,1305*ms));CHECK(f.armed());
 CHECK(!f.observe(.01f,false,1310*ms));CHECK(!f.observe(.01f,false,1325*ms));
 CHECK(f.observe(.01f,false,1330*ms));
 CHECK(!f.observe(.01f,false,1335*ms));CHECK(!f.armed());
}
JTEST("turn","a short click does not fire and a J move counts as sound"){
 FaderStart f;const qint64 ms=1000000;
 CHECK(!f.observe(0,false,0));CHECK(!f.observe(0,false,400*ms));CHECK(f.armed());
 CHECK(!f.observe(.2f,false,405*ms));CHECK(!f.observe(0,false,410*ms));CHECK(!f.observe(.2f,false,415*ms));CHECK(!f.observe(.2f,false,430*ms));
 CHECK(f.observe(.2f,false,435*ms));
 FaderStart j;CHECK(!j.observe(0,false,0));CHECK(!j.observe(0,false,300*ms));CHECK(!j.observe(0,true,301*ms));CHECK(j.observe(0,true,321*ms));
 CHECK(!j.observe(FaderStart::kThreshold*.9f,false,400*ms));
}
JTEST("turn","J unity accepts only unity flat THRU"){
 CHECK(junctionUnity(unity()));
 auto c=unity();c["volume"]=.9;CHECK(!junctionUnity(c));
 c=unity();c["eqLow"]=0.0;CHECK(!junctionUnity(c));
 c=unity();c["orientation"]=0;CHECK(!junctionUnity(c));
 c=unity();c["available"]=false;CHECK(!junctionUnity(c));
}
JTEST("turn","latency budget covers the relay and the standby stream"){
 CHECK_NEAR(LatencyBudget::budgetMs(24000),250,.001);
 CHECK_NEAR(LatencyBudget::pathMs(100,40,false),100+20+LatencyBudget::kMarginMs,.001);
 CHECK_NEAR(LatencyBudget::pathMs(100,40,true),100,.001);
}
JTEST("turn","tail decks accept loops only and the rest of the mixer stays free"){
 const QSet<int> tail{0};
 const auto deck=[](const char* d){return QJsonObject{{"deck",d}};};
 for(const char* op:{"deck.loop.set","deck.loop.enable","deck.loop.beat","mixer.channel.pfl"})CHECK(TailLock::check(op,deck("A"),tail).isEmpty());
 for(const char* op:{"deck.play","deck.seek","deck.hotcue.jump","deck.scratch","deck.pitchbend","deck.tempo.set","deck.key.shift","deck.sync.set","deck.load","deck.unload","deck.beatjump","mixer.channel.gain","mixer.channel.eq","mixer.eq.set","mixer.filter.set","mixer.fx.set","mixer.colorfx.set","mixer.channel.orientation","mixer.trim.set"})
  CHECK(!TailLock::check(op,deck("A"),tail).isEmpty());
 for(const char* op:{"deck.play","deck.load","mixer.channel.gain","deck.sync.set"})CHECK(TailLock::check(op,deck("B"),tail).isEmpty());
 for(const char* op:{"mixer.crossfader","mixer.master.gain","mixer.beatfx.set"})CHECK(!TailLock::check(op,{},tail).isEmpty());
 CHECK(!TailLock::check("deck.sync.set",QJsonObject{{"deck","B"},{"leader","A"}},tail).isEmpty());
 CHECK(TailLock::check("sampler.play",{},tail).isEmpty());
}
JTEST("turn","tail ends only after the tail stops and stays silent"){
 TailEnded t;const qint64 s=1000000000LL;
 CHECK(!t.observe(true,0,0));CHECK(!t.observe(false,.1f,1*s));CHECK(!t.observe(false,0,2*s));CHECK(!t.observe(false,0,4*s));
 CHECK(!t.observe(false,.5f,4*s+1));CHECK(!t.observe(false,0,5*s));CHECK(t.observe(false,0,8*s));
}
JTEST("turn","timetable picks the next eligible DJ and repeat keeps B2B looping"){
 const QStringList order{"a","b","c","d"};
 const auto all=[](const QString&){return true;};
 CHECK_EQ(nextInOrder(order,"a","",{},all),QString("b"));
 CHECK_EQ(nextInOrder(order,"a","b",{},all),QString("c"));
 CHECK_EQ(nextInOrder(order,"a","",{"b"},all),QString("c"));
 CHECK_EQ(nextInOrder(order,"a","",{},[](const QString& id){return id=="d";}),QString("d"));
 CHECK_EQ(nextInOrder(QStringList{"a"},"a","",{},all),QString());
 CHECK(afterTurn(order,"b","a",{})==(QStringList{"b","c","d","a"}));
 CHECK(afterTurn(QStringList{"a","b","c","d"},"b","a",{"d"})==(QStringList{"b","c","a","d"}));
 CHECK(afterTurn(QStringList{"a","b","c","d"},"b","a",{"a","d"})==(QStringList{"b","c","d","a"}));
 CHECK(afterTurn(QStringList{"a","b"},"b","a",{})==(QStringList{"b","a"}));
 CHECK(joined(QStringList{"a","b","c","d"},"a","d",{"c","d"})==(QStringList{"a","b","d","c"}));
 CHECK(joined(QStringList{"a","c"},"a","c",{"c"})==(QStringList{"a","c"}));
 CHECK(skipped(QStringList{"a","b","c","d"},"a","b",{"d"})==(QStringList{"a","c","b","d"}));
 CHECK(skipped(QStringList{"a","b"},"a","a",{})==(QStringList{"a","b"}));
}
