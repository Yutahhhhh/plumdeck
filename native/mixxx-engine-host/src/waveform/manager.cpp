#include "manager.h"
#include "pyramid.h"
#include "../deck_telemetry.h"
#include "track/track.h"
#include <QCryptographicHash>
#include <QDateTime>
#include <QDir>
#include <QStandardPaths>
#include <QFileInfo>
#include <QFile>
#include <QSaveFile>
#include <QJsonDocument>
#include <QUuid>
#include <QtEndian>
#include <cstring>
#include <optional>
#include <sys/stat.h>
namespace waveform {
namespace {
QString fingerprint(const QString& path) {
    return sourceFingerprint(path);
}
void save(const QString& path,const QByteArray& bytes) {
    QSaveFile file(path);if(!file.open(QIODevice::WriteOnly)||file.write(bytes)!=bytes.size()||!file.commit())throw std::runtime_error("Waveform cache write failed");
}
QByteArray encode(unsigned lod,std::uint64_t index,const std::vector<Bin>& bins,unsigned rate,bool bands,std::int64_t origin) {
    const unsigned count=bins.size(), payload=52*count;
    QByteArray bytes(64+payload,'\0');auto* out=reinterpret_cast<unsigned char*>(bytes.data());
    const auto u16=[&](int offset,quint16 v){qToLittleEndian(v,out+offset);};
    const auto u32=[&](int offset,quint32 v){qToLittleEndian(v,out+offset);};
    const auto u64=[&](int offset,quint64 v){qToLittleEndian(v,out+offset);};
    std::memcpy(out,"DJWV",4);u16(4,2);u16(6,64);u32(8,bands?1:0);u16(12,2);u16(14,lod);
    u32(16,count);u32(20,64u<<lod);u64(24,origin+index*2048*(std::uint64_t(64)<<lod));
    std::uint64_t covered=0;for(unsigned b=0;b<count;++b){covered+=bins[b].count;u32(64+b*4,bins[b].count);}
    u64(32,covered);u32(40,rate);u32(44,payload);u32(52,6);
    for(unsigned field=0;field<6;++field)for(unsigned channel=0;channel<2;++channel)for(unsigned b=0;b<count;++b){
        const auto& bin=bins[b];const float value=field==0?bin.min[channel]:field==1?bin.max[channel]:(field>=3&&!bands?0:bin.squares[field-2][channel]/bin.count);
        std::uint32_t bits;std::memcpy(&bits,&value,4);u32(64+count*4+((field*2+channel)*count+b)*4,bits);
    }
    u32(48,crc32(out+64,payload));return bytes;
}
}
Manager::Manager():root_(qEnvironmentVariable("PLUMDECK_WAVEFORM_CACHE")) {
    if(root_.isEmpty())root_=QStandardPaths::writableLocation(QStandardPaths::GenericCacheLocation)+"/plumdeck/waveform-v2";
    QDir().mkpath(root_);worker_=std::thread([this]{run();});
}
Manager::~Manager(){stop_=true;wake_.notify_one();if(worker_.joinable())worker_.join();}
QJsonObject Manager::ensure(const QString& path,quint64 generation) {
    const auto binding=ensureSourceBinding(path);if(!binding.provider)return {{"state","error"},{"error","DECODE_FAILED"}};
    const auto fp=fingerprint(path);if(fp.isEmpty()||fp!=binding.fingerprint)return {{"state","error"},{"error","SOURCE_CHANGED"}};
    const auto key=QString::fromLatin1(QCryptographicHash::hash((fp+binding.provider->getDisplayName()+":3ebac449:stereo:64:butterworth2-v2").toUtf8(),QCryptographicHash::Sha256).toHex());
    std::lock_guard<std::mutex> lock(mutex_);
    if(!assets_.count(key)&&assets_.size()>=128){
        for(auto it=assets_.begin();it!=assets_.end();++it){bool pinned=it->first==activeKey_;for(const auto& job:jobs_)pinned|=job.key==it->first;for(const auto& [id,lease]:leases_)pinned|=lease.key==it->first;for(const auto& window:windows_)pinned|=window.job.key==it->first;
            if(!pinned){manifests_.erase(it->first);assets_.erase(it);break;}}
        if(assets_.size()>=128)return {{"error","BUSY"}};
    }
    assets_[key]={path,key,fp,generation,binding};
    if(!manifests_.count(key) || manifests_[key]["state"]=="evicted") {
        if(jobs_.size()>=8)return {{"error","BUSY"}};
        QFile cached(root_+"/"+key+"/manifest.json");
        if(cached.size()<=65536 && cached.open(QIODevice::ReadOnly)) {
            const auto candidate=QJsonDocument::fromJson(cached.readAll()).object();
            bool valid=candidate["schemaVersion"]==2 && candidate["assetKey"]==key && candidate["sourceFingerprint"]==fp && candidate["state"]=="ready" && candidate["frameCountFinal"].toBool() && candidate["sourceSampleRateHz"]==binding.rate && !candidate["levels"].toArray().isEmpty();
            for(const auto& value:candidate["levels"].toArray()) {
                const auto level=value.toObject();for(const auto& range:level["readyTileRanges"].toArray()) {
                    const auto bounds=range.toArray();if(bounds.size()!=2){valid=false;break;}
                    for(int i=bounds[0].toInt();valid && i<bounds[1].toInt();++i)valid=QFileInfo(root_+"/"+key+QString("/%1-%2-bands.bin").arg(level["lod"].toInt()).arg(i)).size()>=64;
                }
            }
            if(valid){manifests_[key]=candidate;return {{"assetKey",key},{"jobId",key},{"state","ready"},{"loadGeneration",double(generation)}};}
        }
        manifests_[key]={{"schemaVersion",2},{"assetKey",key},{"state","queued"},{"revision",0}};
        jobs_.push_back({path,key,fp,generation,binding});wake_.notify_one();
    }
    return {{"assetKey",key},{"jobId",key},{"state",manifests_[key]["state"]},{"loadGeneration",double(generation)}};
}
QJsonObject Manager::manifest(const QString& key){std::lock_guard<std::mutex> lock(mutex_);auto it=manifests_.find(key);return it==manifests_.end()?QJsonObject{{"error","STALE_ASSET"}}:it->second;}
QJsonObject Manager::lease(const QString& key,const QString& resource) {
    std::lock_guard<std::mutex> lock(mutex_);
    const double now=deckclock::monotonicUs();
    for(auto it=leases_.begin();it!=leases_.end();)if(it->second.until<=now)it=leases_.erase(it);else ++it;
    if(!manifests_.count(key)||resource.contains('/')||resource.contains("..")||leases_.size()>=256)return {{"error","STALE_ASSET"}};
    const auto id=QUuid::createUuid().toString(QUuid::WithoutBraces);leases_[id]={key,deckclock::monotonicUs()+30e6};return {{"leaseId",id},{"expiresAfterMs",30000}};
}
void Manager::release(const QString& id){std::lock_guard<std::mutex> lock(mutex_);leases_.erase(id);}
QJsonObject Manager::requestRange(const QJsonObject& p) {
    const auto key=p["assetKey"].toString(),request=p["requestId"].toString();
    const double start=p["startSourceFrame"].toDouble(-1),end=p["endSourceFrame"].toDouble(-1);
    if(request.isEmpty()||request.size()>128||!std::isfinite(start)||!std::isfinite(end)||start<0||end<=start||end-start>262144||end>9007199254740991.0||std::floor(start)!=start||std::floor(end)!=end)return {{"error","INVALID_RANGE"}};
    std::lock_guard<std::mutex> lock(mutex_);auto found=assets_.find(key);
    if(found==assets_.end())return {{"error","STALE_ASSET"}};
    const QString id=QString::number(qint64(start))+"-"+QString::number(qint64(end));
    if(p["detail"]!="pcm")return {{"state",manifests_[key]["state"]}};
    if(QFileInfo::exists(root_+"/"+key+"/pcm-"+id+".bin"))return {{"state","ready"},{"windowId",id}};
    if(windows_.size()>=12||requests_.size()>=64)return {{"error","BUSY"}};
    requests_[request]=key+":"+id;
    bool queued=false;for(const auto& window:windows_)queued|=window.job.key==key&&window.id==id;
    if(!queued)windows_.push_front({found->second,id,SINT(start),SINT(end)});
    wake_.notify_one();return {{"state","queued"},{"windowId",id}};
}
void Manager::cancelRequest(const QString& id) {
    std::lock_guard<std::mutex> lock(mutex_);requests_.erase(id);
    for(auto it=windows_.begin();it!=windows_.end();) {
        bool needed=false;for(const auto& [request,key]:requests_)needed|=key==it->job.key+":"+it->id;
        if(!needed)it=windows_.erase(it);else ++it;
    }
}
void Manager::pcm(const Window& window) {
    auto track=Track::newTemporary(window.job.path);SoundSourceProxy proxy(track,window.job.binding.provider);
    mixxx::AudioSource::OpenParams params;params.setChannelCount(mixxx::audio::ChannelCount(2));
    const auto source=proxy.openAudioSource(params);if(!source)return;
    struct Close{mixxx::AudioSourcePointer source;~Close(){source->close();}}close{source};
    const auto origin=source->frameIndexRange().start(),end=source->frameIndexRange().end();
    const unsigned count=window.end-window.start,rate=source->getSignalInfo().getSampleRate().value();
    if(rate!=unsigned(window.job.binding.rate)||source->getSignalInfo().getChannelCount()!=2)return;
    QByteArray bytes(64+count*8,'\0');auto* out=reinterpret_cast<unsigned char*>(bytes.data());
    std::memcpy(out,"DJWP",4);qToLittleEndian<quint16>(2,out+4);qToLittleEndian<quint16>(64,out+6);qToLittleEndian<quint16>(2,out+12);
    qToLittleEndian<quint32>(count,out+16);qToLittleEndian<quint32>(1,out+20);qToLittleEndian<quint64>(window.start,out+24);qToLittleEndian<quint64>(count,out+32);
    qToLittleEndian<quint32>(rate,out+40);qToLittleEndian<quint32>(count*8,out+44);qToLittleEndian<quint32>(1,out+52);
    mixxx::SampleBuffer buffer(16384*2);
    // The selected provider owns seek/preroll; accept only the exact requested frame range.
    for(SINT position=std::max(origin,window.start);position<std::min(window.end,end)&&!stop_;) {
        {std::lock_guard<std::mutex> lock(mutex_);bool needed=false;for(const auto& [id,key]:requests_)needed|=key==window.job.key+":"+window.id;if(!needed)return;}
        const auto next=std::min(position+SINT(16384),std::min(window.end,end));
        const auto read=source->readSampleFrames(mixxx::WritableSampleFrames(mixxx::IndexRange::between(position,next),mixxx::SampleBuffer::WritableSlice(buffer,0,(next-position)*2)));
        if(read.frameLength()<=0||read.frameIndexRange().start()!=position)return;
        for(SINT f=std::max(window.start,position);f<read.frameIndexRange().end();++f)for(int c=0;c<2;++c){float value=read.readableData()[(f-position)*2+c];quint32 bits;std::memcpy(&bits,&value,4);qToLittleEndian(bits,out+64+4*(c*count+f-window.start));}
        position=read.frameIndexRange().end();std::this_thread::yield();
    }
    if(stop_||fingerprint(window.job.path)!=window.job.fingerprint)return;
    qToLittleEndian<quint32>(crc32(out+64,count*8),out+48);
    QDir().mkpath(root_+"/"+window.job.key);
    // PCM is a small transient window cache, never whole-track storage.
    {std::lock_guard<std::mutex> lock(mutex_);const QString directory=root_+"/"+window.job.key;
     const auto files=QDir(directory).entryInfoList({"pcm-*.bin"},QDir::Files,QDir::Time);
     bool pinned=false;for(const auto& [id,lease]:leases_)pinned|=lease.key==window.job.key&&lease.until>deckclock::monotonicUs();
     if(files.size()>=3&&pinned)return;
     for(int i=2;i<files.size();++i)QFile::remove(files[i].filePath());
     qint64 pcmBytes=0;QFileInfoList all;
     for(const auto& dir:QDir(root_).entryInfoList(QDir::Dirs|QDir::NoDotAndDotDot))for(const auto& file:QDir(dir.filePath()).entryInfoList({"pcm-*.bin"},QDir::Files)){pcmBytes+=file.size();all.append(file);}
     std::sort(all.begin(),all.end(),[](const auto& a,const auto& b){return a.lastModified()<b.lastModified();});
     for(const auto& file:all){if(pcmBytes+bytes.size()<=32*1024*1024)break;bool inUse=false;for(const auto& [id,lease]:leases_)inUse|=lease.key==file.dir().dirName()&&lease.until>deckclock::monotonicUs();if(!inUse&&QFile::remove(file.filePath()))pcmBytes-=file.size();}
     if(pcmBytes+bytes.size()>32*1024*1024)return;
     save(directory+"/pcm-"+window.id+".bin",bytes);
     for(auto it=requests_.begin();it!=requests_.end();)if(it->second==window.job.key+":"+window.id)it=requests_.erase(it);else ++it;
    }
}
void Manager::invalidate(const QString& key) {
    std::lock_guard<std::mutex> lock(mutex_);auto asset=assets_.find(key);
    if(asset==assets_.end()||activeKey_==key||jobs_.size()>=8)return;
    for(const auto& job:jobs_)if(job.key==key)return;
    manifests_[key]["state"]="queued";jobs_.push_front(asset->second);wake_.notify_one();
}
void Manager::run(){
    while(!stop_){Job job;{std::unique_lock<std::mutex> lock(mutex_);wake_.wait(lock,[&]{return stop_||!jobs_.empty()||!windows_.empty();});if(stop_)return;if(!windows_.empty()){auto window=windows_.front();windows_.pop_front();lock.unlock();try{pcm(window);}catch(...){ }continue;}job=jobs_.front();jobs_.pop_front();activeKey_=job.key;}
        try{prune();analyze(job);}catch(const std::exception& e){std::lock_guard<std::mutex> lock(mutex_);manifests_[job.key]["state"]="error";manifests_[job.key]["error"]=QJsonObject{{"code",QString::fromUtf8(e.what())},{"retryable",true}};}
        {std::lock_guard<std::mutex> lock(mutex_);activeKey_.clear();}
    }
}
void Manager::analyze(const Job& job){
    auto track=Track::newTemporary(job.path);SoundSourceProxy proxy(track,job.binding.provider);
    mixxx::AudioSource::OpenParams params;params.setChannelCount(mixxx::audio::ChannelCount(2));
    const auto source=proxy.openAudioSource(params);if(!source)throw std::runtime_error("DECODE_FAILED");
    struct Close{mixxx::AudioSourcePointer source;~Close(){source->close();}} close{source};
    const unsigned rate=source->getSignalInfo().getSampleRate().value();
    if(rate!=unsigned(job.binding.rate)||source->getSignalInfo().getChannelCount()!=2||proxy.getProvider()!=job.binding.provider)throw std::runtime_error("SOURCE_CHANGED");
    const auto origin=source->frameIndexRange().start();
    QDir().mkpath(root_+"/"+job.key);
    QJsonObject manifest{{"schemaVersion",2},{"assetKey",job.key},{"sourceFingerprint",job.fingerprint},{"analysisVersion","stereo-64-v2"},
      {"decoderProvider",job.binding.provider->getDisplayName()},{"decoderVersion","3ebac449"},{"sourceSampleRateHz",int(rate)},
      {"channelCount",2},{"sourceFrameOrigin",double(origin)},{"sourceFrameCount",0},{"frameCountFinal",false},{"baseFramesPerBin",64},{"tileBins",2048},
      {"filterVersion","butterworth2-causal-250-2500-v1"},{"bandsHz",QJsonArray{250,2500}},{"state","partial"},{"revision",1},
      {"normalization",QJsonObject{{"gain",1},{"final",true}}}};
    QJsonArray levels;
    const auto publishManifest=[&]{manifest["levels"]=levels;save(root_+"/"+job.key+"/manifest.json",QJsonDocument(manifest).toJson(QJsonDocument::Compact));std::lock_guard<std::mutex> lock(mutex_);manifests_[job.key]=manifest;};
    Pyramid pyramid(rate,[&](unsigned lod,std::uint64_t index,const std::vector<Bin>& bins){
        const auto bytes=encode(lod,index,bins,rate,rate>5000,origin);
        const auto full=encode(lod,index,bins,rate,false,origin);
        const auto path=root_+"/"+job.key+QString("/%1-%2-bands.bin").arg(lod).arg(index);
        const QString fullPath=root_+"/"+job.key+QString("/%1-%2-full.bin").arg(lod).arg(index);
        const auto oldSize=QFileInfo(path).size()+QFileInfo(fullPath).size();
        if(diskBytes_+bytes.size()+full.size()-oldSize>1024ull*1024*1024-65536)prune();
        if(diskBytes_+bytes.size()+full.size()-oldSize>1024ull*1024*1024-65536)throw std::runtime_error("CACHE_QUOTA");
        save(path,bytes);save(fullPath,full);diskBytes_+=bytes.size()+full.size()-oldSize;
        while(levels.size()<=int(lod))levels.append(QJsonObject{});
        // QJsonArray{QJsonArray{...}} selects QJsonArray's copy constructor on
        // some Qt/MSVC combinations and silently produces [0, end], not the
        // protocol's required [[0, end]]. Wrap the inner array as a JSON value
        // so the outer array always contains one range.
        const QJsonArray readyRanges{QJsonValue(QJsonArray{0,double(index+1)})};
        levels[int(lod)]=QJsonObject{{"lod",int(lod)},{"framesPerBin",double(std::uint64_t(64)<<lod)},
          {"readyTileRanges",readyRanges},{"bandReadyTileRanges",rate>5000?readyRanges:QJsonArray{}}};
    });
    mixxx::SampleBuffer buffer(16384*2);SINT position=origin;double lastPublish=0;unsigned late=deckclock::lateCallbacks.load();
    while(!stop_ && position<source->frameIndexRange().end()){
        const auto observedLate=deckclock::lateCallbacks.load(std::memory_order_relaxed);if(observedLate!=late){late=observedLate;std::this_thread::sleep_for(std::chrono::milliseconds(5));}
        const auto end=std::min(position+SINT(16384),source->frameIndexRange().end());
        const auto read=source->readSampleFrames(mixxx::WritableSampleFrames(mixxx::IndexRange::between(position,end),mixxx::SampleBuffer::WritableSlice(buffer,0,(end-position)*2)));
        if(read.frameLength()<=0||read.frameIndexRange().start()!=position)throw std::runtime_error("DECODE_FAILED");
        const auto* pcm=read.readableData();for(SINT f=0;f<read.frameLength();++f)pyramid.sample(pcm[f*2],pcm[f*2+1]);
        position=read.frameIndexRange().end();manifest["sourceFrameCount"]=double(position-origin);
        const auto now=deckclock::monotonicUs();if(now-lastPublish>200000){manifest["revision"]=manifest["revision"].toInt()+1;publishManifest();lastPublish=now;}
        // Interactive PCM requests are serviced between bounded decode chunks.
        std::optional<Window> window;{std::lock_guard<std::mutex> lock(mutex_);if(!windows_.empty()){window=windows_.front();windows_.pop_front();}}
        if(window){try{this->pcm(*window);}catch(...){ }}
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    if(stop_)return;
    if(fingerprint(job.path)!=job.fingerprint)throw std::runtime_error("SOURCE_CHANGED");
    pyramid.finish();manifest["frameCountFinal"]=true;manifest["state"]="ready";manifest["revision"]=manifest["revision"].toInt()+1;publishManifest();
}
void Manager::prune(){
    std::lock_guard<std::mutex> lock(mutex_);const double now=deckclock::monotonicUs();
    for(auto it=leases_.begin();it!=leases_.end();)if(it->second.until<now)it=leases_.erase(it);else ++it;
    const auto dirs=QDir(root_).entryInfoList(QDir::Dirs|QDir::NoDotAndDotDot,QDir::Time|QDir::Reversed);
    qint64 total=0;for(const auto& dir:dirs)for(const auto& file:QDir(dir.filePath()).entryInfoList(QDir::Files))total+=file.size();
    for(const auto& dir:dirs){if(total<768ll*1024*1024)break;bool pinned=dir.fileName()==activeKey_;for(const auto& job:jobs_)pinned|=job.key==dir.fileName();for(const auto& [id,lease]:leases_)pinned|=lease.key==dir.fileName();if(pinned)continue;
        qint64 removed=0;for(const auto& file:QDir(dir.filePath()).entryInfoList(QDir::Files))removed+=file.size();if(QDir(dir.filePath()).removeRecursively()){total-=removed;if(manifests_.count(dir.fileName()))manifests_[dir.fileName()]["state"]="evicted";}}
    diskBytes_=total;
}
}
