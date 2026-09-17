#include "session_protocol.h"
#include <QJsonDocument>
#include <array>
#include <cmath>

namespace junction {
namespace {

struct TypeName { MessageType type; const char* name; };
constexpr std::array<TypeName, 9> kTypeNames{{
    {MessageType::PeerHello, "peer.hello"},
    {MessageType::SessionSnapshot, "session.snapshot"},
    {MessageType::ClockProbeRequest, "clock.probe"},
    {MessageType::ClockProbeReply, "clock.reply"},
    {MessageType::SessionRecovery, "session.recovery"},
    {MessageType::HealthReport, "health.report"},
    {MessageType::PeerLeave, "peer.leave"},
    {MessageType::SessionEnd, "session.end"},
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
bool takeId(const QJsonObject& object, const char* key, QString* out, QString* reason) {
    const QJsonValue value = object.value(QLatin1String(key));
    if (!value.isString() || !validOpaqueId(value.toString())) {
        if (reason) *reason = QStringLiteral("%1 must be an opaque id of 8..64 [A-Za-z0-9_-]").arg(QLatin1String(key));
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

MessageType messageTypeFromName(const QString& name) {
    for (const TypeName& entry : kTypeNames) if (name == QLatin1String(entry.name)) return entry.type;
    return MessageType::Unknown;
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

} // namespace junction
