#include "harness.h"
#include "junction/program_mixer.h"
#include <QJsonArray>
using namespace junction;
namespace {
ProgramMixerInputs host(){ProgramMixerInputs in;in.hosting=true;in.programOpen=true;return in;}
}
JTEST("program_mixer","receiver before takeover keeps JUNCTION MASTER and LOCAL NEXT off Program"){
    auto in=host();in.localOperator=false;in.junctionMasterPresent=true;in.junctionMasterInMain=false;
    const auto r=ProgramMixerRoute::resolve(in);
    CHECK_EQ(r.venue,VenueSource::DirectStream);
    CHECK(!r.junctionMasterInProgram);
    CHECK(!r.localNextInProgram);
    CHECK(r.localNextCueOnly);
}
JTEST("program_mixer","operator Program Master carries JUNCTION MASTER and LOCAL NEXT only through the mixer"){
    auto in=host();in.localOperator=true;in.junctionMasterPresent=true;in.junctionMasterInMain=true;in.releasing=true;
    const auto r=ProgramMixerRoute::resolve(in);
    CHECK_EQ(r.venue,VenueSource::LocalMix);
    CHECK(r.junctionMasterInProgram);
    CHECK(r.localNextInProgram);
    CHECK(!r.localNextCueOnly);
    CHECK(r.releasing);
    CHECK_EQ(r.toJson("lite","lite","")["venueSource"].toString(),QString("local-mix"));
}
JTEST("program_mixer","a JUNCTION MASTER channel outside main never reaches Program"){
    auto in=host();in.localOperator=true;in.junctionMasterPresent=true;in.junctionMasterInMain=false;
    CHECK(!ProgramMixerRoute::resolve(in).junctionMasterInProgram);
    in.junctionMasterPresent=false;in.junctionMasterInMain=true;
    CHECK(!ProgramMixerRoute::resolve(in).junctionMasterInProgram);
}
JTEST("program_mixer","return feed is local play only and refuses JUNCTION MASTER feedback"){
    auto in=host();in.localOperator=false;in.returnRequested=true;
    auto r=ProgramMixerRoute::resolve(in);
    CHECK_EQ(r.returnSource,ReturnSource::LocalPlay);CHECK(r.localReturnAllowed());CHECK(!r.feedbackBlocked);
    CHECK_EQ(r.toJson("","","phone")["returnFeed"].toObject()["targetPeerId"].toString(),QString("phone"));
    in.junctionMasterInMain=true;
    r=ProgramMixerRoute::resolve(in);
    CHECK_EQ(r.returnSource,ReturnSource::None);CHECK(!r.localReturnAllowed());CHECK(r.feedbackBlocked);
    CHECK(r.toJson("","","phone")["returnFeed"].toObject()["targetPeerId"].toString().isEmpty());
    in.returnRelayed=true;
    r=ProgramMixerRoute::resolve(in);
    CHECK_EQ(r.returnSource,ReturnSource::RelayedPeer);CHECK(!r.localReturnAllowed());
}
JTEST("program_mixer","a LOCAL NEXT bus returns local play even while JUNCTION MASTER is in Program"){
    auto in=host();in.localOperator=true;in.junctionMasterPresent=true;in.junctionMasterInMain=true;in.returnRequested=true;in.localReturnBus=true;
    const auto r=ProgramMixerRoute::resolve(in);
    CHECK(r.junctionMasterInProgram);
    CHECK_EQ(r.returnSource,ReturnSource::LocalPlay);CHECK(r.localReturnAllowed());CHECK(!r.feedbackBlocked);
    CHECK_EQ(r.toJson("lite","","phone")["returnFeed"].toObject()["tap"].toString(),QString("local-next-bus"));
    in.localReturnBus=false;
    CHECK_EQ(ProgramMixerRoute::resolve(in).toJson("lite","","phone")["returnFeed"].toObject()["tap"].toString(),QString("main-bus"));
}
JTEST("program_mixer","guests and closed outputs never claim the venue"){
    ProgramMixerInputs guest;guest.localOperator=true;
    CHECK_EQ(ProgramMixerRoute::resolve(guest).venue,VenueSource::RemoteHost);
    auto closed=host();closed.programOpen=false;closed.localOperator=true;
    CHECK_EQ(ProgramMixerRoute::resolve(closed).venue,VenueSource::None);
    CHECK_EQ(ProgramMixerRoute::venueName(VenueSource::None),QString("none"));
}
