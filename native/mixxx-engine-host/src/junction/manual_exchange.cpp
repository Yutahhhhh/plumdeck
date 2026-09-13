#include "manual_exchange.h"
#include "network_settings.h"
#include <QDateTime>
#include <cmath>
#include <QFile>
#include <QJsonDocument>
#include <QUrl>
#include <QUrlQuery>
#include <openssl/evp.h>
#include <openssl/pem.h>
#include <openssl/x509.h>

namespace junction {
namespace {

constexpr int kExchangeVersion = 1;

bool bad(QString* error, QString* code, const QString& text, const QString& machine) {
    if (error) *error = text;
    if (code) *code = machine;
    return false;
}

QString kindName(ExchangeKind kind) {
    switch (kind) {
    case ExchangeKind::Invite: return QStringLiteral("invite");
    case ExchangeKind::Response: return QStringLiteral("response");
    case ExchangeKind::Notice: return QStringLiteral("notice");
    }
    return {};
}

std::optional<ExchangeKind> kindFromName(const QString& name) {
    if (name == QLatin1String("invite")) return ExchangeKind::Invite;
    if (name == QLatin1String("response")) return ExchangeKind::Response;
    if (name == QLatin1String("notice")) return ExchangeKind::Notice;
    return std::nullopt;
}

/// Bounded free text. Never forwarded raw: control characters are stripped so a
/// remote reason string cannot corrupt a log line or a UI label.
QString sanitizeText(const QString& value, int limit) {
    QString out;
    out.reserve(std::min<qsizetype>(value.size(), limit));
    for (const QChar ch : value) {
        if (out.size() >= limit) break;
        if (ch.isPrint() || ch == QLatin1Char(' ')) out.append(ch);
    }
    return out.trimmed();
}

bool validDescriptionType(const QString& type) {
    return type == QLatin1String("offer") || type == QLatin1String("answer");
}

QString hexOf(const unsigned char* data, unsigned int length) {
    return QString::fromLatin1(QByteArray(reinterpret_cast<const char*>(data), int(length)).toHex());
}

} // namespace

QString exchangeTextPrefix() { return QStringLiteral("PLUMDECK-JUNCTION-1."); }

QString exchangeStateName(ExchangeState state) {
    switch (state) {
    case ExchangeState::Idle: return QStringLiteral("idle");
    case ExchangeState::Collecting: return QStringLiteral("collecting");
    case ExchangeState::InviteReady: return QStringLiteral("invite_ready");
    case ExchangeState::AwaitingAnswer: return QStringLiteral("awaiting_answer");
    case ExchangeState::ApprovalPending: return QStringLiteral("approval_pending");
    case ExchangeState::ResponseReady: return QStringLiteral("response_ready");
    case ExchangeState::AwaitingHost: return QStringLiteral("awaiting_host");
    case ExchangeState::Connecting: return QStringLiteral("connecting");
    case ExchangeState::Connected: return QStringLiteral("connected");
    case ExchangeState::Interrupted: return QStringLiteral("interrupted");
    case ExchangeState::NeedsExchange: return QStringLiteral("needs_exchange");
    case ExchangeState::Failed: return QStringLiteral("failed");
    case ExchangeState::Expired: return QStringLiteral("expired");
    case ExchangeState::Cancelled: return QStringLiteral("cancelled");
    case ExchangeState::Rejected: return QStringLiteral("rejected");
    }
    return QStringLiteral("idle");
}

std::optional<ExchangeState> exchangeStateFromName(const QString& name) {
    static const ExchangeState all[] = {
        ExchangeState::Idle, ExchangeState::Collecting, ExchangeState::InviteReady,
        ExchangeState::AwaitingAnswer, ExchangeState::ApprovalPending, ExchangeState::ResponseReady,
        ExchangeState::AwaitingHost, ExchangeState::Connecting, ExchangeState::Connected,
        ExchangeState::Interrupted, ExchangeState::NeedsExchange, ExchangeState::Failed,
        ExchangeState::Expired, ExchangeState::Cancelled, ExchangeState::Rejected,
    };
    for (const auto state : all)
        if (exchangeStateName(state) == name) return state;
    return std::nullopt;
}

bool exchangeStateTerminal(ExchangeState state) {
    switch (state) {
    case ExchangeState::Failed:
    case ExchangeState::Expired:
    case ExchangeState::Cancelled:
    case ExchangeState::Rejected:
    case ExchangeState::NeedsExchange:
        return true;
    default:
        return false;
    }
}

QString sdpFingerprint(const QString& sdp) {
    for (const auto& line : sdp.split(QLatin1Char('\n'))) {
        const auto trimmed = line.trimmed();
        if (!trimmed.startsWith(QLatin1String("a=fingerprint:"))) continue;
        const auto parts = trimmed.mid(14).split(QLatin1Char(' '), Qt::SkipEmptyParts);
        if (parts.size() != 2 || parts[0].compare(QLatin1String("sha-256"), Qt::CaseInsensitive) != 0) continue;
        const auto value = QString(parts[1]).remove(QLatin1Char(':')).toLower();
        if (validHexDigest(value, 32)) return value;
    }
    return {};
}

QJsonObject ExchangePacket::payload() const {
    QJsonObject object{
        {"v", kExchangeVersion}, {"engineVersion", "mixxx-3ebac449e7e5fe2a0186596657696e87ce8b0e56-junction-5"},
        {"kind", kindName(kind)},
        {"sessionId", sessionId},
        {"inviteId", inviteId},
        {"hostPeerId", hostPeerId},
        {"hostFingerprint", hostFingerprint},
        {"peerId", peerId},
        {"generation", u64(generation)},
        {"attempt", u64(attempt)},
        {"expiresAt", double(expiresAt)},
    };
    if (!sessionName.isEmpty()) object["sessionName"] = sessionName;
    if (!hostName.isEmpty()) object["hostName"] = hostName;
    if (!peerName.isEmpty()) object["peerName"] = peerName;
    if (!peerFingerprint.isEmpty()) object["peerFingerprint"] = peerFingerprint;
    if (!noticeReason.isEmpty()) object["noticeReason"] = noticeReason;
    if (!noticeText.isEmpty()) object["noticeText"] = noticeText;
    if (!iceServers.isEmpty()) object["iceServers"] = iceServers;
    QJsonArray descriptions;
    for (const auto& value : description) {
        if (value.isEmpty()) continue;
        descriptions.append(QJsonObject{{"type", value.type}, {"sdp", value.sdp}});
    }
    if (!descriptions.isEmpty()) object["sdp"] = descriptions;
    return object;
}

QByteArray ExchangePacket::canonicalPayload() const {
    return QJsonDocument(payload()).toJson(QJsonDocument::Compact);
}

QJsonObject ExchangePacket::sanitized() const {
    return {
        {"kind", kindName(kind)},   {"sessionName", sessionName}, {"hostName", hostName},
        {"hostPeerId", hostPeerId}, {"peerId", peerId},           {"inviteId", inviteId},
        {"expiresAt", double(expiresAt)},
    };
}

QString ExchangePacket::descriptionFingerprint() const {
    return kind == ExchangeKind::Response ? peerFingerprint : hostFingerprint;
}

QString encodeExchangePacket(const ExchangePacket& packet, QString* error) {
    auto object = packet.payload();
    if (!packet.certificatePem.isEmpty()) object["cert"] = QString::fromLatin1(packet.certificatePem);
    if (!packet.signature.isEmpty()) object["sig"] = QString::fromLatin1(packet.signature.toBase64());
    const auto bytes = QJsonDocument(object).toJson(QJsonDocument::Compact);
    if (bytes.size() > kMaxExchangeJsonBytes) {
        if (error) *error = QStringLiteral("接続情報が大きすぎます。ネットワーク設定を見直してもう一度お試しください");
        return {};
    }
    const auto text = exchangeTextPrefix() + QString::fromLatin1(bytes.toBase64(
        QByteArray::Base64UrlEncoding | QByteArray::OmitTrailingEquals));
    if (text.size() > kMaxExchangeTextBytes) {
        if (error) *error = QStringLiteral("接続情報が大きすぎます。ネットワーク設定を見直してもう一度お試しください");
        return {};
    }
    return text;
}

std::optional<QJsonArray> validateExchangeIceServers(const QJsonArray& servers, qint64 nowMs, QString* error) {
    const auto failure=validateNetworkServers(servers,nowMs);
    if(!failure.isEmpty()){if(error)*error=failure;return std::nullopt;}
    return servers;
}

std::optional<ExchangePacket> decodeExchangePacket(const QString& text, qint64 nowMs, QString* error, QString* errorCode) {
    if (errorCode) errorCode->clear();
    const auto trimmed = text.trimmed();
    if (trimmed.isEmpty()) {
        bad(error, errorCode, QStringLiteral("接続情報を貼り付けてください"), QStringLiteral("invalid"));
        return std::nullopt;
    }
    if (trimmed.size() > kMaxExchangeTextBytes) {
        bad(error, errorCode, QStringLiteral("接続情報が大きすぎます"), QStringLiteral("too_large"));
        return std::nullopt;
    }
    if (!trimmed.startsWith(exchangeTextPrefix())) {
        // Give the version mismatch its own code: the text is well-formed for
        // some other build, and telling the user to update is actionable.
        const auto generic = QStringLiteral("PLUMDECK-JUNCTION-");
        if (trimmed.startsWith(generic)) {
            bad(error, errorCode, QStringLiteral("この接続情報は別のバージョンのものです。両方のアプリを更新してください"),
                QStringLiteral("unsupported_version"));
            return std::nullopt;
        }
        bad(error, errorCode, QStringLiteral("Junctionの接続情報ではありません"), QStringLiteral("invalid"));
        return std::nullopt;
    }
    const auto encoded = trimmed.mid(exchangeTextPrefix().size()).toLatin1();
    const auto decoded = QByteArray::fromBase64Encoding(encoded, QByteArray::Base64UrlEncoding
                                                                     | QByteArray::AbortOnBase64DecodingErrors);
    if (!decoded) {
        bad(error, errorCode, QStringLiteral("接続情報が壊れています。全文をコピーできているか確認してください"), QStringLiteral("invalid"));
        return std::nullopt;
    }
    if ((*decoded).size() > kMaxExchangeJsonBytes) {
        bad(error, errorCode, QStringLiteral("接続情報が大きすぎます"), QStringLiteral("too_large"));
        return std::nullopt;
    }
    QJsonParseError parse{};
    const auto document = QJsonDocument::fromJson(*decoded, &parse);
    if (parse.error != QJsonParseError::NoError || !document.isObject()) {
        bad(error, errorCode, QStringLiteral("接続情報が壊れています。全文をコピーできているか確認してください"), QStringLiteral("invalid"));
        return std::nullopt;
    }
    const auto object = document.object();
    if (object["v"].toInt(-1) != kExchangeVersion) {
        bad(error, errorCode, QStringLiteral("この接続情報は別のバージョンのものです。両方のアプリを更新してください"),
            QStringLiteral("unsupported_version"));
        return std::nullopt;
    }
    if(object["engineVersion"]!="mixxx-3ebac449e7e5fe2a0186596657696e87ce8b0e56-junction-5"){
        bad(error,errorCode,"Junctionの基礎プロトコルに互換性がありません","unsupported_version");return std::nullopt;
    }
    const auto kind = kindFromName(object["kind"].toString());
    if (!kind) {
        bad(error, errorCode, QStringLiteral("接続情報の種類が不明です"), QStringLiteral("schema"));
        return std::nullopt;
    }

    ExchangePacket packet;
    packet.kind = *kind;
    packet.sessionId = object["sessionId"].toString();
    packet.inviteId = object["inviteId"].toString();
    packet.hostPeerId = object["hostPeerId"].toString();
    packet.peerId = object["peerId"].toString();
    packet.hostFingerprint = object["hostFingerprint"].toString().toLower();
    packet.peerFingerprint = object["peerFingerprint"].toString().toLower();
    packet.sessionName = sanitizeText(object["sessionName"].toString(), 80);
    packet.hostName = sanitizeDisplayName(object["hostName"].toString());
    packet.peerName = sanitizeDisplayName(object["peerName"].toString());
    packet.noticeReason = sanitizeText(object["noticeReason"].toString(), 32);
    packet.noticeText = sanitizeText(object["noticeText"].toString(), 200);

    if (!validOpaqueId(packet.sessionId) || !validOpaqueId(packet.inviteId)
        || !validOpaqueId(packet.hostPeerId) || !validOpaqueId(packet.peerId)) {
        bad(error, errorCode, QStringLiteral("接続情報の識別子が不正です"), QStringLiteral("schema"));
        return std::nullopt;
    }
    if (!validHexDigest(packet.hostFingerprint, 32)) {
        bad(error, errorCode, QStringLiteral("ホストの識別情報が不正です"), QStringLiteral("schema"));
        return std::nullopt;
    }
    if (packet.kind == ExchangeKind::Response && !validHexDigest(packet.peerFingerprint, 32)) {
        bad(error, errorCode, QStringLiteral("参加者の識別情報が不正です"), QStringLiteral("schema"));
        return std::nullopt;
    }
    const auto generation = parseU64(object["generation"]), attempt = parseU64(object["attempt"]);
    if (!generation || !attempt || *generation == 0) {
        bad(error, errorCode, QStringLiteral("接続情報の世代が不正です"), QStringLiteral("schema"));
        return std::nullopt;
    }
    packet.generation = *generation;
    packet.attempt = *attempt;

    const double rawExpiry=object["expiresAt"].toDouble(-1);
    if(!std::isfinite(rawExpiry)||std::floor(rawExpiry)!=rawExpiry||rawExpiry<1||rawExpiry>double(nowMs+kMaxExchangeLifetimeMs)){bad(error,errorCode,"有効期限の形式が不正です","schema");return std::nullopt;}
    const auto expiresAt = qint64(rawExpiry);
    if (expiresAt <= 0 || expiresAt > nowMs + kMaxExchangeLifetimeMs) {
        bad(error, errorCode, QStringLiteral("接続情報の有効期限が不正です"), QStringLiteral("schema"));
        return std::nullopt;
    }
    packet.expiresAt = expiresAt;

    const auto descriptions = object["sdp"].toArray();
    if (packet.kind == ExchangeKind::Notice) {
        if (!descriptions.isEmpty()) {
            bad(error, errorCode, QStringLiteral("通知に接続情報が含まれています"), QStringLiteral("schema"));
            return std::nullopt;
        }
    } else {
        if (descriptions.size() != 2) {
            bad(error, errorCode, QStringLiteral("接続情報が揃っていません。全文をコピーできているか確認してください"),
                QStringLiteral("schema"));
            return std::nullopt;
        }
        const auto expectedType = packet.kind == ExchangeKind::Invite ? QLatin1String("offer") : QLatin1String("answer");
        for (int index = 0; index < 2; ++index) {
            const auto row = descriptions.at(index).toObject();
            ExchangeDescription value{row["type"].toString(), row["sdp"].toString()};
            if (!validDescriptionType(value.type) || value.type != expectedType
                || value.sdp.isEmpty() || value.sdp.size() > kMaxExchangeSdpBytes) {
                bad(error, errorCode, QStringLiteral("接続情報の形式が不正です"), QStringLiteral("schema"));
                return std::nullopt;
            }
            // A packet may never authorise a DTLS identity it does not name.
            if (sdpFingerprint(value.sdp) != packet.descriptionFingerprint()) {
                bad(error, errorCode, QStringLiteral("接続情報の識別情報が一致しません"), QStringLiteral("schema"));
                return std::nullopt;
            }
            packet.description[index] = value;
        }
    }

    if (object.contains(QLatin1String("iceServers"))) {
        QString reason;
        auto servers = validateExchangeIceServers(object["iceServers"].toArray(), nowMs, &reason);
        if (!servers) {
            bad(error, errorCode, reason, QStringLiteral("schema"));
            return std::nullopt;
        }
        packet.iceServers = *servers;
    }

    packet.certificatePem = object["cert"].toString().toLatin1();
    if (packet.certificatePem.size() > 8192) {
        bad(error, errorCode, QStringLiteral("ホストの証明書が不正です"), QStringLiteral("schema"));
        return std::nullopt;
    }
    packet.signature = QByteArray::fromBase64(object["sig"].toString().toLatin1());
    if (packet.signature.size() > 512) {
        bad(error, errorCode, QStringLiteral("署名が不正です"), QStringLiteral("schema"));
        return std::nullopt;
    }

    // Expiry is checked last so that `exchange.inspect` can still explain what
    // the packet was before reporting that it is stale.
    if (expiresAt <= nowMs) {
        bad(error, errorCode, QStringLiteral("この接続情報は期限切れです。相手に再発行を依頼してください"), QStringLiteral("expired"));
        return std::nullopt;
    }
    return packet;
}

QByteArray readCertificatePem(const QString& certificatePemPath) {
    QFile file(certificatePemPath);
    if (!file.open(QIODevice::ReadOnly)) return {};
    return file.read(8192);
}

QByteArray signExchangePayload(const QString& privateKeyPemPath, const QByteArray& payload, QString* error) {
    QFile file(privateKeyPemPath);
    if (!file.open(QIODevice::ReadOnly)) {
        if (error) *error = QStringLiteral("この端末の署名鍵を読み込めません");
        return {};
    }
    const auto pem = file.read(16384);
    BIO* bio = BIO_new_mem_buf(pem.constData(), int(pem.size()));
    EVP_PKEY* key = bio ? PEM_read_bio_PrivateKey(bio, nullptr, nullptr, nullptr) : nullptr;
    EVP_MD_CTX* context = key ? EVP_MD_CTX_new() : nullptr;
    QByteArray signature;
    bool ok = context && EVP_DigestSignInit(context, nullptr, EVP_sha256(), nullptr, key) == 1;
    size_t length = 0;
    ok = ok && EVP_DigestSign(context, nullptr, &length,
                              reinterpret_cast<const unsigned char*>(payload.constData()), size_t(payload.size())) == 1
         && length > 0 && length <= 512;
    if (ok) {
        signature.resize(qsizetype(length));
        ok = EVP_DigestSign(context, reinterpret_cast<unsigned char*>(signature.data()), &length,
                            reinterpret_cast<const unsigned char*>(payload.constData()), size_t(payload.size())) == 1;
        if (ok) signature.resize(qsizetype(length));
    }
    if (context) EVP_MD_CTX_free(context);
    if (key) EVP_PKEY_free(key);
    if (bio) BIO_free(bio);
    if (!ok) {
        if (error) *error = QStringLiteral("接続情報に署名できません");
        return {};
    }
    return signature;
}

bool verifyExchangeSignature(const ExchangePacket& packet, const QString& pinnedFingerprint, QString* error) {
    if (packet.certificatePem.isEmpty() || packet.signature.isEmpty()) {
        if (error) *error = QStringLiteral("この接続情報には署名がありません");
        return false;
    }
    BIO* bio = BIO_new_mem_buf(packet.certificatePem.constData(), int(packet.certificatePem.size()));
    X509* certificate = bio ? PEM_read_bio_X509(bio, nullptr, nullptr, nullptr) : nullptr;
    bool ok = certificate != nullptr;
    QString fingerprint;
    if (ok) {
        unsigned char digest[EVP_MAX_MD_SIZE];
        unsigned int length = 0;
        ok = X509_digest(certificate, EVP_sha256(), digest, &length) == 1;
        if (ok) fingerprint = hexOf(digest, length);
    }
    // Both bindings are required: the certificate must be the identity the
    // packet itself names (host for invites/notices, guest for responses) and,
    // when the caller has pinned one, that exact identity; and the signature
    // must be by that certificate's key.
    const auto declared = packet.descriptionFingerprint();
    const auto expected = pinnedFingerprint.isEmpty() ? declared : pinnedFingerprint.toLower();
    if (ok && (fingerprint != expected || fingerprint != declared)) {
        if (certificate) X509_free(certificate);
        if (bio) BIO_free(bio);
        if (error)
            *error = packet.kind == ExchangeKind::Response
                         ? QStringLiteral("参加者の識別情報が一致しません")
                         : QStringLiteral("ホストの識別情報が一致しません。この接続情報は使用できません");
        return false;
    }
    EVP_PKEY* key = ok ? X509_get_pubkey(certificate) : nullptr;
    EVP_MD_CTX* context = key ? EVP_MD_CTX_new() : nullptr;
    const auto payload = packet.canonicalPayload();
    ok = ok && context && EVP_DigestVerifyInit(context, nullptr, EVP_sha256(), nullptr, key) == 1
         && EVP_DigestVerify(context, reinterpret_cast<const unsigned char*>(packet.signature.constData()),
                             size_t(packet.signature.size()),
                             reinterpret_cast<const unsigned char*>(payload.constData()), size_t(payload.size())) == 1;
    if (context) EVP_MD_CTX_free(context);
    if (key) EVP_PKEY_free(key);
    if (certificate) X509_free(certificate);
    if (bio) BIO_free(bio);
    if (!ok && error) *error = QStringLiteral("接続情報の署名を確認できません");
    return ok;
}

} // namespace junction
