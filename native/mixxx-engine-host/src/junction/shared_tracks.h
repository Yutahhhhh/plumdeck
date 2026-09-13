#pragma once
#include <QJsonArray>
#include <QJsonObject>
#include <QSet>
#include <QString>
#include <QStringList>
#include <functional>
#include <optional>
#include <vector>

namespace junction {

/// One of the two presentation tracks of the current performer, as announced
/// to the coordinator. `current` is on air; `next` is prefetched but hidden
/// behind the same Junction Live source.
/// There is deliberately no path: the sender's filesystem layout never leaves
/// its own process. The receiver resolves `assetId` in its own cache.
struct AnnouncedTrack {
    QString role, deck, assetId, title, artist, musicalKey;
    quint64 sizeBytes = 0;
    double durationMs = 0, bpm = 0, positionMs = 0, rate = 1, audibility = 0;
    bool playing = false;
};

inline constexpr int kMaxAnnouncedDecks = 2;
/// Upper bound for one Junction Live asset. Far above any DJ audio file, and
/// well inside the shared cache quota so one copy cannot evict everything.
inline constexpr quint64 kMaxAnnouncedAssetBytes = 2ULL * 1024 * 1024 * 1024;

/// `tracks.announce` payload. `stream` scopes `revision` to one sender run so a
/// restarted process is not ignored as stale.
QJsonObject trackAnnouncement(const std::vector<AnnouncedTrack>& rows, const QString& stream, quint64 revision);
/// Strict parser: any malformed row rejects the whole announcement.
std::optional<std::vector<AnnouncedTrack>> parseTrackAnnouncement(const QJsonObject& payload, QString* stream, quint64* revision, QString* error = nullptr);

/// Coordinator-side current/next presentation pair. This is not a playlist:
/// the UI exposes one Junction Live source and follows the `current` entry.
/// `toJson` is local UI state and must never be sent to a peer.
class SharedTrackList {
public:
    enum class State { Pending, Receiving, Verifying, Ready, Failed };
    struct Entry {
        AnnouncedTrack track;
        QString sourcePeerId, sourceName, path, detail;
        State state = State::Pending;
        bool onDeck = false;
        quint64 order = 0;
        unsigned attempts = 0;
    };
    static constexpr int kMaxEntries = 2;

    /// Applies one validated announcement. Entries absent from the newest
    /// current/next pair are removed, including already cached files.
    /// Returns the asset ids that were not listed before.
    QStringList apply(const QString& source, const QString& sourceName, const std::vector<AnnouncedTrack>& rows);
    /// Drops never-started entries whose source may no longer supply them.
    void retainPendingFrom(const QSet<QString>& sources);
    /// Next asset to fetch: a playing deck first, then any loaded deck.
    const Entry* next() const;
    Entry* find(const QString& assetId);
    const Entry* find(const QString& assetId) const;
    const std::vector<Entry>& entries() const { return entries_; }
    QSet<QString> assetIds() const;
    bool empty() const { return entries_.empty(); }
    void clear() { entries_.clear(); }
    /// `progress` reports 0..1 for in-flight entries. Paths appear only when ready.
    QJsonArray toJson(const std::function<double(const Entry&)>& progress = {}) const;

private:
    std::vector<Entry> entries_;
    quint64 order_ = 0;
};

QString sharedTrackStateName(SharedTrackList::State state);

}
