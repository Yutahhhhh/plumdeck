#pragma once
#include <QJsonObject>
#include <QString>
namespace junction {
/// Program Master routing, stated in the terms the DJ sees.
///
/// - JUNCTION MASTER: the previous DJ's master arriving over P2P. A virtual
///   input channel (Auxiliary), never a file deck, never "the next track".
/// - LOCAL NEXT: the local decks the receiving DJ is about to play.
/// - Program Master: the only bus that may reach the venue.
///
/// This is the single decision point the runtime publishes and guards with.
/// It does not move audio; it says where audio is allowed to go, so the UI and
/// tests can see the same boundary the engine enforces. The audio itself moves
/// in the Mixxx engine: JUNCTION MASTER is Auxiliary1 (fed in the callback's
/// before-hook), Program Master is the captured main bus, and the return feed
/// is the engine's LOCAL NEXT bus.
enum class VenueSource {
    None,        ///< No Program output open on this computer.
    RemoteHost,  ///< This computer is a guest; the host owns the venue output.
    /// The remote operator's master is delivered to the venue output without
    /// passing through this computer's mixer. Existing engine path, kept until
    /// Program Master can carry a remote operator (see docs, "未実装").
    DirectStream,
    LocalMix,    ///< Program Master = this mixer: JUNCTION MASTER + LOCAL NEXT.
};
enum class ReturnSource {
    None,
    LocalPlay,   ///< This computer's own local play, never Program and never JUNCTION MASTER.
    RelayedPeer, ///< The host forwards an outgoing Lite DJ's stream to the next Lite DJ.
};
struct ProgramMixerInputs {
    bool hosting=false,programOpen=false;
    /// This computer holds the operator role (controls what Program carries).
    bool localOperator=false;
    /// A remote DJ feeds the JUNCTION MASTER channel.
    bool junctionMasterPresent=false;
    /// The JUNCTION MASTER channel is routed into this mixer's main bus.
    bool junctionMasterInMain=false;
    /// The previous DJ keeps sounding until the operator fades them out.
    bool releasing=false;
    bool returnRequested=false,returnRelayed=false;
    /// The engine renders a LOCAL NEXT bus that structurally excludes
    /// JUNCTION MASTER. Without one the return is the main bus and is only
    /// safe while JUNCTION MASTER is out of main.
    bool localReturnBus=false;
};
struct ProgramMixerRoute {
    VenueSource venue=VenueSource::None;
    bool junctionMasterInProgram=false;
    bool localNextInProgram=false;
    /// LOCAL NEXT is audible on CUE only (headphones), never on Program.
    bool localNextCueOnly=false;
    ReturnSource returnSource=ReturnSource::None;
    /// A local return was requested while JUNCTION MASTER was in main; it is
    /// refused because it would send the remote DJ their own audio back.
    bool feedbackBlocked=false;
    bool localReturnBus=false;
    bool releasing=false;

    static ProgramMixerRoute resolve(const ProgramMixerInputs& in) noexcept {
        ProgramMixerRoute r;
        if(!in.hosting)r.venue=VenueSource::RemoteHost;
        else if(!in.programOpen)r.venue=VenueSource::None;
        else r.venue=in.localOperator?VenueSource::LocalMix:VenueSource::DirectStream;
        // Only the operator's mixer contributes to Program. Before takeover the
        // receiver's faders are rehearsal: CUE only.
        r.junctionMasterInProgram=in.localOperator&&in.junctionMasterPresent&&in.junctionMasterInMain;
        r.localNextInProgram=in.localOperator;
        r.localNextCueOnly=!in.localOperator;
        r.releasing=in.releasing;
        r.localReturnBus=in.localReturnBus;
        if(in.returnRequested){
            if(in.returnRelayed)r.returnSource=ReturnSource::RelayedPeer;
            else if(in.junctionMasterInMain&&!in.localReturnBus)r.feedbackBlocked=true;
            else r.returnSource=ReturnSource::LocalPlay;
        }
        return r;
    }
    /// A local return may be captured from this mixer's main bus.
    bool localReturnAllowed() const noexcept {return returnSource==ReturnSource::LocalPlay;}
    static QString venueName(VenueSource v) {
        switch(v){
        case VenueSource::RemoteHost:return QStringLiteral("remote-host");
        case VenueSource::DirectStream:return QStringLiteral("direct-stream");
        case VenueSource::LocalMix:return QStringLiteral("local-mix");
        case VenueSource::None:break;
        }
        return QStringLiteral("none");
    }
    static QString returnName(ReturnSource s) {
        return s==ReturnSource::LocalPlay?QStringLiteral("local-play"):s==ReturnSource::RelayedPeer?QStringLiteral("relayed-peer"):QStringLiteral("none");
    }
    QJsonObject toJson(const QString& junctionMasterPeer,const QString& releasingPeer,const QString& returnTarget) const {
        return {
            {"venueSource",venueName(venue)},
            {"directStreamBypass",venue==VenueSource::DirectStream},
            {"junctionMaster",QJsonObject{{"peerId",junctionMasterPeer},{"inProgram",junctionMasterInProgram},{"releasingPeerId",releasingPeer}}},
            {"localNext",QJsonObject{{"inProgram",localNextInProgram},{"cueOnly",localNextCueOnly}}},
            {"returnFeed",QJsonObject{{"source",returnName(returnSource)},{"targetPeerId",returnSource==ReturnSource::None?QString{}:returnTarget},{"feedbackBlocked",feedbackBlocked},
                {"tap",localReturnBus?QStringLiteral("local-next-bus"):QStringLiteral("main-bus")}}},
        };
    }
};
}
