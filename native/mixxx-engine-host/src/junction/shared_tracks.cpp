#include "shared_tracks.h"
#include "session_protocol.h"
#include <algorithm>
#include <cmath>

namespace junction {
namespace {

/// Display text from another computer: printable, single line and bounded.
QString displayText(const QJsonValue& value, int limit) {
    QString out;
    for (const QChar ch : value.toString()) if (ch.isPrint() && ch != QChar::LineSeparator && ch != QChar::ParagraphSeparator) out.append(ch);
    out = out.trimmed();
    if (out.size() > limit) out.truncate(limit);
    return out;
}

bool finiteIn(const QJsonValue& value, double low, double high) {
    return value.isDouble() && std::isfinite(value.toDouble()) && value.toDouble() >= low && value.toDouble() <= high;
}

bool fail(QString* error, const QString& reason) {
    if (error) *error = reason;
    return false;
}

}

QJsonObject trackAnnouncement(const std::vector<AnnouncedTrack>& rows, const QString& stream, quint64 revision) {
    QJsonArray decks;
    for (const auto& row : rows) {
        decks.append(QJsonObject{
            {"role", row.role}, {"deck", row.deck}, {"assetId", row.assetId}, {"sizeBytes", u64(row.sizeBytes)},
            {"title", row.title}, {"artist", row.artist}, {"musicalKey", row.musicalKey},
            {"durationMs", row.durationMs}, {"bpm", row.bpm}, {"positionMs", row.positionMs},
            {"rate", row.rate}, {"audibility", row.audibility}, {"playing", row.playing},
        });
    }
    return {{"stream", stream}, {"revision", u64(revision)}, {"decks", decks}};
}

std::optional<std::vector<AnnouncedTrack>> parseTrackAnnouncement(const QJsonObject& payload, QString* stream, quint64* revision, QString* error) {
    const auto sequence = parseU64(payload["revision"]);
    if (!validOpaqueId(payload["stream"].toString()) || !sequence) { fail(error, QStringLiteral("stream and revision are required")); return std::nullopt; }
    if (!payload["decks"].isArray() || payload["decks"].toArray().size() > kMaxAnnouncedDecks) { fail(error, QStringLiteral("at most current and next")); return std::nullopt; }
    std::vector<AnnouncedTrack> rows;
    QSet<QString> decks, roles;
    for (const auto& value : payload["decks"].toArray()) {
        const auto row = value.toObject();
        AnnouncedTrack track;
        track.role = row["role"].toString();
        track.deck = row["deck"].toString();
        track.assetId = row["assetId"].toString();
        const auto size = parseU64(row["sizeBytes"]);
        if (!value.isObject() || !QStringList{"current", "next"}.contains(track.role) || roles.contains(track.role)
                || !QStringList{"A", "B", "C", "D"}.contains(track.deck) || decks.contains(track.deck)
                || !validHexDigest(track.assetId, 32) || !size || !*size || *size > kMaxAnnouncedAssetBytes
                || !row["title"].isString() || !row["artist"].isString() || !row["musicalKey"].isString() || !row["playing"].isBool()
                || !finiteIn(row["durationMs"], 0, 86400000.0) || !finiteIn(row["bpm"], 0, 1000)
                || !finiteIn(row["positionMs"], -60000, 86400000.0) || !finiteIn(row["rate"], 0.25, 4.0)
                || !finiteIn(row["audibility"], 0, 16.0)) {
            fail(error, QStringLiteral("invalid announced deck"));
            return std::nullopt;
        }
        decks.insert(track.deck); roles.insert(track.role);
        track.sizeBytes = *size;
        track.title = displayText(row["title"], 200);
        track.artist = displayText(row["artist"], 200);
        track.musicalKey = displayText(row["musicalKey"], 16);
        track.durationMs = row["durationMs"].toDouble();
        track.bpm = row["bpm"].toDouble();
        track.positionMs = row["positionMs"].toDouble();
        track.rate = row["rate"].toDouble();
        track.audibility = row["audibility"].toDouble();
        track.playing = row["playing"].toBool();
        rows.push_back(track);
    }
    if (stream) *stream = payload["stream"].toString();
    if (revision) *revision = *sequence;
    return rows;
}

QStringList SharedTrackList::apply(const QString& source, const QString& sourceName, const std::vector<AnnouncedTrack>& rows) {
    QStringList added;
    for (auto& entry : entries_) if (entry.sourcePeerId == source) { entry.onDeck = false; entry.track.playing = false; }
    for (const auto& row : rows) {
        if (auto* entry = find(row.assetId)) {
            // The same audio may be announced again, possibly by another DJ.
            // It stays a single entry; the latest announcement describes it.
            entry->track.deck = row.deck;
            entry->track.role = row.role;
            entry->track.playing = row.playing;
            entry->track.positionMs = row.positionMs;
            entry->track.rate = row.rate;
            entry->track.audibility = row.audibility;
            if (!row.title.isEmpty()) entry->track.title = row.title;
            if (!row.artist.isEmpty()) entry->track.artist = row.artist;
            if (!row.musicalKey.isEmpty()) entry->track.musicalKey = row.musicalKey;
            if (row.durationMs > 0) entry->track.durationMs = row.durationMs;
            if (row.bpm > 0) entry->track.bpm = row.bpm;
            entry->onDeck = true;
            entry->sourcePeerId = source;
            entry->sourceName = sourceName;
            continue;
        }
        if (int(entries_.size()) >= kMaxEntries) {
            const auto stale = std::find_if(entries_.begin(), entries_.end(), [](const Entry& entry) { return !entry.onDeck; });
            if (stale == entries_.end()) continue;
            entries_.erase(stale);
        }
        Entry entry;
        entry.track = row;
        entry.sourcePeerId = source;
        entry.sourceName = sourceName;
        entry.onDeck = true;
        entry.order = ++order_;
        entries_.push_back(entry);
        added.append(row.assetId);
    }
    entries_.erase(std::remove_if(entries_.begin(), entries_.end(), [](const Entry& entry) { return !entry.onDeck; }), entries_.end());
    return added;
}

void SharedTrackList::retainPendingFrom(const QSet<QString>& sources) {
    entries_.erase(std::remove_if(entries_.begin(), entries_.end(), [&](const Entry& entry) { return entry.state == State::Pending && !sources.contains(entry.sourcePeerId); }), entries_.end());
}

const SharedTrackList::Entry* SharedTrackList::next() const {
    const Entry* best = nullptr;
    for (const auto& entry : entries_) {
        if (entry.state != State::Pending || !entry.onDeck) continue;
        if (!best || (entry.track.role == "current" && best->track.role != "current")) best = &entry;
    }
    return best;
}

QSet<QString> SharedTrackList::assetIds() const {
    QSet<QString> result;
    for (const auto& entry : entries_) result.insert(entry.track.assetId);
    return result;
}

SharedTrackList::Entry* SharedTrackList::find(const QString& assetId) {
    for (auto& entry : entries_) if (entry.track.assetId == assetId) return &entry;
    return nullptr;
}

const SharedTrackList::Entry* SharedTrackList::find(const QString& assetId) const {
    for (const auto& entry : entries_) if (entry.track.assetId == assetId) return &entry;
    return nullptr;
}

QJsonArray SharedTrackList::toJson(const std::function<double(const Entry&)>& progress) const {
    QJsonArray rows;
    for (const auto& entry : entries_) {
        const bool ready = entry.state == State::Ready;
        QJsonObject row{
            {"assetId", entry.track.assetId}, {"title", entry.track.title}, {"artist", entry.track.artist},
            {"musicalKey", entry.track.musicalKey}, {"durationMs", entry.track.durationMs}, {"bpm", entry.track.bpm},
            {"sizeBytes", double(entry.track.sizeBytes)}, {"sourcePeerId", entry.sourcePeerId}, {"sourceDjName", entry.sourceName},
            {"role", entry.track.role}, {"sourceDeck", entry.onDeck ? entry.track.deck : QString{}}, {"onDeck", entry.onDeck}, {"playing", entry.onDeck && entry.track.playing},
            {"positionMs", entry.track.positionMs}, {"rate", entry.track.rate}, {"audibility", entry.track.audibility},
            {"state", sharedTrackStateName(entry.state)}, {"ready", ready}, {"order", double(entry.order)},
            {"progress", ready ? 1.0 : progress && (entry.state == State::Receiving || entry.state == State::Verifying) ? std::clamp(progress(entry), 0.0, 1.0) : 0.0},
        };
        if (!entry.detail.isEmpty()) row["detail"] = entry.detail;
        if (ready) row["path"] = entry.path;
        rows.append(row);
    }
    return rows;
}

QString sharedTrackStateName(SharedTrackList::State state) {
    switch (state) {
    case SharedTrackList::State::Pending: return QStringLiteral("pending");
    case SharedTrackList::State::Receiving: return QStringLiteral("receiving");
    case SharedTrackList::State::Verifying: return QStringLiteral("verifying");
    case SharedTrackList::State::Ready: return QStringLiteral("ready");
    case SharedTrackList::State::Failed: return QStringLiteral("failed");
    }
    return QStringLiteral("failed");
}

}
