#pragma once
// Junction control-channel schema (03 §4).
//
// This is a *network* protocol and is versioned independently of the local
// NDJSON engine protocol (which stays at protocol=1). Nothing here may carry a
// local filesystem path, a device identifier, an invite token, a private key or
// any other local-only value: those are listed explicitly in the design and are
// stripped by the encoders below rather than by convention.
//
// Every 64-bit counter is a decimal string on the wire. Every decoder validates
// shape *and* range before the value reaches any state machine, and returns a
// reason on failure instead of a partially filled struct.
#include "ids.h"
#include <QJsonObject>
#include <QString>
#include <optional>

namespace junction {

/// Message kinds carried on the reliable ordered control channel.
enum class MessageType {
    Unknown,
    PeerHello,
    SessionSnapshot,
    ClockProbeRequest,
    ClockProbeReply,
    SessionRecovery,
    HealthReport,
    PeerLeave,
    SessionEnd,
    /// Fader-start turn traffic; `payload.kind` names the event.
    Turn,
};

MessageType messageTypeFromName(const QString& name);

/// The common envelope. `senderPeerId` is checked against the authenticated
/// identity of the connection it arrived on: a payload cannot name itself host.
struct Envelope {
    int version = kJunctionProtocolVersion;
    QString sessionId;
    QString senderPeerId;
    quint64 epoch = 0;
    QString messageId;
    MessageType type = MessageType::Unknown;
    QJsonObject payload;
};

/// Why a frame was refused. Returned instead of throwing so every rejection
/// can be counted, rate limited and surfaced to the operator.
enum class DecodeError {
    None,
    NotAnObject,
    UnsupportedVersion,
    MissingField,
    BadField,
    UnknownType,
    TooLarge,
    IdentityMismatch,
};

struct DecodeResult {
    DecodeError error = DecodeError::None;
    QString detail;
    bool ok() const { return error == DecodeError::None; }
};

/// Decodes and validates an envelope. `authenticatedPeerId`, when non-empty,
/// must equal the declared sender; this is the check that stops a guest from
/// impersonating the host by editing a field.
DecodeResult decodeEnvelope(const QJsonObject& object, Envelope* out, const QString& authenticatedPeerId = {});

/// Same, from raw bytes, additionally enforcing kMaxControlMessageBytes.
/// Oversized frames are rejected with a reason and never truncated (03 §4).
DecodeResult decodeEnvelopeBytes(const QByteArray& bytes, Envelope* out, const QString& authenticatedPeerId = {});

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

struct HealthReportMessage {
    double rttMs = 0;
    double jitterMs = 0;
    double lossFraction = 0;
    quint32 queueFrames = 0;
    double audioClockErrorMs = 0;
    bool acceptable = false;
    QJsonObject toJson() const;
    static std::optional<HealthReportMessage> fromJson(const QJsonObject& payload, QString* reason = nullptr);
};

// ---------------------------------------------------------------------------
// Shared validation helpers, exposed so the tests can exercise them directly.
// ---------------------------------------------------------------------------

/// Opaque identifier: 8..64 chars of [A-Za-z0-9_-]. Rejects path separators,
/// dots and control characters so an id can never become a path component.
bool validOpaqueId(const QString& value);
/// Display text: trimmed, 1..40 chars, control characters removed. Returns the
/// sanitised value; callers store and forward that, not the raw input.
QString sanitizeDisplayName(const QString& value);
/// Lowercase hex digest of exactly `bytes` bytes.
bool validHexDigest(const QString& value, int bytes);

} // namespace junction
