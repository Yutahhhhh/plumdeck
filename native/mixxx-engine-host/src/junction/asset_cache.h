#pragma once
#include <QByteArray>
#include <QString>
#include <memory>
#include <functional>
namespace junction {
class AssetCache {
public:
    explicit AssetCache(QString root,quint64 quota=5ULL*1024*1024*1024);
    ~AssetCache();
    static QString hashFile(const QString& path,QString* error=nullptr);
    bool begin(const QString& sha256,quint64 bytes,QString* error=nullptr);
    bool put(const QString& sha256,quint64 offset,const QByteArray& chunk,QString* error=nullptr);
    // Decoder validation is required before publishing an asset as ready.
    bool finalize(const QString& sha256,const std::function<bool(const QString&)>& decoderAccepts,QString* error=nullptr);
    // Same contract as finalize for a caller that already hashed partialPath()
    // off the latency-critical path. A digest other than sha256 is rejected.
    bool finalizeVerified(const QString& sha256,const QString& partialDigest,const std::function<bool(const QString&)>& decoderAccepts,QString* error=nullptr);
    QString resolve(const QString& sha256) const;
    // Published file path without re-hashing it; callers must still verify.
    QString existing(const QString& sha256) const;
    QString partialPath(const QString& sha256) const;
    // Drops an unpublished transfer so a later begin() starts from scratch.
    void discard(const QString& sha256);
    QByteArray receivedBitmap(const QString& sha256) const;
    void pin(const QString& sha256,bool pinned);
private:
    bool publish(const QString& sha256,const QString& partialDigest,const std::function<bool(const QString&)>& decoderAccepts,QString* error);
    struct Impl; std::unique_ptr<Impl> d;
};
}
