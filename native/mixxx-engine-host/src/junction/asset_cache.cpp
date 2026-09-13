#include "asset_cache.h"
#include "ids.h"
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QCryptographicHash>
#include <QSaveFile>
#include <QJsonDocument>
#include <QJsonObject>
#include <map>
#include <set>
#include "../platform_file.h"
namespace junction {
namespace { bool valid(const QString& s) { if(s.size()!=64)return false; for(auto c:s)if(!((c>='0'&&c<='9')||(c>='a'&&c<='f')))return false;return true; }
bool fail(QString* e,const QString& s){if(e)*e=s;return false;} }
struct AssetCache::Impl {
 QString root; quint64 quota; struct Transfer {quint64 size;QByteArray bitmap;}; std::map<QString,Transfer> transfers;std::set<QString> pins;
 QString path(const QString& id,const QString& suffix={})const{return root+"/"+id+suffix;}
 bool safe(const QString& p)const{return !QFileInfo(p).isSymLink()&&QFileInfo(root).canonicalFilePath()==root;}
 bool save(const QString& id) {const auto&t=transfers.at(id);QSaveFile f(path(id,".resume"));if(!f.open(QIODevice::WriteOnly))return false;const auto b=QJsonDocument(QJsonObject{{"bytes",u64(t.size)},{"bitmap",QString::fromLatin1(t.bitmap.toBase64())}}).toJson(QJsonDocument::Compact);return f.write(b)==b.size()&&f.commit();}
};
AssetCache::AssetCache(QString root,quint64 quota):d(new Impl){QDir().mkpath(root);d->root=QFileInfo(root).canonicalFilePath();d->quota=quota;}
AssetCache::~AssetCache()=default;
QString AssetCache::hashFile(const QString& path,QString* error){QFile f(path);if(!f.open(QIODevice::ReadOnly)){fail(error,"Cannot read asset");return{};}QCryptographicHash h(QCryptographicHash::Sha256);while(!f.atEnd()){auto b=f.read(1024*1024);if(b.isEmpty()&&f.error()!=QFile::NoError){fail(error,"Asset read failed");return{};}h.addData(b);}return QString::fromLatin1(h.result().toHex());}
bool AssetCache::begin(const QString& id,quint64 bytes,QString* error){
 if(!valid(id)||!bytes||bytes>d->quota||d->root.isEmpty())return fail(error,"Invalid asset manifest");
 if(!d->safe(d->path(id,".partial"))||!d->safe(d->path(id,".resume")))return fail(error,"Unsafe cache path");
 quint64 used=0;auto entries=QDir(d->root).entryInfoList(QDir::Files,QDir::Time|QDir::Reversed);for(const auto& f:entries)used+=f.size();
 quint64 existing=quint64(QFileInfo(d->path(id,".partial")).size());quint64 reserve=bytes>existing?bytes-existing:0;if(reserve){for(const auto& f:entries){if(used+reserve<=d->quota)break;if(valid(f.fileName())&&!d->pins.count(f.fileName())&&!f.isSymLink()){if(QFile::remove(f.filePath()))used-=f.size();}}if(used+reserve>d->quota)return fail(error,"Cache quota exceeded");}
 Impl::Transfer t{bytes,QByteArray(int((bytes+kMaxChunkBytes-1)/kMaxChunkBytes),char(0))};QFile resume(d->path(id,".resume"));if(resume.open(QIODevice::ReadOnly)){auto obj=QJsonDocument::fromJson(resume.readAll()).object();auto old=parseU64(obj["bytes"]);auto bm=QByteArray::fromBase64(obj["bitmap"].toString().toLatin1());if(!old||*old!=bytes||bm.size()!=t.bitmap.size())return fail(error,"Resume manifest mismatch");t.bitmap=bm;}
 // QSaveFile replaces this manifest below; Windows requires the read handle closed.
 resume.close();
 QFile partial(d->path(id,".partial"));if(!platform_file::open(partial,true))return fail(error,"Cannot create partial asset");if(!partial.resize(qint64(bytes))||!partial.flush()||platform_file::sync(partial.handle())!=0)return fail(error,"Cannot reserve asset file");partial.close();d->transfers[id]=t;return d->save(id)||fail(error,"Cannot persist resume state");
}
bool AssetCache::put(const QString&id,quint64 offset,const QByteArray& chunk,QString* error){auto it=d->transfers.find(id);if(it==d->transfers.end())return fail(error,"Unknown transfer");auto&t=it->second;if(offset>=t.size||offset%kMaxChunkBytes||chunk.size()!=qint64(std::min<quint64>(kMaxChunkBytes,t.size-offset)))return fail(error,"Invalid chunk range");QFile partial(d->path(id,".partial"));if(!platform_file::open(partial,false)||!partial.seek(qint64(offset)))return fail(error,"Cannot open partial asset");auto slot=int(offset/kMaxChunkBytes);bool ok;if(t.bitmap[slot]){ok=partial.read(chunk.size())==chunk;}else{ok=partial.write(chunk)==chunk.size()&&partial.flush()&&platform_file::sync(partial.handle())==0;}partial.close();if(!ok)return fail(error,"Conflicting or failed chunk");t.bitmap[slot]=1;return d->save(id)||fail(error,"Cannot persist chunk state");}
bool AssetCache::finalize(const QString&id,const std::function<bool(const QString&)>& decoder,QString* error){return publish(id,{},decoder,error);}
bool AssetCache::finalizeVerified(const QString&id,const QString& digest,const std::function<bool(const QString&)>& decoder,QString* error){if(digest.isEmpty())return fail(error,"Asset hash mismatch");return publish(id,digest,decoder,error);}
bool AssetCache::publish(const QString&id,const QString& digest,const std::function<bool(const QString&)>& decoder,QString* error){auto it=d->transfers.find(id);if(it==d->transfers.end()||it->second.bitmap.contains(char(0)))return fail(error,"Asset incomplete");auto path=d->path(id,".partial");if(!d->safe(path)||(digest.isEmpty()?hashFile(path,error):digest)!=id)return fail(error,"Asset hash mismatch");if(!decoder||!decoder(path))return fail(error,"Asset decoder validation failed");if(!d->safe(d->path(id)))return fail(error,"Unsafe asset destination");if(QFile::exists(d->path(id))){if(hashFile(d->path(id))!=id)return fail(error,"Existing cache integrity failure");QFile::remove(path);}else if(!QFile::rename(path,d->path(id)))return fail(error,"Atomic publish failed");QFile::remove(d->path(id,".resume"));d->transfers.erase(it);return true;}
QString AssetCache::resolve(const QString&id)const{if(!valid(id)||!d->safe(d->path(id))||!QFileInfo(d->path(id)).isFile())return{};return hashFile(d->path(id))==id?d->path(id):QString{};}
QString AssetCache::existing(const QString&id)const{if(!valid(id)||!d->safe(d->path(id))||!QFileInfo(d->path(id)).isFile())return{};return d->path(id);}
QString AssetCache::partialPath(const QString&id)const{return valid(id)&&d->transfers.count(id)&&d->safe(d->path(id,".partial"))?d->path(id,".partial"):QString{};}
void AssetCache::discard(const QString&id){if(!valid(id))return;d->transfers.erase(id);QFile::remove(d->path(id,".partial"));QFile::remove(d->path(id,".resume"));}
QByteArray AssetCache::receivedBitmap(const QString&id)const{auto it=d->transfers.find(id);return it==d->transfers.end()?QByteArray{}:it->second.bitmap;}
void AssetCache::pin(const QString&id,bool pin){if(!valid(id))return;if(pin)d->pins.insert(id);else d->pins.erase(id);}
}
