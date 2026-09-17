#pragma once
// Junction identifiers, 64-bit encoding rules and cryptographic randomness.
//
// Wire rule (03 §3): 64-bit counters travel as decimal strings in JSON because
// JavaScript numbers lose precision past 2^53. Native code keeps them as
// quint64. Every encoder in this subsystem goes through these helpers so no
// call site can accidentally emit a double.
#include <QByteArray>
#include <QJsonValue>
#include <QString>
#include <optional>

namespace junction {

/// Wire sample rate for the shared media timeline. Frames, not milliseconds.
inline constexpr quint32 kWireSampleRate = 48000;
/// Junction's own protocol version, independent of the local engine's protocol=1.
inline constexpr int kJunctionProtocolVersion = 1;
/// Control messages above this are rejected with a reason, never truncated.
inline constexpr int kMaxControlMessageBytes = 64 * 1024;
/// Asset/validation chunk ceiling negotiated with peers.
inline constexpr int kMaxChunkBytes = 32 * 1024;

inline QString u64(quint64 value) {
    return QString::number(value);
}

/// Parses a decimal-string counter. Rejects signs, whitespace, empty strings,
/// leading zeros beyond "0" and anything that is not exactly a quint64, so a
/// peer cannot smuggle a float or an overflowed value past the schema.
inline std::optional<quint64> parseU64(const QJsonValue& value) {
    if (!value.isString()) return std::nullopt;
    const QString text = value.toString();
    if (text.isEmpty() || text.size() > 20) return std::nullopt;
    for (const QChar ch : text) if (ch < QLatin1Char('0') || ch > QLatin1Char('9')) return std::nullopt;
    if (text.size() > 1 && text.startsWith(QLatin1Char('0'))) return std::nullopt;
    bool ok = false;
    const quint64 parsed = text.toULongLong(&ok);
    return ok ? std::optional<quint64>(parsed) : std::nullopt;
}

/// Cryptographically secure random bytes from the OS. Never a PRNG seeded from
/// the clock: these back invite tokens and session identifiers.
QByteArray secureRandomBytes(int count);
/// Lowercase hex of `count` secure random bytes.
QString secureRandomHex(int count);
/// URL-safe base64 (no padding) of `count` secure random bytes. Invite tokens.
QString secureRandomToken(int count);

/// Constant-time comparison. Used for invite tokens and payload digests so a
/// remote peer cannot time its way to a valid token.
bool constantTimeEquals(const QByteArray& a, const QByteArray& b);

/// Lowercase hex SHA-256 of a buffer.
QString sha256Hex(const QByteArray& data);

} // namespace junction
