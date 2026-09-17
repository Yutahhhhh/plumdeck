#pragma once
#include "session_protocol.h"
#include <QSet>
#include "turn_state.h"
namespace junction {
class Authority {
public:
    QString sessionId,host,local,owner,next,handoffId;
    quint64 epoch=1,throughSeq=0,revision=0,fenceFrame=0,cutoverFrame=0;
    QString phase="playing";
    /// A Lite DJ performs: this computer's decks are local preparation only.
    bool localPrep=false;
    /// This computer's master is still delivered after losing the operator
    /// role (an outgoing DJ awaiting release). Its controls stay locked, even
    /// when `localPrep` is set; local-only PFL/headphone operations remain.
    bool sending=false;
    std::optional<HandoffCommitMessage> committed;
    /// Fader-start sessions: every computer operates only its own mixer, and
    /// ownership decides which mixer reaches the venue, not which operations
    /// apply. The only refusal is the OUTGOING tail lock on `tailDecks`.
    bool faderStart=false;
    QSet<int> tailDecks;
    static bool localOnly(const QString& op);
    /// Non-mutating queries. A peer that is still waiting for admission (or is
    /// simply not the current performer) must still be able to draw waveforms
    /// and read diagnostics, so these bypass the performer gate. They never
    /// touch shared performance state and never advance the control sequence.
    static bool readOnlyQuery(const QString& op);
    QString authorize(const QString& op,const QJsonObject& ticket,quint64 frame,const QJsonObject& params={}) const;
    QString prepare(const QString& target);
    QString fence(quint64 frame,quint64 watermark);
    QString commit(const HandoffCommitMessage& message,const QString& authenticatedSender);
    QString cancel();
    void advance(quint64 frame);
    QString recover(const QString& producer,quint64 newEpoch,quint64 frame);
};
}
