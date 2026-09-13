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
#include <QJsonArray>
#include <QJsonObject>
#include <QString>
#include <QStringList>
#include <optional>
#include <vector>

namespace junction {

/// Message kinds carried on the reliable ordered control channel.
enum class MessageType {
    Unknown,
    PeerHello,
    SessionSnapshot,
    ClockProbeRequest,
    ClockProbeReply,
    GraphManifest,
    GraphApplied,
    GraphCheckpoint,
    HandoffRequest,
    HandoffPrepare,
    HandoffFence,
    HandoffFenced,
    HandoffReady,
    HandoffCommit,
    HandoffActivate,
    HandoffCancel,
    SessionRecovery,
    HealthReport,
    AssetManifest,
    AssetRequest,
    AssetChunk,
    AssetComplete,
    ValidationWindow,
    ValidationChunk,
    PeerLeave,
    SessionEnd,
    MonitorRequest,
    TrackAnnounce,
};

QString messageTypeName(MessageType type);
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

QString decodeErrorName(DecodeError error);

struct DecodeResult {
    DecodeError error = DecodeError::None;
    QString detail;
    bool ok() const { return error == DecodeError::None; }
};

QJsonObject encodeEnvelope(const Envelope& envelope);

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

/// Identity of an engine build, for compatibility gating. A version string
/// alone is not enough: the same Mixxx commit with different plumdeck DSP or a
/// different checkpoint schema is not interchangeable (01, "固定依存の扱い").
struct EngineFingerprint {
    QString mixxxCommit;
    QString plumdeckBuild;
    int checkpointVersion = 0;
    int protocolVersion = kJunctionProtocolVersion;
    QString dspProfile;

    QJsonObject toJson() const;
    static std::optional<EngineFingerprint> fromJson(const QJsonValue& value);
    bool compatibleWith(const EngineFingerprint& other, QString* reason = nullptr) const;
};

struct PeerHello {
    int version = kJunctionProtocolVersion;
    QString displayName;
    EngineFingerprint fingerprint;
    QStringList capabilities;
    /// Nominal local device rate, so 44.1 vs 48 kHz mapping is explicit.
    quint32 localSampleRateHz = 0;

    QJsonObject toJson() const;
    static std::optional<PeerHello> fromJson(const QJsonObject& payload, QString* reason = nullptr);
};

enum class PeerRole { Host, Performer, NextUp, Guest };
QString peerRoleName(PeerRole role);

/// Public participant record. Deliberately has no device id, no file path and
/// no invite material: this struct is what reaches every peer and the UI.
struct ParticipantInfo {
    QString peerId;
    QString displayName;
    /// Short disambiguator shown next to duplicate display names (06).
    QString shortId;
    bool isHost = false;
    bool isPerformer = false;
    bool isNextUp = false;
    bool approved = false;
    bool monitoring = false;
    QString connection;
    QString readiness;

    QJsonObject toJson() const;
    static std::optional<ParticipantInfo> fromJson(const QJsonObject& object);
};

struct SessionSnapshotMessage {
    QString sessionId;
    QString sessionName;
    QString hostPeerId;
    QString performerPeerId;
    QString nextPeerId;
    quint64 epoch = 0;
    quint64 graphGeneration = 0;
    /// Host monotonic anchor T0 and its frame, so a late joiner adopts the
    /// same media timeline instead of inventing one.
    qint64 timelineOriginNanos = 0;
    quint64 timelineOriginFrame = 0;
    quint32 programDelayFrames = 0;
    QString handoffState;
    QString handoffId;
    std::vector<ParticipantInfo> participants;
    int maxPeers = 8;

    QJsonObject toJson() const;
    static std::optional<SessionSnapshotMessage> fromJson(const QJsonObject& payload, QString* reason = nullptr);
};

/// One normalised, *actually applied* performance command (04 §3.2).
///
/// The sequence number is assigned after native application and after any
/// permitted coalescing, never at UI submit time. `appliedMediaFrame` is where
/// the engine really applied it, which is what the replica has to reproduce.
struct AppliedStageCommand {
    QString sessionId;
    quint64 ownerEpoch = 0;
    QString actorPeerId;
    quint64 sequence = 0;
    quint64 appliedMediaFrame = 0;
    QString op;
    QJsonObject params;
    quint64 resultingRevision = 0;

    QJsonObject toJson() const;
    static std::optional<AppliedStageCommand> fromJson(const QJsonObject& object, QString* reason = nullptr);
};

struct GraphAppliedMessage {
    std::vector<AppliedStageCommand> commands;
    QJsonObject toJson() const;
    static std::optional<GraphAppliedMessage> fromJson(const QJsonObject& payload, QString* reason = nullptr);
};

/// Reference to one shared audio file. `assetId` is the SHA-256 of the whole
/// file. A numeric local track id or an absolute path from another machine is
/// never authoritative (03 §6) and is not part of this struct.
struct AssetRef {
    QString assetId;
    quint64 sizeBytes = 0;
    quint32 sourceSampleRateHz = 0;
    quint16 channels = 0;
    quint64 durationFrames = 0;
    QString container;
    /// Display-only, sanitised. Never used to build a path.
    QString displayName;
    /// Shared musical metadata (beatgrid/cues) is versioned separately from
    /// the audio bytes so a grid edit does not re-transfer the file.
    quint64 metadataVersion = 0;

    QJsonObject toJson() const;
    static std::optional<AssetRef> fromJson(const QJsonObject& object, QString* reason = nullptr);
    bool valid(QString* reason = nullptr) const;
};

struct HandoffRequestMessage {
    QString handoffId;
    QString targetPeerId;
    quint64 currentEpoch = 0;
    QJsonObject toJson() const;
    static std::optional<HandoffRequestMessage> fromJson(const QJsonObject& payload, QString* reason = nullptr);
};

struct HandoffFencedMessage {
    QString handoffId;
    quint64 fencedAtMediaFrame = 0;
    quint64 lastAppliedSeq = 0;
    quint64 revision = 0;
    QJsonObject toJson() const;
    static std::optional<HandoffFencedMessage> fromJson(const QJsonObject& payload, QString* reason = nullptr);
};

struct HandoffReadyMessage {
    QString handoffId;
    quint64 throughSeq = 0;
    bool ready = false;
    /// Human-readable wait reason when `ready` is false (06: 「曲の受信中」等).
    QString reason;
    QJsonObject metrics;
    QJsonObject toJson() const;
    static std::optional<HandoffReadyMessage> fromJson(const QJsonObject& payload, QString* reason = nullptr);
};

/// Only the host may issue this, and only after recording it durably.
struct HandoffCommitMessage {
    QString sessionId;
    QString handoffId;
    quint64 oldEpoch = 0;
    quint64 newEpoch = 0;
    QString oldOwner;
    QString newOwner;
    /// H: the media frame at which ownership actually moves.
    quint64 effectiveMediaFrame = 0;
    /// F0: where the old owner stopped accepting shared input.
    quint64 fencedAtMediaFrame = 0;
    quint64 lastAppliedSeq = 0;
    quint64 capsuleRevision = 0;

    QJsonObject toJson() const;
    static std::optional<HandoffCommitMessage> fromJson(const QJsonObject& payload, QString* reason = nullptr);
    bool valid(QString* reason = nullptr) const;
};

struct HandoffCancelMessage {
    QString handoffId;
    QString reason;
    QJsonObject toJson() const;
    static std::optional<HandoffCancelMessage> fromJson(const QJsonObject& payload, QString* reason = nullptr);
};

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

/// The raw-PCM comparison window of 03 §9. Deliberately bounded: 24,000 frames
/// of stereo float32 is 192,000 bytes, and it is only sent while preparing.
struct ValidationWindowManifest {
    QString windowId;
    QString handoffId;
    QString producerPeerId;
    quint64 epoch = 0;
    quint64 graphGeneration = 0;
    quint64 throughSeq = 0;
    quint64 startMediaFrame = 0;
    quint32 frameCount = 0;
    quint32 sampleRateHz = kWireSampleRate;
    quint16 channels = kWireChannels;
    QString format = QStringLiteral("f32le-interleaved");
    QString bus;
    EngineFingerprint engineFingerprint;
    qint32 srcDelayFrames = 0;
    QString payloadSha256;

    static constexpr quint32 kMaxFrameCount = 24000;

    QJsonObject toJson() const;
    static std::optional<ValidationWindowManifest> fromJson(const QJsonObject& payload, QString* reason = nullptr);
    /// Enforces rate, channel count, format, bus name and size. A mono or
    /// 44.1 kHz payload must never be silently reinterpreted (03 §9).
    bool valid(QString* reason = nullptr) const;
    quint64 payloadBytes() const {
        return static_cast<quint64>(frameCount) * channels * sizeof(float);
    }
};

/// Recovery after a post-commit failure. Never rolls back to an older epoch.
struct SessionRecoveryMessage {
    QString reason;
    quint64 newEpoch = 0;
    QString producerPeerId;
    quint64 effectiveMediaFrame = 0;
    QJsonObject toJson() const;
    static std::optional<SessionRecoveryMessage> fromJson(const QJsonObject& payload, QString* reason = nullptr);
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
