#pragma once
#include "sources/soundsourceproxy.h"
#include "track/track.h"
#include <QFileInfo>
#include <QFile>
#include <QDateTime>
#include <QCryptographicHash>
#include <sys/stat.h>
#include <map>
#include <mutex>
namespace waveform {
inline QString sourceFingerprint(const QString& path) {
    QFileInfo file(path);
    if (!file.isFile()) return {};
#ifdef Q_OS_WIN
    // QFileInfo uses the wide Win32 API, including non-ASCII paths.
    const auto nanos = (file.lastModified().toMSecsSinceEpoch() % 1000) * 1000000;
#else
    struct stat info{};if(::stat(QFile::encodeName(path).constData(),&info)!=0)return {};
#ifdef __APPLE__
    const auto nanos=info.st_mtimespec.tv_nsec;
#else
    const auto nanos=info.st_mtim.tv_nsec;
#endif
#endif
    return QString::fromLatin1(QCryptographicHash::hash((file.canonicalFilePath()+":"+QString::number(file.size())+":"+QString::number(file.lastModified().toMSecsSinceEpoch())+":"+QString::number(nanos)).toUtf8(),QCryptographicHash::Sha256).toHex());
}
struct SourceBinding { mixxx::SoundSourceProviderPointer provider; int rate=0;QString fingerprint; };
inline std::mutex bindingsMutex;
inline std::map<QString,SourceBinding> bindings;
// Called by CachingReaderWorker after opening, never by the audio callback.
inline void bindSource(const QString& path, const SoundSourceProxy& proxy, const mixxx::AudioSourcePointer& source) {
    if(!source)return;
    std::lock_guard<std::mutex> lock(bindingsMutex);
    if(bindings.size()>=32)bindings.erase(bindings.begin());
    bindings[path]={proxy.getProvider(),int(source->getSignalInfo().getSampleRate().value()),sourceFingerprint(path)};
}
inline SourceBinding sourceBinding(const QString& path) {
    std::lock_guard<std::mutex> lock(bindingsMutex);const auto it=bindings.find(path);return it==bindings.end()?SourceBinding{}:it->second;
}
/// Junction monitor assets have never been loaded into a Mixxx deck. Probe the
/// decoder once so waveform analysis can use the same provider selection as a
/// normal track without mutating any deck or audio route.
inline SourceBinding ensureSourceBinding(const QString& path) {
    auto binding=sourceBinding(path);if(binding.provider)return binding;
    auto track=Track::newTemporary(path);SoundSourceProxy proxy(track);
    mixxx::AudioSource::OpenParams params;params.setChannelCount(mixxx::audio::ChannelCount(2));
    const auto source=proxy.openAudioSource(params);if(!source)return {};
    bindSource(path,proxy,source);source->close();return sourceBinding(path);
}
}
