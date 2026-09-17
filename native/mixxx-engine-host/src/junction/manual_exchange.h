#pragma once
// Manual (signalling-free) Junction admission packets.
//
// A manual session never contacts a signalling server. The host and every
// guest exchange three kinds of bounded text packet through the clipboard:
//
//   invite    host -> guest   session identity + both non-trickle offers
//   response  guest -> host   both non-trickle answers
//   notice    host -> guest   signed rejection / cancellation / expiry
//
// Security notes (03 §1, §3):
//  * Packets carry no long-term secret. The only credentials permitted are
//    short-lived (<= 24 h) ICE credentials, validated on import.
//  * Host-originated packets are signed with the host's DTLS private key and
//    carry its certificate. A guest pins the certificate fingerprint from the
//    first invite and thereafter refuses any packet that does not verify
//    against it, so a cancellation notice cannot be forged by a third party.
//  * The SDP fingerprint is cross-checked against the declared fingerprint in
//    the packet, so a packet can never authorise a DTLS identity it does not
//    itself name.
//  * Every packet is parsed with hard bounds before any of it is trusted.
#include "ids.h"
#include "session_protocol.h"
#include <QJsonArray>
#include <QJsonObject>
#include <QString>
#include <optional>

namespace junction {

/// Hard ceiling for a pasted/imported packet, matching the gateway budget.
inline constexpr int kMaxExchangeTextBytes = 128 * 1024;
/// Decoded JSON ceiling. base64url costs 4/3, so this always fits the above.
inline constexpr int kMaxExchangeJsonBytes = 90 * 1024;
/// A single SDP, including all aggregated non-trickle candidates.
inline constexpr int kMaxExchangeSdpBytes = 32 * 1024;
/// An invitation may not be minted further ahead than this.
inline constexpr qint64 kMaxExchangeLifetimeMs = 7LL * 24 * 60 * 60 * 1000;

QString exchangeTextPrefix();

enum class ExchangeKind { Invite, Response, Notice };

/// Lifecycle of one per-DJ connection attempt, as surfaced in the snapshot.
/// The UI derives its actions from this; it never re-derives protocol state.
enum class ExchangeState {
    Idle, Collecting, InviteReady, AwaitingAnswer, ApprovalPending, ResponseReady,
    AwaitingHost, Connecting, Connected, Interrupted, NeedsExchange, Failed,
    Expired, Cancelled, Rejected,
};
QString exchangeStateName(ExchangeState state);
std::optional<ExchangeState> exchangeStateFromName(const QString& name);
/// True once the attempt can no longer progress without a fresh exchange.
bool exchangeStateTerminal(ExchangeState state);

struct ExchangeDescription {
    QString type, sdp;
    bool isEmpty() const { return sdp.isEmpty(); }
};

struct ExchangePacket {
    ExchangeKind kind = ExchangeKind::Invite;
    QString sessionId, sessionName, inviteId;
    QString hostPeerId, hostName, hostFingerprint;
    QString peerId, peerName, peerFingerprint;
    quint64 generation = 0, attempt = 0;
    qint64 expiresAt = 0;
    // [0] is the live control/media connection, [1] the bulk transfer one.
    ExchangeDescription description[2];
    QString noticeReason, noticeText;
    QJsonArray iceServers;
    QByteArray certificatePem, signature;

    /// Canonical signed body. QJsonObject orders keys, so QJsonDocument's
    /// compact form is deterministic on both ends.
    QJsonObject payload() const;
    QByteArray canonicalPayload() const;
    /// The subset `exchange.inspect` may reveal before anything is trusted.
    QJsonObject sanitized() const;
    /// The declared DTLS fingerprint of whoever produced the descriptions.
    QString descriptionFingerprint() const;
};

/// Extracts the `a=fingerprint:sha-256` value of an SDP as lowercase hex with
/// no separators. Empty when absent or not SHA-256.
QString sdpFingerprint(const QString& sdp);

/// Serialises to `PLUMDECK-JUNCTION-1.<base64url>`. Fails rather than truncate.
QString encodeExchangePacket(const ExchangePacket& packet, QString* error = nullptr);

/// Bounded parse. `errorCode` receives a stable machine code
/// (too_large/invalid/unsupported_version/schema/expired) for the snapshot.
std::optional<ExchangePacket> decodeExchangePacket(const QString& text, qint64 nowMs,
                                                   QString* error = nullptr,
                                                   QString* errorCode = nullptr);

/// Validates incoming short-lived ICE credentials. Rejects long-term secrets,
/// unbounded lifetimes, oversized fields and non-STUN/TURN schemes. Returns a
/// sanitised array, or std::nullopt with a reason.
std::optional<QJsonArray> validateExchangeIceServers(const QJsonArray& servers, qint64 nowMs,
                                                     QString* error = nullptr);

/// ECDSA-SHA256 over `canonicalPayload()` with the host's DTLS private key.
QByteArray signExchangePayload(const QString& privateKeyPemPath, const QByteArray& payload,
                               QString* error = nullptr);
/// Verifies the signature *and* that the embedded certificate hashes to
/// `pinnedFingerprint`. Both must hold; neither alone is sufficient.
bool verifyExchangeSignature(const ExchangePacket& packet, const QString& pinnedFingerprint,
                             QString* error = nullptr);
QByteArray readCertificatePem(const QString& certificatePemPath);

} // namespace junction
