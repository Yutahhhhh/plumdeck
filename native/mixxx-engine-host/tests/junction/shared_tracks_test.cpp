#include "harness.h"
#include "junction/asset_cache.h"
#include "junction/ids.h"
#include "junction/session_protocol.h"
#include "junction/shared_tracks.h"
#include <QJsonDocument>
#include <QTemporaryDir>
using namespace junction;

namespace {
AnnouncedTrack deck(const QString& name, char digit, bool playing = false) {
    AnnouncedTrack row;
    row.role = playing ? "current" : "next"; row.deck = name; row.assetId = QString(64, QLatin1Char(digit)); row.sizeBytes = 4096;
    row.title = "Title " + name; row.artist = "Artist"; row.musicalKey = "8A"; row.durationMs = 180000; row.bpm = 128;
    row.positionMs = playing ? 42000 : 0; row.rate = 1; row.audibility = playing ? 1 : 0; row.playing = playing;
    return row;
}
QByteArray text(const QJsonObject& value) { return QJsonDocument(value).toJson(QJsonDocument::Compact); }
}

JTEST("shared-tracks", "announcement carries content ids and metadata but never a path") {
    const auto payload = trackAnnouncement({deck("A", 'a', true), deck("B", 'b')}, "stream-0001", 7);
    const auto bytes = text(payload);
    CHECK(!bytes.contains("\"path\""));
    CHECK(!bytes.contains("localTrackId"));
    CHECK_EQ(messageTypeFromName("tracks.announce"), MessageType::TrackAnnounce);
    QString stream; quint64 revision = 0; QString error;
    const auto rows = parseTrackAnnouncement(payload, &stream, &revision, &error);
    CHECK(rows.has_value());
    CHECK_EQ(stream, QString("stream-0001"));
    CHECK_EQ(revision, 7ULL);
    CHECK_EQ(int(rows->size()), 2);
    CHECK_EQ((*rows)[0].assetId, QString(64, 'a'));
    CHECK_EQ((*rows)[0].role, QString("current"));
    CHECK_EQ((*rows)[0].positionMs, 42000.0);
    CHECK((*rows)[0].playing);
}

JTEST("shared-tracks", "malformed announcements are rejected as a whole and display text is bounded") {
    auto payload = trackAnnouncement({deck("A", 'a')}, "stream-0001", 1);
    const auto mutate = [&](const std::function<void(QJsonObject&)>& change) {
        auto copy = payload; auto rows = copy["decks"].toArray(); auto row = rows[0].toObject(); change(row); rows[0] = row; copy["decks"] = rows;
        return parseTrackAnnouncement(copy, nullptr, nullptr).has_value();
    };
    CHECK(!mutate([](QJsonObject& row) { row["deck"] = "E"; }));
    CHECK(!mutate([](QJsonObject& row) { row["assetId"] = QString(64, 'A'); }));
    CHECK(!mutate([](QJsonObject& row) { row["assetId"] = "../../etc/passwd"; }));
    CHECK(!mutate([](QJsonObject& row) { row["sizeBytes"] = "0"; }));
    CHECK(!mutate([](QJsonObject& row) { row["sizeBytes"] = 4096; }));
    CHECK(!mutate([](QJsonObject& row) { row["bpm"] = 5000; }));
    CHECK(!mutate([](QJsonObject& row) { row["role"] = "previous"; }));
    CHECK(!mutate([](QJsonObject& row) { row["positionMs"] = -70000; }));
    CHECK(!mutate([](QJsonObject& row) { row["rate"] = 0; }));
    CHECK(!mutate([](QJsonObject& row) { row["playing"] = "yes"; }));
    CHECK(!parseTrackAnnouncement(trackAnnouncement({deck("A", 'a'), deck("A", 'b')}, "stream-0001", 1), nullptr, nullptr));
    CHECK(!parseTrackAnnouncement(trackAnnouncement({deck("A", 'a', true), deck("B", 'b'), deck("C", 'c')}, "stream-0001", 1), nullptr, nullptr));
    CHECK(!parseTrackAnnouncement(trackAnnouncement({deck("A", 'a')}, "x", 1), nullptr, nullptr));
    auto noisy = deck("A", 'a'); noisy.title = QString("Line\nbreak ") + QString(400, 'x');
    const auto rows = parseTrackAnnouncement(trackAnnouncement({noisy}, "stream-0001", 1), nullptr, nullptr);
    CHECK(rows.has_value());
    CHECK(!(*rows)[0].title.contains('\n'));
    CHECK((*rows)[0].title.size() <= 200);
}

JTEST("shared-tracks", "the current and next pair replaces stale presentation state") {
    SharedTrackList list;
    CHECK_EQ(list.apply("dj-one-0001", "DJ One", {deck("A", 'a'), deck("B", 'b', true)}).size(), 2);
    CHECK_EQ(list.next()->track.assetId, QString(64, 'b'));
    list.find(QString(64, 'a'))->state = SharedTrackList::State::Ready;
    list.find(QString(64, 'a'))->path = "/cache/junction/" + QString(64, 'a');
    auto replacement = deck("C", 'c', true);
    CHECK_EQ(list.apply("dj-one-0001", "DJ One", {replacement}).size(), 1);
    CHECK(!list.find(QString(64, 'a')));
    CHECK(!list.find(QString(64, 'b')));
    CHECK(list.find(QString(64, 'c')));
    CHECK_EQ(list.find(QString(64, 'c'))->track.role, QString("current"));
    list.retainPendingFrom({"dj-one-0001"});
    CHECK(list.find(QString(64, 'c')));
    list.retainPendingFrom({});
    CHECK(!list.find(QString(64, 'c')));
}

JTEST("shared-tracks", "local UI json exposes this computer's path only for verified entries") {
    SharedTrackList list;
    list.apply("dj-one-0001", "DJ One", {deck("A", 'a', true), deck("B", 'b')});
    auto* ready = list.find(QString(64, 'a'));
    ready->state = SharedTrackList::State::Ready; ready->path = "/cache/junction/" + QString(64, 'a');
    list.find(QString(64, 'b'))->state = SharedTrackList::State::Receiving;
    list.find(QString(64, 'b'))->path = "/should/not/appear";
    const auto rows = list.toJson([](const SharedTrackList::Entry&) { return 0.5; });
    CHECK_EQ(rows[0].toObject()["path"].toString(), "/cache/junction/" + QString(64, 'a'));
    CHECK(rows[0].toObject()["ready"].toBool());
    CHECK(!rows[1].toObject().contains("path"));
    CHECK_EQ(rows[1].toObject()["state"].toString(), QString("receiving"));
    CHECK_EQ(rows[1].toObject()["progress"].toDouble(), 0.5);
    CHECK_EQ(rows[0].toObject()["sourceDeck"].toString(), QString("A"));
    CHECK_EQ(rows[0].toObject()["role"].toString(), QString("current"));
    CHECK_EQ(rows[0].toObject()["positionMs"].toDouble(), 42000.0);
}

JTEST("asset-cache", "sliced verification publishes only with the matching digest") {
    QTemporaryDir dir; QByteArray data(40000, 'y'); const auto id = sha256Hex(data); QString error;
    AssetCache cache(dir.path());
    CHECK(cache.existing(id).isEmpty());
    CHECK(cache.begin(id, data.size(), &error));
    CHECK(cache.put(id, 0, data.left(32768), &error));
    CHECK(cache.put(id, 32768, data.mid(32768), &error));
    CHECK(!cache.partialPath(id).isEmpty());
    CHECK(!cache.finalizeVerified(id, QString(64, '0'), [](auto) { return true; }, &error));
    CHECK(!cache.finalizeVerified(id, {}, [](auto) { return true; }, &error));
    CHECK(!cache.finalizeVerified(id, AssetCache::hashFile(cache.partialPath(id)), [](auto) { return false; }, &error));
    CHECK(cache.finalizeVerified(id, AssetCache::hashFile(cache.partialPath(id)), [](auto) { return true; }, &error));
    CHECK_EQ(cache.existing(id), cache.resolve(id));
    const QByteArray other(1000, 'z'); const auto otherId = sha256Hex(other);
    CHECK(cache.begin(otherId, other.size(), &error));
    cache.discard(otherId);
    CHECK(cache.partialPath(otherId).isEmpty());
    CHECK(cache.receivedBitmap(otherId).isEmpty());
}
