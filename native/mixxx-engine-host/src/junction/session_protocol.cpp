#include "session_protocol.h"
#include <QJsonDocument>
#include <QRegularExpression>
#include <array>
#include <cmath>

namespace junction {
namespace {

struct TypeName { MessageType type; const char* name; };
constexpr std::array<TypeName, 28> kTypeNames{{
    {MessageType::PeerHello, "peer.hello"},
    {MessageType::SessionSnapshot, "session.snapshot"},
    {MessageType::ClockProbeRequest, "clock.probe"},
    {MessageType::ClockProbeReply, "clock.reply"},
    {MessageType::GraphManifest, "graph.manifest"},
    {MessageType::GraphApplied, "graph.applied"},
    {MessageType::GraphCheckpoint, "graph.checkpoint"},
    {MessageType::HandoffRequest, "handoff.request"},
    {MessageType::HandoffPrepare, "handoff.prepare"},
    {MessageType::HandoffFence, "handoff.fence"},
    {MessageType::HandoffFenced, "handoff.fenced"},
    {MessageType::HandoffReady, "handoff.ready"},
    {MessageType::HandoffCommit, "handoff.commit"},
    {MessageType::HandoffActivate, "handoff.activate"},
    {MessageType::HandoffCancel, "handoff.cancel"},
    {MessageType::SessionRecovery, "session.recovery"},
    {MessageType::HealthReport, "health.report"},
    {MessageType::AssetManifest, "asset.manifest"},
    {MessageType::AssetRequest, "asset.request"},
    {MessageType::AssetChunk, "asset.chunk"},
    {MessageType::AssetComplete, "asset.complete"},
    {MessageType::ValidationWindow, "validation.window"},
    {MessageType::ValidationChunk, "validation.chunk"},
    {MessageType::PeerLeave, "peer.leave"},
    {MessageType::SessionEnd, "session.end"},
    {MessageType::MonitorRequest, "monitor.request"},
    {MessageType::TrackAnnounce, "tracks.announce"},
    {MessageType::Turn, "turn"},
}};

/// Reads a required decimal-string counter into `out`.
bool takeU64(const QJsonObject& object, const char* key, quint64* out, QString* reason) {
    const auto parsed = parseU64(object.value(QLatin1String(key)));
    if (!parsed) {
        if (reason) *reason = QStringLiteral("%1 must be a decimal string 64-bit counter").arg(QLatin1String(key));
        return false;
    }
    *out = *parsed;
    return true;
}

/// Optional counter; absent leaves `out` untouched, malformed still fails.
bool takeOptionalU64(const QJsonObject& object, const char* key, quint64* out, QString* reason) {
    const QJsonValue value = object.value(QLatin1String(key));
    if (value.isUndefined() || value.isNull()) return true;
    return takeU64(object, key, out, reason);
}

bool takeId(const QJsonObject& object, const char* key, QString* out, QString* reason) {
    const QJsonValue value = object.value(QLatin1String(key));
    if (!value.isString() || !validOpaqueId(value.toString())) {
        if (reason) *reason = QStringLiteral("%1 must be an opaque id of 8..64 [A-Za-z0-9_-]").arg(QLatin1String(key));
        return false;
    }
    *out = value.toString();
    return true;
}

bool takeOptionalId(const QJsonObject& object, const char* key, QString* out, QString* reason) {
    const QJsonValue value = object.value(QLatin1String(key));
    if (value.isUndefined() || value.isNull() || (value.isString() && value.toString().isEmpty())) return true;
    return takeId(object, key, out, reason);
}

bool takeBoundedString(const QJsonObject& object, const char* key, int maxChars, QString* out, QString* reason) {
    const QJsonValue value = object.value(QLatin1String(key));
    if (!value.isString() || value.toString().size() > maxChars) {
        if (reason) *reason = QStringLiteral("%1 must be a string of at most %2 characters").arg(QLatin1String(key)).arg(maxChars);
        return false;
    }
    *out = value.toString();
    return true;
}

bool takeFinite(const QJsonObject& object, const char* key, double low, double high, double* out, QString* reason) {
    const QJsonValue value = object.value(QLatin1String(key));
    if (!value.isDouble() || !std::isfinite(value.toDouble()) || value.toDouble() < low || value.toDouble() > high) {
        if (reason) *reason = QStringLiteral("%1 must be a finite number in [%2, %3]").arg(QLatin1String(key)).arg(low).arg(high);
        return false;
    }
    *out = value.toDouble();
    return true;
}

bool takeBoundedInt(const QJsonObject& object, const char* key, qint64 low, qint64 high, qint64* out, QString* reason) {
    const QJsonValue value = object.value(QLatin1String(key));
    if (!value.isDouble()) { if (reason) *reason = QStringLiteral("%1 must be a number").arg(QLatin1String(key)); return false; }
    const double raw = value.toDouble();
    if (!std::isfinite(raw) || std::floor(raw) != raw || raw < static_cast<double>(low) || raw > static_cast<double>(high)) {
        if (reason) *reason = QStringLiteral("%1 must be an integer in [%2, %3]").arg(QLatin1String(key)).arg(low).arg(high);
        return false;
    }
    *out = static_cast<qint64>(raw);
    return true;
}

} // namespace

QString messageTypeName(MessageType type) {
    for (const TypeName& entry : kTypeNames) if (entry.type == type) return QString::fromLatin1(entry.name);
    return QStringLiteral("unknown");
}

MessageType messageTypeFromName(const QString& name) {
    for (const TypeName& entry : kTypeNames) if (name == QLatin1String(entry.name)) return entry.type;
    return MessageType::Unknown;
}

QString decodeErrorName(DecodeError error) {
    switch (error) {
    case DecodeError::None: return QStringLiteral("ok");
    case DecodeError::NotAnObject: return QStringLiteral("not_an_object");
    case DecodeError::UnsupportedVersion: return QStringLiteral("unsupported_version");
    case DecodeError::MissingField: return QStringLiteral("missing_field");
    case DecodeError::BadField: return QStringLiteral("bad_field");
    case DecodeError::UnknownType: return QStringLiteral("unknown_type");
    case DecodeError::TooLarge: return QStringLiteral("too_large");
    case DecodeError::IdentityMismatch: return QStringLiteral("identity_mismatch");
    }
    return QStringLiteral("unknown");
}

bool validOpaqueId(const QString& value) {
    if (value.size() < 8 || value.size() > 64) return false;
    for (const QChar ch : value) {
        const char16_t code = ch.unicode();
        const bool allowed = (code >= u'a' && code <= u'z') || (code >= u'A' && code <= u'Z') ||
                (code >= u'0' && code <= u'9') || code == u'-' || code == u'_';
        if (!allowed) return false;
    }
    return true;
}

QString sanitizeDisplayName(const QString& value) {
    QString out;
    out.reserve(value.size());
    for (const QChar ch : value) {
        // Drop control characters and line separators outright: a display name
        // is echoed to every participant and must not be able to forge a line
        // in a log or a roster.
        if (ch.isPrint() && ch != QChar::LineSeparator && ch != QChar::ParagraphSeparator) out.append(ch);
    }
    out = out.trimmed();
    if (out.size() > 40) out.truncate(40);
    return out;
}

bool validHexDigest(const QString& value, int bytes) {
    if (value.size() != bytes * 2) return false;
    for (const QChar ch : value) {
        const char16_t code = ch.unicode();
        if (!((code >= u'0' && code <= u'9') || (code >= u'a' && code <= u'f'))) return false;
    }
    return true;
}

QJsonObject encodeEnvelope(const Envelope& envelope) {
    return QJsonObject{
        {QStringLiteral("version"), envelope.version},
        {QStringLiteral("sessionId"), envelope.sessionId},
        {QStringLiteral("senderPeerId"), envelope.senderPeerId},
        {QStringLiteral("epoch"), u64(envelope.epoch)},
        {QStringLiteral("messageId"), envelope.messageId},
        {QStringLiteral("type"), messageTypeName(envelope.type)},
        {QStringLiteral("payload"), envelope.payload},
    };
}

DecodeResult decodeEnvelope(const QJsonObject& object, Envelope* out, const QString& authenticatedPeerId) {
    DecodeResult result;
    const QJsonValue version = object.value(QStringLiteral("version"));
    if (!version.isDouble() || version.toInt() != kJunctionProtocolVersion) {
        result.error = DecodeError::UnsupportedVersion;
        result.detail = QStringLiteral("expected junction protocol version %1").arg(kJunctionProtocolVersion);
        return result;
    }
    Envelope envelope;
    envelope.version = version.toInt();
    QString reason;
    if (!takeId(object, "sessionId", &envelope.sessionId, &reason) ||
            !takeId(object, "senderPeerId", &envelope.senderPeerId, &reason) ||
            !takeId(object, "messageId", &envelope.messageId, &reason) ||
            !takeU64(object, "epoch", &envelope.epoch, &reason)) {
        result.error = DecodeError::BadField;
        result.detail = reason;
        return result;
    }
    const QJsonValue type = object.value(QStringLiteral("type"));
    if (!type.isString()) {
        result.error = DecodeError::MissingField;
        result.detail = QStringLiteral("type is required");
        return result;
    }
    envelope.type = messageTypeFromName(type.toString());
    if (envelope.type == MessageType::Unknown) {
        result.error = DecodeError::UnknownType;
        result.detail = QStringLiteral("unknown message type");
        return result;
    }
    const QJsonValue payload = object.value(QStringLiteral("payload"));
    if (!payload.isObject()) {
        result.error = DecodeError::MissingField;
        result.detail = QStringLiteral("payload must be an object");
        return result;
    }
    envelope.payload = payload.toObject();
    // The connection's authenticated identity wins over anything the payload
    // claims. Without this a guest could set senderPeerId to the host's id.
    if (!authenticatedPeerId.isEmpty() && authenticatedPeerId != envelope.senderPeerId) {
        result.error = DecodeError::IdentityMismatch;
        result.detail = QStringLiteral("senderPeerId does not match the authenticated peer");
        return result;
    }
    *out = envelope;
    return result;
}

DecodeResult decodeEnvelopeBytes(const QByteArray& bytes, Envelope* out, const QString& authenticatedPeerId) {
    DecodeResult result;
    if (bytes.size() > kMaxControlMessageBytes) {
        result.error = DecodeError::TooLarge;
        result.detail = QStringLiteral("control message exceeds %1 bytes").arg(kMaxControlMessageBytes);
        return result;
    }
    QJsonParseError parse{};
    const QJsonDocument document = QJsonDocument::fromJson(bytes, &parse);
    if (parse.error != QJsonParseError::NoError || !document.isObject()) {
        result.error = DecodeError::NotAnObject;
        result.detail = QStringLiteral("expected one JSON object");
        return result;
    }
    return decodeEnvelope(document.object(), out, authenticatedPeerId);
}

// --- EngineFingerprint -----------------------------------------------------

QJsonObject EngineFingerprint::toJson() const {
    return QJsonObject{
        {QStringLiteral("mixxxCommit"), mixxxCommit},
        {QStringLiteral("plumdeckBuild"), plumdeckBuild},
        {QStringLiteral("checkpointVersion"), checkpointVersion},
        {QStringLiteral("protocolVersion"), protocolVersion},
        {QStringLiteral("dspProfile"), dspProfile},
    };
}

std::optional<EngineFingerprint> EngineFingerprint::fromJson(const QJsonValue& value) {
    if (!value.isObject()) return std::nullopt;
    const QJsonObject object = value.toObject();
    EngineFingerprint fingerprint;
    QString reason;
    if (!takeBoundedString(object, "mixxxCommit", 64, &fingerprint.mixxxCommit, &reason)) return std::nullopt;
    if (!takeBoundedString(object, "plumdeckBuild", 64, &fingerprint.plumdeckBuild, &reason)) return std::nullopt;
    if (!takeBoundedString(object, "dspProfile", 64, &fingerprint.dspProfile, &reason)) return std::nullopt;
    qint64 checkpoint = 0, protocol = 0;
    if (!takeBoundedInt(object, "checkpointVersion", 0, 1000000, &checkpoint, &reason)) return std::nullopt;
    if (!takeBoundedInt(object, "protocolVersion", 0, 1000000, &protocol, &reason)) return std::nullopt;
    fingerprint.checkpointVersion = static_cast<int>(checkpoint);
    fingerprint.protocolVersion = static_cast<int>(protocol);
    return fingerprint;
}

bool EngineFingerprint::compatibleWith(const EngineFingerprint& other, QString* reason) const {
    if (protocolVersion != other.protocolVersion) {
        if (reason) *reason = QStringLiteral("Junction プロトコルの版が違います（%1 と %2）").arg(protocolVersion).arg(other.protocolVersion);
        return false;
    }
    if (checkpointVersion != other.checkpointVersion) {
        if (reason) *reason = QStringLiteral("引き継ぎ状態の形式が違います（%1 と %2）").arg(checkpointVersion).arg(other.checkpointVersion);
        return false;
    }
    // A different pinned engine means different DSP behaviour even at the same
    // Mixxx version string, so this is a hard stop rather than a warning.
    if (mixxxCommit != other.mixxxCommit) {
        if (reason) *reason = QStringLiteral("エンジンのビルドが違います。同じ版の plumdeck を使ってください");
        return false;
    }
    if (dspProfile != other.dspProfile) {
        if (reason) *reason = QStringLiteral("音声処理の構成が違います。同じ版の plumdeck を使ってください");
        return false;
    }
    return true;
}

// --- PeerHello -------------------------------------------------------------

QJsonObject PeerHello::toJson() const {
    QJsonArray caps;
    for (const QString& capability : capabilities) caps.append(capability);
    return QJsonObject{
        {QStringLiteral("version"), version},
        {QStringLiteral("displayName"), displayName},
        {QStringLiteral("engineFingerprint"), fingerprint.toJson()},
        {QStringLiteral("capabilities"), caps},
        {QStringLiteral("localSampleRateHz"), static_cast<int>(localSampleRateHz)},
    };
}

std::optional<PeerHello> PeerHello::fromJson(const QJsonObject& payload, QString* reason) {
    PeerHello hello;
    qint64 version = 0;
    if (!takeBoundedInt(payload, "version", 1, 1000000, &version, reason)) return std::nullopt;
    hello.version = static_cast<int>(version);
    QString rawName;
    if (!takeBoundedString(payload, "displayName", 200, &rawName, reason)) return std::nullopt;
    hello.displayName = sanitizeDisplayName(rawName);
    if (hello.displayName.isEmpty()) {
        if (reason) *reason = QStringLiteral("displayName is empty after sanitisation");
        return std::nullopt;
    }
    const auto fingerprint = EngineFingerprint::fromJson(payload.value(QStringLiteral("engineFingerprint")));
    if (!fingerprint) {
        if (reason) *reason = QStringLiteral("engineFingerprint is missing or malformed");
        return std::nullopt;
    }
    hello.fingerprint = *fingerprint;
    const QJsonValue caps = payload.value(QStringLiteral("capabilities"));
    if (!caps.isArray()) {
        if (reason) *reason = QStringLiteral("capabilities must be an array");
        return std::nullopt;
    }
    const QJsonArray array = caps.toArray();
    if (array.size() > 64) {
        if (reason) *reason = QStringLiteral("capabilities list is too long");
        return std::nullopt;
    }
    for (const QJsonValue& capability : array) {
        if (!capability.isString() || capability.toString().size() > 64) {
            if (reason) *reason = QStringLiteral("capability entries must be short strings");
            return std::nullopt;
        }
        hello.capabilities.append(capability.toString());
    }
    qint64 rate = 0;
    if (!takeBoundedInt(payload, "localSampleRateHz", 8000, 384000, &rate, reason)) return std::nullopt;
    hello.localSampleRateHz = static_cast<quint32>(rate);
    return hello;
}

QString peerRoleName(PeerRole role) {
    switch (role) {
    case PeerRole::Host: return QStringLiteral("host");
    case PeerRole::Performer: return QStringLiteral("performer");
    case PeerRole::NextUp: return QStringLiteral("next");
    case PeerRole::Guest: return QStringLiteral("guest");
    }
    return QStringLiteral("guest");
}

// --- ParticipantInfo -------------------------------------------------------

QJsonObject ParticipantInfo::toJson() const {
    return QJsonObject{
        {QStringLiteral("peerId"), peerId},
        {QStringLiteral("displayName"), displayName},
        {QStringLiteral("shortId"), shortId},
        {QStringLiteral("isHost"), isHost},
        {QStringLiteral("isPerformer"), isPerformer},
        {QStringLiteral("isNextUp"), isNextUp},
        {QStringLiteral("approved"), approved},
        {QStringLiteral("monitoring"), monitoring},
        {QStringLiteral("connection"), connection},
        {QStringLiteral("readiness"), readiness},
    };
}

std::optional<ParticipantInfo> ParticipantInfo::fromJson(const QJsonObject& object) {
    ParticipantInfo info;
    QString reason;
    if (!takeId(object, "peerId", &info.peerId, &reason)) return std::nullopt;
    QString rawName;
    if (!takeBoundedString(object, "displayName", 200, &rawName, &reason)) return std::nullopt;
    info.displayName = sanitizeDisplayName(rawName);
    takeBoundedString(object, "shortId", 16, &info.shortId, &reason);
    info.isHost = object.value(QStringLiteral("isHost")).toBool();
    info.isPerformer = object.value(QStringLiteral("isPerformer")).toBool();
    info.isNextUp = object.value(QStringLiteral("isNextUp")).toBool();
    info.approved = object.value(QStringLiteral("approved")).toBool();
    info.monitoring = object.value(QStringLiteral("monitoring")).toBool();
    takeBoundedString(object, "connection", 32, &info.connection, &reason);
    takeBoundedString(object, "readiness", 64, &info.readiness, &reason);
    return info;
}

// --- SessionSnapshotMessage ------------------------------------------------

QJsonObject SessionSnapshotMessage::toJson() const {
    QJsonArray peers;
    for (const ParticipantInfo& participant : participants) peers.append(participant.toJson());
    return QJsonObject{
        {QStringLiteral("sessionId"), sessionId},
        {QStringLiteral("sessionName"), sessionName},
        {QStringLiteral("hostPeerId"), hostPeerId},
        {QStringLiteral("performerPeerId"), performerPeerId},
        {QStringLiteral("nextPeerId"), nextPeerId},
        {QStringLiteral("epoch"), u64(epoch)},
        {QStringLiteral("graphGeneration"), u64(graphGeneration)},
        {QStringLiteral("timelineOriginNanos"), QString::number(timelineOriginNanos)},
        {QStringLiteral("timelineOriginFrame"), u64(timelineOriginFrame)},
        {QStringLiteral("programDelayFrames"), static_cast<int>(programDelayFrames)},
        {QStringLiteral("handoffState"), handoffState},
        {QStringLiteral("handoffId"), handoffId},
        {QStringLiteral("participants"), peers},
        {QStringLiteral("maxPeers"), maxPeers},
    };
}

std::optional<SessionSnapshotMessage> SessionSnapshotMessage::fromJson(const QJsonObject& payload, QString* reason) {
    SessionSnapshotMessage snapshot;
    if (!takeId(payload, "sessionId", &snapshot.sessionId, reason)) return std::nullopt;
    if (!takeId(payload, "hostPeerId", &snapshot.hostPeerId, reason)) return std::nullopt;
    if (!takeOptionalId(payload, "performerPeerId", &snapshot.performerPeerId, reason)) return std::nullopt;
    if (!takeOptionalId(payload, "nextPeerId", &snapshot.nextPeerId, reason)) return std::nullopt;
    if (!takeU64(payload, "epoch", &snapshot.epoch, reason)) return std::nullopt;
    if (!takeU64(payload, "graphGeneration", &snapshot.graphGeneration, reason)) return std::nullopt;
    if (!takeOptionalU64(payload, "timelineOriginFrame", &snapshot.timelineOriginFrame, reason)) return std::nullopt;
    const QJsonValue origin = payload.value(QStringLiteral("timelineOriginNanos"));
    if (origin.isString()) {
        bool ok = false;
        snapshot.timelineOriginNanos = origin.toString().toLongLong(&ok);
        if (!ok) { if (reason) *reason = QStringLiteral("timelineOriginNanos must be a decimal string"); return std::nullopt; }
    }
    QString rawName;
    takeBoundedString(payload, "sessionName", 200, &rawName, reason);
    snapshot.sessionName = sanitizeDisplayName(rawName);
    takeBoundedString(payload, "handoffState", 32, &snapshot.handoffState, reason);
    takeOptionalId(payload, "handoffId", &snapshot.handoffId, reason);
    qint64 delay = 0;
    if (takeBoundedInt(payload, "programDelayFrames", 0, 48000 * 10, &delay, reason))
        snapshot.programDelayFrames = static_cast<quint32>(delay);
    qint64 maxPeers = 8;
    if (takeBoundedInt(payload, "maxPeers", 1, 64, &maxPeers, reason)) snapshot.maxPeers = static_cast<int>(maxPeers);
    const QJsonArray peers = payload.value(QStringLiteral("participants")).toArray();
    if (peers.size() > 64) { if (reason) *reason = QStringLiteral("too many participants"); return std::nullopt; }
    for (const QJsonValue& peer : peers) {
        if (!peer.isObject()) continue;
        if (const auto participant = ParticipantInfo::fromJson(peer.toObject())) snapshot.participants.push_back(*participant);
    }
    return snapshot;
}

// --- AppliedStageCommand ---------------------------------------------------

QJsonObject AppliedStageCommand::toJson() const {
    return QJsonObject{
        {QStringLiteral("sessionId"), sessionId},
        {QStringLiteral("ownerEpoch"), u64(ownerEpoch)},
        {QStringLiteral("actorPeerId"), actorPeerId},
        {QStringLiteral("sequence"), u64(sequence)},
        {QStringLiteral("appliedMediaFrame"), u64(appliedMediaFrame)},
        {QStringLiteral("op"), op},
        {QStringLiteral("params"), params},
        {QStringLiteral("resultingRevision"), u64(resultingRevision)},
    };
}

std::optional<AppliedStageCommand> AppliedStageCommand::fromJson(const QJsonObject& object, QString* reason) {
    AppliedStageCommand command;
    if (!takeId(object, "sessionId", &command.sessionId, reason)) return std::nullopt;
    if (!takeId(object, "actorPeerId", &command.actorPeerId, reason)) return std::nullopt;
    if (!takeU64(object, "ownerEpoch", &command.ownerEpoch, reason)) return std::nullopt;
    if (!takeU64(object, "sequence", &command.sequence, reason)) return std::nullopt;
    if (!takeU64(object, "appliedMediaFrame", &command.appliedMediaFrame, reason)) return std::nullopt;
    if (!takeU64(object, "resultingRevision", &command.resultingRevision, reason)) return std::nullopt;
    if (!takeBoundedString(object, "op", 64, &command.op, reason) || command.op.isEmpty()) {
        if (reason) *reason = QStringLiteral("op must be a non-empty short string");
        return std::nullopt;
    }
    const QJsonValue params = object.value(QStringLiteral("params"));
    if (!params.isObject()) { if (reason) *reason = QStringLiteral("params must be an object"); return std::nullopt; }
    command.params = params.toObject();
    return command;
}

QJsonObject GraphAppliedMessage::toJson() const {
    QJsonArray array;
    for (const AppliedStageCommand& command : commands) array.append(command.toJson());
    return QJsonObject{{QStringLiteral("commands"), array}};
}

std::optional<GraphAppliedMessage> GraphAppliedMessage::fromJson(const QJsonObject& payload, QString* reason) {
    const QJsonValue value = payload.value(QStringLiteral("commands"));
    if (!value.isArray()) { if (reason) *reason = QStringLiteral("commands must be an array"); return std::nullopt; }
    const QJsonArray array = value.toArray();
    // A single control frame stays under the 64 KiB cap; larger batches go
    // through the chunked checkpoint path instead of being truncated here.
    if (array.size() > 512) { if (reason) *reason = QStringLiteral("too many commands in one message"); return std::nullopt; }
    GraphAppliedMessage message;
    for (const QJsonValue& entry : array) {
        if (!entry.isObject()) { if (reason) *reason = QStringLiteral("command entries must be objects"); return std::nullopt; }
        const auto command = AppliedStageCommand::fromJson(entry.toObject(), reason);
        if (!command) return std::nullopt;
        message.commands.push_back(*command);
    }
    return message;
}

// --- AssetRef --------------------------------------------------------------

QJsonObject AssetRef::toJson() const {
    return QJsonObject{
        {QStringLiteral("assetId"), assetId},
        {QStringLiteral("sizeBytes"), u64(sizeBytes)},
        {QStringLiteral("sourceSampleRateHz"), static_cast<int>(sourceSampleRateHz)},
        {QStringLiteral("channels"), static_cast<int>(channels)},
        {QStringLiteral("durationFrames"), u64(durationFrames)},
        {QStringLiteral("container"), container},
        {QStringLiteral("displayName"), displayName},
        {QStringLiteral("metadataVersion"), u64(metadataVersion)},
    };
}

std::optional<AssetRef> AssetRef::fromJson(const QJsonObject& object, QString* reason) {
    AssetRef asset;
    const QJsonValue id = object.value(QStringLiteral("assetId"));
    if (!id.isString() || !validHexDigest(id.toString(), 32)) {
        if (reason) *reason = QStringLiteral("assetId must be a lowercase hex SHA-256");
        return std::nullopt;
    }
    asset.assetId = id.toString();
    if (!takeU64(object, "sizeBytes", &asset.sizeBytes, reason)) return std::nullopt;
    if (!takeOptionalU64(object, "durationFrames", &asset.durationFrames, reason)) return std::nullopt;
    if (!takeOptionalU64(object, "metadataVersion", &asset.metadataVersion, reason)) return std::nullopt;
    qint64 rate = 0, channels = 0;
    if (!takeBoundedInt(object, "sourceSampleRateHz", 8000, 384000, &rate, reason)) return std::nullopt;
    if (!takeBoundedInt(object, "channels", 1, 8, &channels, reason)) return std::nullopt;
    asset.sourceSampleRateHz = static_cast<quint32>(rate);
    asset.channels = static_cast<quint16>(channels);
    if (!takeBoundedString(object, "container", 16, &asset.container, reason)) return std::nullopt;
    QString rawName;
    takeBoundedString(object, "displayName", 200, &rawName, reason);
    asset.displayName = sanitizeDisplayName(rawName);
    if (!asset.valid(reason)) return std::nullopt;
    return asset;
}

bool AssetRef::valid(QString* reason) const {
    if (!validHexDigest(assetId, 32)) { if (reason) *reason = QStringLiteral("assetId must be a SHA-256 hex digest"); return false; }
    if (sizeBytes == 0) { if (reason) *reason = QStringLiteral("sizeBytes must be positive"); return false; }
    // 2 GiB ceiling: beyond that the chunk offsets and the cache budget stop
    // being meaningful and it is not a DJ track.
    if (sizeBytes > (2ULL << 30)) { if (reason) *reason = QStringLiteral("asset exceeds the 2 GiB transfer ceiling"); return false; }
    if (sourceSampleRateHz < 8000) { if (reason) *reason = QStringLiteral("sourceSampleRateHz is implausible"); return false; }
    if (channels < 1 || channels > 8) { if (reason) *reason = QStringLiteral("channels out of range"); return false; }
    return true;
}

// --- handoff payloads ------------------------------------------------------

QJsonObject HandoffRequestMessage::toJson() const {
    return QJsonObject{
        {QStringLiteral("handoffId"), handoffId},
        {QStringLiteral("targetPeerId"), targetPeerId},
        {QStringLiteral("currentEpoch"), u64(currentEpoch)},
    };
}

std::optional<HandoffRequestMessage> HandoffRequestMessage::fromJson(const QJsonObject& payload, QString* reason) {
    HandoffRequestMessage message;
    if (!takeId(payload, "handoffId", &message.handoffId, reason)) return std::nullopt;
    if (!takeId(payload, "targetPeerId", &message.targetPeerId, reason)) return std::nullopt;
    if (!takeU64(payload, "currentEpoch", &message.currentEpoch, reason)) return std::nullopt;
    return message;
}

QJsonObject HandoffFencedMessage::toJson() const {
    return QJsonObject{
        {QStringLiteral("handoffId"), handoffId},
        {QStringLiteral("fencedAtMediaFrame"), u64(fencedAtMediaFrame)},
        {QStringLiteral("lastAppliedSeq"), u64(lastAppliedSeq)},
        {QStringLiteral("revision"), u64(revision)},
    };
}

std::optional<HandoffFencedMessage> HandoffFencedMessage::fromJson(const QJsonObject& payload, QString* reason) {
    HandoffFencedMessage message;
    if (!takeId(payload, "handoffId", &message.handoffId, reason)) return std::nullopt;
    if (!takeU64(payload, "fencedAtMediaFrame", &message.fencedAtMediaFrame, reason)) return std::nullopt;
    if (!takeU64(payload, "lastAppliedSeq", &message.lastAppliedSeq, reason)) return std::nullopt;
    if (!takeU64(payload, "revision", &message.revision, reason)) return std::nullopt;
    return message;
}

QJsonObject HandoffReadyMessage::toJson() const {
    return QJsonObject{
        {QStringLiteral("handoffId"), handoffId},
        {QStringLiteral("throughSeq"), u64(throughSeq)},
        {QStringLiteral("ready"), ready},
        {QStringLiteral("reason"), reason},
        {QStringLiteral("metrics"), metrics},
    };
}

std::optional<HandoffReadyMessage> HandoffReadyMessage::fromJson(const QJsonObject& payload, QString* failure) {
    HandoffReadyMessage message;
    if (!takeId(payload, "handoffId", &message.handoffId, failure)) return std::nullopt;
    if (!takeU64(payload, "throughSeq", &message.throughSeq, failure)) return std::nullopt;
    message.ready = payload.value(QStringLiteral("ready")).toBool();
    takeBoundedString(payload, "reason", 200, &message.reason, failure);
    message.metrics = payload.value(QStringLiteral("metrics")).toObject();
    return message;
}

QJsonObject HandoffCommitMessage::toJson() const {
    return QJsonObject{
        {QStringLiteral("sessionId"), sessionId},
        {QStringLiteral("handoffId"), handoffId},
        {QStringLiteral("oldEpoch"), u64(oldEpoch)},
        {QStringLiteral("newEpoch"), u64(newEpoch)},
        {QStringLiteral("oldOwner"), oldOwner},
        {QStringLiteral("newOwner"), newOwner},
        {QStringLiteral("effectiveMediaFrame"), u64(effectiveMediaFrame)},
        {QStringLiteral("fencedAtMediaFrame"), u64(fencedAtMediaFrame)},
        {QStringLiteral("lastAppliedSeq"), u64(lastAppliedSeq)},
        {QStringLiteral("capsuleRevision"), u64(capsuleRevision)},
    };
}

std::optional<HandoffCommitMessage> HandoffCommitMessage::fromJson(const QJsonObject& payload, QString* reason) {
    HandoffCommitMessage commit;
    if (!takeId(payload, "sessionId", &commit.sessionId, reason)) return std::nullopt;
    if (!takeId(payload, "handoffId", &commit.handoffId, reason)) return std::nullopt;
    if (!takeId(payload, "oldOwner", &commit.oldOwner, reason)) return std::nullopt;
    if (!takeId(payload, "newOwner", &commit.newOwner, reason)) return std::nullopt;
    if (!takeU64(payload, "oldEpoch", &commit.oldEpoch, reason)) return std::nullopt;
    if (!takeU64(payload, "newEpoch", &commit.newEpoch, reason)) return std::nullopt;
    if (!takeU64(payload, "effectiveMediaFrame", &commit.effectiveMediaFrame, reason)) return std::nullopt;
    if (!takeU64(payload, "fencedAtMediaFrame", &commit.fencedAtMediaFrame, reason)) return std::nullopt;
    if (!takeU64(payload, "lastAppliedSeq", &commit.lastAppliedSeq, reason)) return std::nullopt;
    if (!takeU64(payload, "capsuleRevision", &commit.capsuleRevision, reason)) return std::nullopt;
    if (!commit.valid(reason)) return std::nullopt;
    return commit;
}

bool HandoffCommitMessage::valid(QString* reason) const {
    // Epoch must move forward: 04 §5.4 forbids ever returning to an older one.
    if (newEpoch <= oldEpoch) {
        if (reason) *reason = QStringLiteral("newEpoch must be greater than oldEpoch");
        return false;
    }
    // H must be at or after F0, otherwise ownership would move to a frame the
    // old owner was still allowed to control.
    if (effectiveMediaFrame < fencedAtMediaFrame) {
        if (reason) *reason = QStringLiteral("effectiveMediaFrame must not precede fencedAtMediaFrame");
        return false;
    }
    if (oldOwner == newOwner) {
        if (reason) *reason = QStringLiteral("a commit must change the owner");
        return false;
    }
    return true;
}

QJsonObject HandoffCancelMessage::toJson() const {
    return QJsonObject{{QStringLiteral("handoffId"), handoffId}, {QStringLiteral("reason"), reason}};
}

std::optional<HandoffCancelMessage> HandoffCancelMessage::fromJson(const QJsonObject& payload, QString* failure) {
    HandoffCancelMessage message;
    if (!takeId(payload, "handoffId", &message.handoffId, failure)) return std::nullopt;
    takeBoundedString(payload, "reason", 200, &message.reason, failure);
    return message;
}

QJsonObject HealthReportMessage::toJson() const {
    return QJsonObject{
        {QStringLiteral("rttMs"), rttMs},
        {QStringLiteral("jitterMs"), jitterMs},
        {QStringLiteral("lossFraction"), lossFraction},
        {QStringLiteral("queueFrames"), static_cast<int>(queueFrames)},
        {QStringLiteral("audioClockErrorMs"), audioClockErrorMs},
        {QStringLiteral("acceptable"), acceptable},
    };
}

std::optional<HealthReportMessage> HealthReportMessage::fromJson(const QJsonObject& payload, QString* reason) {
    HealthReportMessage report;
    if (!takeFinite(payload, "rttMs", 0, 600000, &report.rttMs, reason)) return std::nullopt;
    if (!takeFinite(payload, "jitterMs", 0, 600000, &report.jitterMs, reason)) return std::nullopt;
    if (!takeFinite(payload, "lossFraction", 0, 1, &report.lossFraction, reason)) return std::nullopt;
    if (!takeFinite(payload, "audioClockErrorMs", -600000, 600000, &report.audioClockErrorMs, reason)) return std::nullopt;
    qint64 queue = 0;
    if (!takeBoundedInt(payload, "queueFrames", 0, 48000 * 60, &queue, reason)) return std::nullopt;
    report.queueFrames = static_cast<quint32>(queue);
    report.acceptable = payload.value(QStringLiteral("acceptable")).toBool();
    return report;
}

// --- ValidationWindowManifest ---------------------------------------------

QJsonObject ValidationWindowManifest::toJson() const {
    return QJsonObject{
        {QStringLiteral("windowId"), windowId},
        {QStringLiteral("handoffId"), handoffId},
        {QStringLiteral("producerPeerId"), producerPeerId},
        {QStringLiteral("epoch"), u64(epoch)},
        {QStringLiteral("graphGeneration"), u64(graphGeneration)},
        {QStringLiteral("throughSeq"), u64(throughSeq)},
        {QStringLiteral("startMediaFrame"), u64(startMediaFrame)},
        {QStringLiteral("frameCount"), static_cast<int>(frameCount)},
        {QStringLiteral("sampleRateHz"), static_cast<int>(sampleRateHz)},
        {QStringLiteral("channels"), static_cast<int>(channels)},
        {QStringLiteral("format"), format},
        {QStringLiteral("bus"), bus},
        {QStringLiteral("engineFingerprint"), engineFingerprint.toJson()},
        {QStringLiteral("srcDelayFrames"), srcDelayFrames},
        {QStringLiteral("payloadSha256"), payloadSha256},
    };
}

std::optional<ValidationWindowManifest> ValidationWindowManifest::fromJson(const QJsonObject& payload, QString* reason) {
    ValidationWindowManifest manifest;
    if (!takeId(payload, "windowId", &manifest.windowId, reason)) return std::nullopt;
    if (!takeId(payload, "handoffId", &manifest.handoffId, reason)) return std::nullopt;
    if (!takeId(payload, "producerPeerId", &manifest.producerPeerId, reason)) return std::nullopt;
    if (!takeU64(payload, "epoch", &manifest.epoch, reason)) return std::nullopt;
    if (!takeU64(payload, "graphGeneration", &manifest.graphGeneration, reason)) return std::nullopt;
    if (!takeU64(payload, "throughSeq", &manifest.throughSeq, reason)) return std::nullopt;
    if (!takeU64(payload, "startMediaFrame", &manifest.startMediaFrame, reason)) return std::nullopt;
    qint64 frames = 0, rate = 0, channels = 0, delay = 0;
    if (!takeBoundedInt(payload, "frameCount", 1, kMaxFrameCount, &frames, reason)) return std::nullopt;
    if (!takeBoundedInt(payload, "sampleRateHz", 1, 384000, &rate, reason)) return std::nullopt;
    if (!takeBoundedInt(payload, "channels", 1, 8, &channels, reason)) return std::nullopt;
    if (!takeBoundedInt(payload, "srcDelayFrames", -48000, 48000, &delay, reason)) return std::nullopt;
    manifest.frameCount = static_cast<quint32>(frames);
    manifest.sampleRateHz = static_cast<quint32>(rate);
    manifest.channels = static_cast<quint16>(channels);
    manifest.srcDelayFrames = static_cast<qint32>(delay);
    if (!takeBoundedString(payload, "format", 32, &manifest.format, reason)) return std::nullopt;
    if (!takeBoundedString(payload, "bus", 32, &manifest.bus, reason)) return std::nullopt;
    const QJsonValue digest = payload.value(QStringLiteral("payloadSha256"));
    if (!digest.isString() || !validHexDigest(digest.toString(), 32)) {
        if (reason) *reason = QStringLiteral("payloadSha256 must be a lowercase hex SHA-256");
        return std::nullopt;
    }
    manifest.payloadSha256 = digest.toString();
    const auto fingerprint = EngineFingerprint::fromJson(payload.value(QStringLiteral("engineFingerprint")));
    if (!fingerprint) { if (reason) *reason = QStringLiteral("engineFingerprint is missing or malformed"); return std::nullopt; }
    manifest.engineFingerprint = *fingerprint;
    if (!manifest.valid(reason)) return std::nullopt;
    return manifest;
}

bool ValidationWindowManifest::valid(QString* reason) const {
    // Reinterpreting a mono or 44.1 kHz payload under this manifest would make
    // the comparison meaningless, so the fixed fields are checked, not coerced.
    if (sampleRateHz != kWireSampleRate) {
        if (reason) *reason = QStringLiteral("validation windows are 48 kHz only");
        return false;
    }
    if (channels != kWireChannels) {
        if (reason) *reason = QStringLiteral("validation windows are stereo only");
        return false;
    }
    if (format != QLatin1String("f32le-interleaved")) {
        if (reason) *reason = QStringLiteral("validation windows are f32le-interleaved only");
        return false;
    }
    if (bus != QLatin1String("post-master") && bus != QLatin1String("dry-reference") && bus != QLatin1String("wet-reference")) {
        if (reason) *reason = QStringLiteral("unknown validation bus");
        return false;
    }
    if (frameCount == 0 || frameCount > kMaxFrameCount) {
        if (reason) *reason = QStringLiteral("frameCount must be 1..%1").arg(kMaxFrameCount);
        return false;
    }
    return true;
}

QJsonObject SessionRecoveryMessage::toJson() const {
    return QJsonObject{
        {QStringLiteral("reason"), reason},
        {QStringLiteral("newEpoch"), u64(newEpoch)},
        {QStringLiteral("producerPeerId"), producerPeerId},
        {QStringLiteral("effectiveMediaFrame"), u64(effectiveMediaFrame)},
    };
}

std::optional<SessionRecoveryMessage> SessionRecoveryMessage::fromJson(const QJsonObject& payload, QString* failure) {
    SessionRecoveryMessage message;
    takeBoundedString(payload, "reason", 200, &message.reason, failure);
    if (!takeU64(payload, "newEpoch", &message.newEpoch, failure)) return std::nullopt;
    if (!takeId(payload, "producerPeerId", &message.producerPeerId, failure)) return std::nullopt;
    if (!takeU64(payload, "effectiveMediaFrame", &message.effectiveMediaFrame, failure)) return std::nullopt;
    return message;
}

} // namespace junction
