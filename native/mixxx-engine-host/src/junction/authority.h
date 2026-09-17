#pragma once
#include "session_protocol.h"
#include <QSet>
#include "turn_state.h"
namespace junction {
class Authority {
public:
    QString sessionId,host,local,owner,next;
    quint64 epoch=1,revision=0;
    QString phase="playing";
    /// This computer's master is still delivered after losing ON AIR (an
    /// OUTGOING DJ awaiting release). Only the tail decks are locked.
    bool sending=false;
    /// Every computer operates only its own mixer; ownership decides which
    /// mixer reaches the venue, not which operations apply. The only refusal
    /// is the OUTGOING tail lock on `tailDecks`.
    QSet<int> tailDecks;
    static bool localOnly(const QString& op);
    /// Non-mutating queries that never touch performance state.
    static bool readOnlyQuery(const QString& op);
    QString authorize(const QString& op,const QJsonObject& params={}) const;
};
}
