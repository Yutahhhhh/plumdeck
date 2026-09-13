#include "waveform/manager.h"
// plumdeck-owned adapter. Upstream APIs pinned to Mixxx 2.5.6 / 3ebac449.
#include "backend.h"
#include "scratch_deck.h"
#include "sampler_bank.h"
#include "beat_fx.h"
#include "junction/ddj_checkpoint.h"
#include "junction/keylock_checkpoint.h"
#include "junction/fx_checkpoint.h"
#include "engine/effects/engineeffectsmanager.h"
#include <future>
#include "junction/runtime.h"
#include "junction/audio_bridge.h"
#include "junction/private_preview.h"
#include "junction/replay_driver.h"
#include <QCoreApplication>
#include <QFileInfo>
#include <QJsonArray>
#include <QTemporaryDir>
#include <QTimer>
#include <QSaveFile>
#include <QJsonDocument>
#include <QDateTime>
#include <QDir>
#include <QElapsedTimer>
#include <QStandardPaths>
#include <array>
#include <portaudio.h>
#include "control/control.h"
#include "control/controlindicatortimer.h"
#include "control/controlobject.h"
#include "effects/effectsmanager.h"
#include "effects/chains/quickeffectchain.h"
#include "effects/chains/standardeffectchain.h"
#include "effects/effectslot.h"
#include "engine/channels/enginedeck.h"
#include "engine/channels/enginemicrophone.h"
#include "engine/enginebuffer.h"
#include "engine/enginemixer.h"
#include "soundio/soundmanager.h"
#include "sources/soundsourceproxy.h"
#include "encoder/encoder.h"
#include "recording/defs_recording.h"
#include "recording/recordingmanager.h"
#include "track/track.h"
#include "track/beats.h"
#include "util/cmdlineargs.h"

namespace {
QString deviceId(PaDeviceIndex index) {
#ifdef Q_OS_MACOS
    return QStringLiteral("coreaudio:%1").arg(index);
#else
    return QStringLiteral("portaudio:%1").arg(index);
#endif
}
QString deviceLabel(PaDeviceIndex index, const QString& fallback) {
#ifdef Q_OS_WIN
    const auto* device = Pa_GetDeviceInfo(index);
    const auto* api = device ? Pa_GetHostApiInfo(device->hostApi) : nullptr;
    if (api) return fallback + QStringLiteral(" [%1]").arg(QString::fromUtf8(api->name));
#endif
    return fallback;
}
PaDeviceIndex defaultOutputDevice() {
#ifdef Q_OS_WIN
    const auto apiIndex = Pa_HostApiTypeIdToHostApiIndex(paWASAPI);
    const auto* api = apiIndex >= 0 ? Pa_GetHostApiInfo(apiIndex) : nullptr;
    if (api && api->defaultOutputDevice != paNoDevice) return api->defaultOutputDevice;
#endif
    return Pa_GetDefaultOutputDevice();
}
const QString groups[] = {QStringLiteral("[Channel1]"), QStringLiteral("[Channel2]"), QStringLiteral("[Channel3]"), QStringLiteral("[Channel4]")};
const QString names[] = {"A", "B", "C", "D"};
class MixxxBackend final : public QObject, public PlaybackBackend {
public:
    MixxxBackend() = default;
    void attachJunction(junction::Runtime* runtime) override { junctionRuntime_.store(runtime,std::memory_order_release); }
    QString privatePreviewCommand(const QString& op,const QJsonObject& params) override {
        if(!available_||!pflAvailable_)return "Private preview requires an available headphone output";
        return preview_.command(op,params);
    }
    QJsonObject privatePreviewState() const override { auto state=preview_.state();state["available"]=available_&&pflAvailable_;return state; }
    bool validateDspAsset(const QString& path) const override {return junction::ddj::read(path,nullptr)||junction::keylock::read(path,nullptr)||junction::fx::read(path,nullptr);}
    bool validateAudioAsset(const QString& path) const override {
        const auto binding=waveform::ensureSourceBinding(path);
        return binding.provider&&binding.fingerprint==waveform::sourceFingerprint(path);
    }
    QJsonObject junctionGraph() const override {
        auto* self=const_cast<MixxxBackend*>(this);
        if(!self->capturedGraph_.isEmpty()){const auto graph=self->capturedGraph_;self->capturedGraph_={};return graph;}
        if(!self->snapshotPending_&&!self->snapshotWrite_.valid()&&!self->restoring_&&available_)self->snapshotPending_=true;
        return {};
    }
    QJsonObject snapshotStoppedGraph() const {

        QJsonArray items;
        if(!available_)return {};
        for(int i=0;i<4;++i){QJsonObject controls;
            const auto presentation=junctionTrackPresentation?junctionTrackPresentation(i):QJsonObject{};
            for(const auto* key:graphControls())controls[key]=ControlObject::get(ConfigKey(groups[i],key));
            items.append(QJsonObject{{"index",i},{"path",tracks_[i]?tracks_[i]->getLocation():QString()},{"title",tracks_[i]?presentation["title"].toString(tracks_[i]->getTitle().isEmpty()?QFileInfo(tracks_[i]->getLocation()).completeBaseName():tracks_[i]->getTitle()):QString()},{"artist",tracks_[i]?presentation["artist"].toString(tracks_[i]->getArtist()):QString()},
                {"positionFrames",tracks_[i]?decks_[i]->junctionPositionFrames():0},
                {"sourceSampleRateHz",tracks_[i]?int(tracks_[i]->getSampleRate().value()):0},
                {"play",playing(i)},{"scratching",scratching(i)},{"tempo",playbackRate(i)},{"speed",decks_[i]->junctionSpeed()},
                {"controls",controls},{"performance",performanceState(i)},{"beatgrid",beatgridState(i)}});
        }
        bool historyRequired=micEnabled_||micDuckingEnabled_||(beatFx_&&beatFx_->state()["enabled"].toBool()&&!capturedDspReady_);
        for(int i=0;i<4;++i){const auto group=StandardEffectChain::formatEffectChainGroup(i);historyRequired|=ControlObject::get(ConfigKey(group,"enabled"))>0||std::abs(colorAmounts_[i])>.005||scratching(i)||ControlObject::get(ConfigKey(groups[i],"keylock"))>0||ControlObject::get(ConfigKey(groups[i],"slip_enabled"))>0||ControlObject::get(ConfigKey(groups[i],"reverseroll"))>0;}
        QJsonObject result{{"schema",1},{"decks",items},{"mixer",mixer()},{"sampler",samplers_->junctionState()},
            {"engineFingerprint",QStringLiteral("mixxx-3ebac449-junction-graph3")},{"microphoneClosed",!micEnabled_},{"microphoneTailSettled",!micEnabled_&&!micDuckingEnabled_},
            {"dspStateComplete",!historyRequired},{"requiresHistoricalDsp",historyRequired},{"dspStateStrategy","semantic-plus-exclusive-replay"},{"renderFrame",QString::number(renderDriver_.clock().renderFrame())}};
        if(auto* runtime=junctionRuntime_.load(std::memory_order_acquire))result["throughSeq"]=QString::number(runtime->currentAppliedSequence());
        return result;
    }
    QString restoreJunctionGraph(const QJsonObject& graph) override {
        if(!available_)return "Mixxx audio graph is unavailable";
        if(!graph["snapshotError"].toString().isEmpty())return graph["snapshotError"].toString();
        const auto items=graph["decks"].toArray();
        if(graph["schema"].toInt()!=1||items.size()!=4)return "Invalid Junction graph schema";
        // Paths have already been resolved from verified asset IDs by Runtime.
        // Validate every deck before changing any of the shared graph.
        for(int i=0;i<4;++i){const auto row=items[i].toObject();const auto path=row["path"].toString();
            const auto pos=row["positionFrames"].toDouble(-1);
            if(row["index"].toInt(-1)!=i||!std::isfinite(pos)||(path.isEmpty()?pos!=0:pos < -60.0*row["sourceSampleRateHz"].toDouble())||(!path.isEmpty()&&(!QFileInfo(path).isAbsolute()||!QFileInfo(path).isFile())))return "Invalid resolved Junction deck";
            const auto grid=row["beatgrid"].toObject();double previous=-1;
            if(grid["markers"].toArray().size()>100000)return "Junction beatgrid is too large";
            for(const auto v:grid["markers"].toArray()){const auto marker=v.toObject();const auto frame=marker["positionFrames"].toDouble(-1);const auto count=marker["beatsTillNext"].toDouble(-1);if(!std::isfinite(frame)||frame<0||frame<=previous||frame!=std::floor(frame)||count<1||count>100000||count!=std::floor(count))return "Invalid Junction beat marker";previous=frame;}
            if((!grid["markers"].toArray().isEmpty()||grid["lastMarkerBpm"].toDouble()>0)&&(!std::isfinite(grid["lastMarkerBpm"].toDouble())||grid["lastMarkerBpm"].toDouble()<=0||grid["lastMarkerBpm"].toDouble()>1000||grid["lastMarkerMs"].toDouble(-1)<0))return "Invalid Junction beatgrid tempo";
            const auto controls=row["controls"].toObject();for(const auto* key:graphControls())if(controls.contains(key)&&(!controls[key].isDouble()||!std::isfinite(controls[key].toDouble())))return "Invalid Junction deck control";
        }
        if(!graph["microphoneClosed"].toBool())return "Close the performing microphone before preparing a handoff";
        if(micEnabled_)return "Close your local microphone before replacing the shared graph";
        std::vector<junction::ddj::Snapshot> dspStates;
        junction::keylock::Snapshot keylockStates;
        junction::fx::Snapshot fxStates;
        const auto dspAssets=graph["dspStateAssets"].toArray();if(dspAssets.size()>3)return "Too many Junction DSP checkpoints";
        for(const auto value:dspAssets){const auto asset=value.toObject();
            if(asset["format"].toString()==junction::fx::format){if(!fxStates.empty()||asset["fingerprint"].toString()!=junction::fx::fingerprint||!junction::fx::read(asset["path"].toString(),&fxStates))return "Invalid verified FX checkpoint";continue;}
            if(asset["format"].toString()==junction::keylock::format){if(!keylockStates.empty()||asset["fingerprint"].toString()!=junction::keylock::fingerprint||!junction::keylock::read(asset["path"].toString(),&keylockStates))return "Invalid verified keylock checkpoint";continue;}
            junction::ddj::Snapshot snapshot;if(asset["format"].toString()!="plumdeck-ddj-dsp-v1"||asset["fingerprint"].toString()!=QString::fromLatin1(junction::ddj::fingerprint)||!junction::ddj::read(asset["path"].toString(),&snapshot)||snapshot.processor!=asset["processor"].toString())return "Invalid verified Junction DSP checkpoint";dspStates.push_back(std::move(snapshot));}
        pendingDsp_=std::move(dspStates);pendingKeylock_=std::move(keylockStates);pendingFx_=std::move(fxStates);
        for(int i=0;i<4;++i){restoreDecks_[i]={};restoreAfter_[i]=0;play(i,false);}
        const auto samplerError=samplers_->restoreJunction(graph["sampler"].toObject());if(!samplerError.isEmpty())return samplerError;
        originalGraph_=graph;aligning_=false;
        pendingMixer_=graph["mixer"].toObject();restoring_=true;restoreError_.clear();restoreTransfer_=0;
        for(int i=0;i<4;++i){const auto row=items[i].toObject();
            const auto generation=generations_[i]+1;
            if(graphDeckRestoring)graphDeckRestoring(i,generation,row);
            if(row["path"].toString().isEmpty()){unload(i);continue;}
            restoreDecks_[i]=row;load(i,row["path"].toString(),generation);
        }
        return {};
    }
    QString alignJunctionGraph(quint64 mediaFrame) override {
        if(!junctionGraphReady()||originalGraph_.isEmpty())return "Wait for Junction graph decoding and restoration";
        bool valid=false;const auto origin=originalGraph_["atMediaFrame"].toString().toULongLong(&valid);
        if(!valid||mediaFrame<origin)return "Invalid Junction graph media anchor";
        const auto items=originalGraph_["decks"].toArray();
        for(const auto value:items){const auto row=value.toObject();const auto c=row["controls"].toObject();if(c["slip_enabled"].toDouble()>0||c["reverseroll"].toDouble()>0||row["scratching"].toBool())return "Slip and scratch require historical graph replay";}
        alignTarget_=mediaFrame;aligning_=true;restoring_=true;restoreTransfer_=0;restoreError_.clear();
        return {};
    }
    bool junctionGraphReady() const override {
        if(!available_||restoring_||!restoreError_.isEmpty()||!samplers_->junctionReady())return false;
        for(int i=0;i<4;++i)if(!restoreDecks_[i].isEmpty()||!deckErrors_[i].isEmpty()||(tracks_[i]&&!deckReady_[i])||decks_[i]->junctionAudioBlocks()<restoreAfter_[i])return false;
        return true;
    }
    QString implementation() const override { return "mixxx"; }
    void start() override {
        if (started_) return;
        started_ = true;
        QTimer::singleShot(0, this, [this] { initialize(); });
    }
    void initialize() {
        if (!profile_.isValid()) { problem_ = "Cannot create isolated Mixxx profile"; return; }
        // Everything Mixxx writes stays in this disposable profile. No library DB.
        CmdlineArgs::Instance().setSettingsPath(profile_.path());
        settings_ = UserSettingsPointer(new UserSettings(profile_.filePath("mixxx.cfg")));
        ControlDoublePrivate::setUserConfig(settings_);
        indicator_ = std::make_unique<mixxx::ControlIndicatorTimer>();
        handles_ = std::make_shared<ChannelHandleFactory>();
        deckCount_ = std::make_unique<ControlObject>(ConfigKey("[App]", "num_decks"));
        samplerCount_ = std::make_unique<ControlObject>(ConfigKey("[App]", "num_samplers"));
        previewCount_ = std::make_unique<ControlObject>(ConfigKey("[App]", "num_preview_decks"));
        effects_ = std::make_unique<EffectsManager>(settings_, handles_);
        // DlgPrefMixer normally initializes the frequency shelves. Without it
        // both potmeters start at their midpoint and MID has no useful band.
        ControlObject::set(ConfigKey(kMixerProfile, kLowEqFrequency), 250);
        ControlObject::set(ConfigKey(kMixerProfile, kHighEqFrequency), 2500);
        mixer_ = std::make_unique<EngineMixer>(settings_, "[Master]", effects_.get(), handles_, true);
        const QString defaultRecordingDir = QDir(QStandardPaths::writableLocation(QStandardPaths::MusicLocation)).filePath("plumdeck Recordings");
        const QString recordingDir = qEnvironmentVariable("PLUMDECK_MIXXX_RECORDING_DIR", defaultRecordingDir);
        settings_->set(ConfigKey(RECORDING_PREF_KEY, "Directory"), recordingDir);
        settings_->set(ConfigKey(RECORDING_PREF_KEY, "Encoding"), QStringLiteral("WAV"));
        recorder_ = std::make_unique<RecordingManager>(settings_, mixer_.get());
        QObject::connect(recorder_.get(), &RecordingManager::isRecording, this, [this](bool active) {
            recordingActive_ = active;
            if (active) {
                recordingPending_ = false;
                recordingPath_ = recorder_->getRecordingLocation();
                recordingStartedAt_ = QDateTime::currentDateTimeUtc().toString(Qt::ISODateWithMs);
                recordingTimer_.restart();
                recordingError_.clear();
                // A stop may arrive while the encoder is still opening. Wait
                // for this acknowledgment before requesting OFF: upstream emits
                // no stop acknowledgment when there was never an open file.
                if (recordingStopping_) recorder_->stopRecording();
            } else {
                if (recordingPending_) recordingError_ = "Mixxx could not start the recording";
                recordingPending_ = false;
                if (!recordingStopping_ && recordingTimer_.isValid()) recordedElapsedMs_ = recordingTimer_.elapsed();
                // EngineRecord emits false only after closeFile() has flushed
                // the encoder and closed QFile. This is the preview-ready edge.
                recordingStopping_ = false;
                recordingCooldown_.restart();
            }
            if (recordingChanged) recordingChanged(recording());
        });
        for (int index = 0; index < 4; ++index) {
            const auto orientation = (index == 0 || index == 2) ? EngineChannel::LEFT : EngineChannel::RIGHT;
            const auto handle = mixer_->registerChannelGroup(groups[index]);
            decks_[index] = new ScratchDeck(handle, settings_, mixer_.get(), effects_.get(), orientation);
            mixer_->addChannel(decks_[index]); // mixer owns/deletes the decks.
            effects_->addDeck(handle);
            ControlObject::set(ConfigKey(groups[index], "main_mix"), 1);
            ControlObject::set(ConfigKey(groups[index], "volume"), 1);
            // Placement quantization is host-owned. Native quantize also seeks
            // and bends transport through CueControl, SyncControl and BpmControl.
            ControlObject::set(ConfigKey(groups[index], "quantize"), 0);
        }
        deckCount_->set(4);
        samplers_ = std::make_unique<SamplerBank>(settings_, mixer_.get(), effects_.get());
        samplerCount_->set(64);
        microphone_ = new EngineMicrophone(mixer_->registerChannelGroup("[Microphone1]"), effects_.get());
        mixer_->addChannel(microphone_); // EngineMixer owns the microphone too.
        ControlObject::set(ConfigKey("[Microphone1]", "pregain"), 1);
        ControlObject::set(ConfigKey("[Microphone1]", "talkover"), 0);
        ControlObject::set(ConfigKey("[Master]", "talkover_mix"), 0); // Main + record mix.
        ControlObject::set(ConfigKey("[Master]", "talkoverDucking"), 0);
        ControlObject::set(ConfigKey("[Master]", "duckStrength"), 1 - micDuckingStrength_);
        // PlayerManager normally creates these chains and MixxxMainWindow loads
        // their processors. The headless host must perform both steps itself.
        effects_->setup();
        beatFx_ = std::make_unique<BeatFx>(effects_.get());
        for (int index = 0; index < 4; ++index) {
            const auto group = StandardEffectChain::formatEffectChainGroup(index);
            effects_->getStandardEffectChain(index)->loadEmptyNamelessPreset();
            for (int channel = 0; channel < 4; ++channel)
                ControlObject::set(ConfigKey(group, QStringLiteral("group_%1_enable").arg(groups[channel])), index == channel ? 1 : 0);
            // エンジン起動直後の最初の 1 回は、どのエフェクトでも音に出ない。
            // ここでロードから有効化まで一通り通しておき、実際に押されるまでに
            // 落ち着かせる。チェーン自体は切ったままなので音は変わらない。
            fx(index, "echo", false, 0.25, 0.5);
            ControlObject::set(ConfigKey(group, QStringLiteral("enabled")), 0);
        }
        ControlObject::set(ConfigKey("[Master]", "gain"), 0.5);
        if (!SoundSourceProxy::registerProviders()) { problem_ = "Mixxx decoder provider registration failed"; return; }
        for (int index = 0; index < 4; ++index) {
            auto* buffer = decks_[index]->getEngineBuffer();
            QObject::connect(buffer, &EngineBuffer::trackLoaded, this, [this, index](TrackPointer track, TrackPointer) {
                if (!track || track != tracks_[index]) return; // nullptr is eject.
                deckReady_[index]=true;deckErrors_[index].clear();
                tryFinalizeGraphRestore();
                decks_[index]->setLoadGeneration(generations_[index]);
                releaseScratch(index); // Publish the decoded source rate before accepting gestures.
                if (loaded) loaded(index, generations_[index], {{"junctionRestore",!restoreDecks_[index].isEmpty()},{"durationMs", track->getDuration() * 1000.0}, {"sampleRateHz", static_cast<int>(track->getSampleRate().value())}, {"channels", track->getChannels()}}, {});
            }, Qt::QueuedConnection);
            QObject::connect(buffer, &EngineBuffer::trackLoadFailed, this, [this, index](TrackPointer track, const QString& reason) {
                if (track == tracks_[index]) { deckErrors_[index]=reason;deckReady_[index]=false;if(loaded)loaded(index, generations_[index], {}, reason); }
            }, Qt::QueuedConnection);
        }
        sound_ = std::make_unique<SoundManager>(settings_, mixer_.get());
        sound_->setConfiguredDeckCount(4);
        // Destination matching uses type/index, independent of physical channel
        // count. SoundManager expands the selected mono input into stereo.
        sound_->registerInput(AudioInput(AudioPathType::Microphone, 0, mixxx::audio::ChannelCount::stereo(), 0), microphone_);
        const AudioOutput main(AudioPathType::Main, 0, mixxx::audio::ChannelCount::stereo());
        sound_->registerOutput(main, mixer_.get());
        sound_->registerOutput(AudioOutput(AudioPathType::Headphones, 0, mixxx::audio::ChannelCount::stereo()), mixer_.get());
        // SoundManager unconditionally configures its internal Network Device
        // RecordBroadcast output, even when broadcasting/sidechain is disabled.
        // The source must exist; no network worker or listener is started.
        sound_->registerOutput(AudioOutput(AudioPathType::RecordBroadcast, 0, mixxx::audio::ChannelCount::stereo()), mixer_.get());
        auto config = sound_->getConfig();
        config.clearInputs(); config.clearOutputs(); config.setDeckCount(4);
        // Explicit device name keeps smoke tests on the intended virtual/built-in
        // device. Never select the DDJ-1000 or an arbitrary output implicitly.
        const auto wanted = qEnvironmentVariable("PLUMDECK_MIXXX_OUTPUT_DEVICE");
        QList<SoundDevicePointer> devices;
        for (const auto& api : sound_->getHostAPIList()) devices.append(sound_->getDeviceList(api, true, false));
        SoundDevicePointer selected;
        for (const auto& device : devices) {
            qInfo() << "plumdeck output device:" << device->getDisplayName();
            if (!wanted.isEmpty() && (deviceLabel(device->getDeviceId().portAudioIndex, device->getDisplayName()) == wanted || deviceId(device->getDeviceId().portAudioIndex) == wanted)) {
                if (selected) { problem_ = "Ambiguous output display name"; return; }
                selected = device;
            }
        }
        if (wanted.isEmpty()) {
            // Enumeration order is unrelated to the macOS default output.
            const auto defaultOutput = defaultOutputDevice();
            for (const auto& device : devices) {
                if (device->getDeviceId().portAudioIndex == defaultOutput &&
                        device->getNumOutputChannels() >= mixxx::audio::ChannelCount::stereo()) {
                    selected = device;
                    break;
                }
            }
        }
        if (!selected) { problem_ = wanted.isEmpty() ? QStringLiteral("The system default output is unavailable or has fewer than two channels. Choose an output in Audio Settings.") : QStringLiteral("Requested audio output not found: ") + wanted; return; }
        if (selected->getNumOutputChannels() < mixxx::audio::ChannelCount::stereo()) { problem_ = "Choose an output with at least two channels"; return; }
        config.setAPI(selected->getHostAPI());
        config.setSampleRate(mixxx::audio::SampleRate(44100));
        // 256 / 44.1k = 5.8ms. A pointer release waits at most one callback
        // then traverses two rate ramps; 512 frames exceeded the 30ms budget.
        config.setAudioBufferSizeIndex(3);
        config.addOutput(selected->getDeviceId(), main);
        // DDJ-1000's USB 1/2 is master, 3/4 is the cue bus. The hardware
        // performs headphone level and cue/master blending, so send pure PFL.
        pflAvailable_ = selected->getDisplayName() == "DDJ-1000" && selected->getNumOutputChannels().value() >= 4;
        if (pflAvailable_) {
            config.addOutput(selected->getDeviceId(), AudioOutput(AudioPathType::Headphones, 2, mixxx::audio::ChannelCount::stereo()));
            ControlObject::set(ConfigKey("[Master]", "headGain"), 1);
            ControlObject::set(ConfigKey("[Master]", "headMix"), -1);
        }
        renderDriver_.requestTransfer(junction::GraphDriver::Realtime,junction::RenderMode::Performing);
        audioBridge_.context=this;
        audioBridge_.before=[](void* context,unsigned) -> bool {auto* self=static_cast<MixxxBackend*>(context);self->audioCaptureSequence_.fetch_add(1,std::memory_order_acq_rel);self->blockGrant_=self->renderDriver_.beginBlock(junction::GraphDriver::Realtime);return self->blockGrant_.mayProcess;};
        audioBridge_.after=[](void* context,float* master,float* pfl,unsigned frames){
            auto* self=static_cast<MixxxBackend*>(context);
            const auto frame=self->renderDriver_.clock().renderFrame();
            auto* runtime=self->junctionRuntime_.load(std::memory_order_acquire);
            if(self->blockGrant_.mayProcess){
                if(runtime&&master)runtime->capture(master,frames,frame,44100);
                // SoundManager caches these device-sink addresses at open.
                // Finish all reads from graph PCM before releasing ownership.
                if(master)std::copy_n(master,frames*2,self->audioBridge_.idleMaster.data());
                if(pfl)std::copy_n(pfl,frames*2,self->audioBridge_.idlePfl.data());
                self->renderDriver_.clock().advance(frames);
            }
            master=self->audioBridge_.idleMaster.data();pfl=self->audioBridge_.idlePfl.data();
            self->audioCaptureSequence_.fetch_add(1,std::memory_order_release);
            if(!self->blockGrant_.mayProcess || (runtime&&!runtime->sharedAudible())){
                if(master)std::fill_n(master,frames*2,0.f);
                if(pfl)std::fill_n(pfl,frames*2,0.f);
            }
            if(runtime&&!runtime->localMasterAudible())std::fill_n(master,frames*2,0.f);
            self->preview_.mixPfl(pfl,frames);
            if(self->blockGrant_.mayProcess&&self->blockGrant_.releaseRequested){self->audioBridge_.inputsEnabled.store(false,std::memory_order_release);self->renderDriver_.acknowledgeRelease(junction::GraphDriver::Realtime,frame+frames,self->blockGrant_.generation);}
        };
        junction::audioBridge.store(&audioBridge_,std::memory_order_release);
        auto status = sound_->setConfig(config);
        if (status != SoundDeviceStatus::Ok) { problem_ = sound_->getLastErrorMessage(status); return; }
        outputDevice_ = selected;
        pflStart_ = pflAvailable_ ? 2 : -1;
        output_ = deviceLabel(selected->getDeviceId().portAudioIndex, selected->getDisplayName()); problem_.clear(); available_ = true;
        auto* scratchDiagnostics = new QTimer(this);
        connect(scratchDiagnostics, &QTimer::timeout, this, [this] { saveScratchDiagnostics(); });
        scratchDiagnostics->start(250);
        auto* graphRestoreTimer=new QTimer(this);connect(graphRestoreTimer,&QTimer::timeout,this,[this]{tryCaptureGraph();tryFinalizeGraphRestore();});graphRestoreTimer->start(5);
    }
    ~MixxxBackend() override {
        for (int index = 0; index < 4; ++index) if (decks_[index]) releaseScratch(index);
        if (recorder_ && (recordingActive_ || recordingPending_)) recorder_->stopRecording();
        outputDevice_.reset();
        sound_.reset(); // Stops callbacks before any engine-owned memory is freed.
        junction::audioBridge.store(nullptr,std::memory_order_release);
        recorder_.reset(); samplers_.reset(); beatFx_.reset(); mixer_.reset(); decks_ = {}; effects_.reset();
    }
    bool available() const override { return available_; }
    QString problem() const override { return problem_; }
    QJsonObject audioDevices() const override {
        // PortAudio initialization is reference counted. Enumerate independently
        // so setup remains available when Mixxx initialization has failed.
        const auto status = Pa_Initialize();
        if (status != paNoError) return {{"devices", QJsonArray{}}, {"reason", QString::fromUtf8(Pa_GetErrorText(status))}};
        QJsonArray devices;
        const auto defaultOutput = defaultOutputDevice();
        const auto defaultInput = Pa_GetDefaultInputDevice();
        const auto count = Pa_GetDeviceCount();
        for (PaDeviceIndex index = 0; index < count; ++index) {
            const auto* device = Pa_GetDeviceInfo(index);
            const auto* api = device ? Pa_GetHostApiInfo(device->hostApi) : nullptr;
            if (!device || !api || (device->maxOutputChannels == 0 && device->maxInputChannels == 0)) continue;
            devices.append(QJsonObject{{"id", deviceId(index)},
                    {"name", QString::fromUtf8(device->name)}, {"displayName", deviceLabel(index, QString::fromUtf8(device->name))},
                    {"outputChannels", device->maxOutputChannels}, {"isDefault", index == defaultOutput},
                    {"inputChannels", device->maxInputChannels}, {"isDefaultInput", index == defaultInput},
                    {"defaultSampleRateHz", device->defaultSampleRate}});
        }
        const QString reason = count < 0 ? QString::fromUtf8(Pa_GetErrorText(count)) : QString();
        Pa_Terminate();
        return {{"devices", devices}, {"reason", reason}};
    }
    QJsonObject audio() const override {
        const bool micApplied = available_ && microphone_ && !micDevice_.isEmpty() && ControlObject::get(ConfigKey("[Microphone1]", "input_configured")) > 0;
        const QJsonObject mic{{"available", available_ && microphone_ != nullptr},
                {"deviceId", micDevice_.isEmpty() ? QJsonValue::Null : QJsonValue(micDevice_)},
                {"channel", micChannel_}, {"enabled", micEnabled_}, {"gain", micGain_},
                {"duckingEnabled", micDuckingEnabled_}, {"duckingStrength", micDuckingStrength_},
                {"applied", micApplied}, {"level", micApplied ? ControlObject::get(ConfigKey("[Microphone1]", "vu_meter")) : 0},
                {"reason", micProblem_}};
        return {{"deviceId", output_}, {"sampleRateHz", 44100}, {"bufferFrames", 256}, {"masterChannels", QJsonArray{masterStart_, masterStart_ + 1}}, {"pflChannels", available_ && pflAvailable_ ? QJsonValue(QJsonArray{pflStart_, pflStart_ + 1}) : QJsonValue::Null}, {"pflApplied", available_ && pflAvailable_}, {"applied", available_}, {"reason", problem_}, {"microphone", mic}};
    }
    QString configureOutputRouting(const QJsonObject& params) override {
        if (!available_ || !sound_ || !outputDevice_) return "Open an output device before configuring channels";
        const auto master = params["masterChannels"].toArray();
        const auto cue = params["pflChannels"].toArray();
        const int count = outputDevice_->getNumOutputChannels().value();
        const auto valid = [count](const QJsonArray& pair) {
            return pair.size() == 2 && pair[0].isDouble() && pair[1].isDouble() &&
                pair[0].toDouble() == pair[0].toInt() && pair[0].toInt() >= 0 &&
                pair[1].toDouble() == pair[0].toInt() + 1 && pair[1].toInt() < count;
        };
        if (!valid(master) || (!params["pflChannels"].isNull() && !valid(cue))) return "Select two consecutive channels available on this output";
        const int mainStart = master[0].toInt(), cueStart = cue.isEmpty() ? -1 : cue[0].toInt();
        if (cueStart >= 0 && std::abs(mainStart - cueStart) < 2) return "Master and headphone channels must not overlap";
        if (mainStart == masterStart_ && cueStart == pflStart_) return {};
        if (recordingActive_ || recordingPending_ || recordingStopping_) return "Stop recording before changing output channels";
        for (int i = 0; i < 4; ++i) if (playing(i)) return "Pause all decks before changing output channels";
        const auto previous = sound_->getConfig();
        auto config = previous;
        config.clearOutputs();
        config.addOutput(outputDevice_->getDeviceId(), AudioOutput(AudioPathType::Main, mainStart, mixxx::audio::ChannelCount::stereo()));
        if (cueStart >= 0) config.addOutput(outputDevice_->getDeviceId(), AudioOutput(AudioPathType::Headphones, cueStart, mixxx::audio::ChannelCount::stereo()));
        const auto status = sound_->setConfig(config);
        if (status != SoundDeviceStatus::Ok) {
            const auto failure = sound_->getLastErrorMessage(status);
            if (sound_->setConfig(previous) != SoundDeviceStatus::Ok) { available_ = false; problem_ = failure; }
            return failure;
        }
        masterStart_ = mainStart; pflStart_ = cueStart; pflAvailable_ = cueStart >= 0;
        if (pflAvailable_) {
            ControlObject::set(ConfigKey("[Master]", "headGain"), 1);
            ControlObject::set(ConfigKey("[Master]", "headMix"), -1);
        }
        return {};
    }
    QString configureMicrophone(const QJsonObject& params) override {
        if (!available_ || !sound_ || !microphone_) return "Microphone input is unavailable until the audio output is ready";
        QString deviceName = params.contains("deviceId") ? params["deviceId"].toString() : micDevice_;
        const int channel = params.contains("channel") ? params["channel"].toInt() : micChannel_;
        const bool enabled = deviceName.isEmpty() ? false : (params.contains("enabled") ? params["enabled"].toBool() : micEnabled_);
        if (deviceName.isEmpty() && params["enabled"].toBool()) return "Select a microphone input before enabling it";
        SoundDevicePointer selected;
        QString deviceKey;
        if (!deviceName.isEmpty()) {
            // Preserve the actual route for live controls, including devices
            // with identical display names. IDs identify this enumeration.
            const auto lookup = params.contains("deviceId") ? deviceName : micDeviceKey_;
            const auto devices = sound_->getDeviceList(outputDevice_->getHostAPI(), false, true);
            for (const auto& device : devices) {
                if (deviceLabel(device->getDeviceId().portAudioIndex, device->getDisplayName()) == lookup || deviceId(device->getDeviceId().portAudioIndex) == lookup) {
                    if (selected) return "Ambiguous microphone display name; select its device id";
                    selected = device;
                }
            }
            if (!selected) return "Requested microphone input not found: " + deviceName;
            if (channel >= selected->getNumInputChannels().value()) return "The selected microphone input channel does not exist";
            deviceKey = QStringLiteral("coreaudio:%1").arg(selected->getDeviceId().portAudioIndex);
            deviceName = selected->getDisplayName();
            int matchingNames = 0;
            for (const auto& device : devices) if (device->getDisplayName() == deviceName) ++matchingNames;
            if (matchingNames > 1) deviceName = deviceKey;
        }
        const bool routeChanged = deviceKey != micDeviceKey_ || (!deviceName.isEmpty() && channel != micChannel_);
        if (routeChanged) {
            if (recordingActive_ || recordingPending_ || recordingStopping_ || (recorder_ && recorder_->isRecordingActive())) return "Stop recording and wait for it to finish before changing the microphone input device or channel";
            const auto previous = sound_->getConfig();
            auto next = previous;
            next.clearInputs();
            if (selected) next.addInput(selected->getDeviceId(), AudioInput(AudioPathType::Microphone, static_cast<unsigned char>(channel), mixxx::audio::ChannelCount::mono(), 0));
            // Quiesce callbacks before SoundManager replaces its input buffers.
            // The existing engine/decks remain alive and keep their positions.
            deckclock::configChanged.store(true,std::memory_order_relaxed);
            const auto status = sound_->setConfig(next);
            if (status != SoundDeviceStatus::Ok) {
                const auto failure = sound_->getLastErrorMessage(status);
                const auto rollback = sound_->setConfig(previous);
                micProblem_ = "Could not open microphone input: " + failure;
                if (rollback != SoundDeviceStatus::Ok) {
                    available_ = false;
                    problem_ = "Audio routing recovery failed: " + sound_->getLastErrorMessage(rollback);
                    micProblem_ += ". " + problem_;
                }
                return micProblem_;
            }
        }
        micDevice_ = deviceName;
        micDeviceKey_ = deviceKey;
        micChannel_ = channel;
        micEnabled_ = enabled;
        if (params.contains("gain")) micGain_ = params["gain"].toDouble();
        if (params.contains("duckingEnabled")) micDuckingEnabled_ = params["duckingEnabled"].toBool();
        if (params.contains("duckingStrength")) micDuckingStrength_ = params["duckingStrength"].toDouble();
        // Only talkover sources feed the detector, so muting immediately stops
        // both the mic mix and further ducking; the normal release restores music.
        ControlObject::set(ConfigKey("[Microphone1]", "pregain"), micGain_);
        micGain_ = ControlObject::get(ConfigKey("[Microphone1]", "pregain"));
        ControlObject::set(ConfigKey("[Microphone1]", "talkover"), micEnabled_ ? 1 : 0);
        // Mixxx's strength is remaining music gain; the UI expresses reduction.
        ControlObject::set(ConfigKey("[Master]", "duckStrength"), 1 - micDuckingStrength_);
        ControlObject::set(ConfigKey("[Master]", "talkoverDucking"), micDuckingEnabled_ ? 1 : 0);
        micProblem_.clear();
        return {};
    }
    QJsonObject mixer() const override {
        auto value = PlaybackBackend::mixer();
        if (!mixer_) return value;
        value["available"] = available_;
        value["eqAvailable"] = available_; value["pflAvailable"] = available_ && pflAvailable_;
        value["crossfader"] = ControlObject::get(ConfigKey("[Master]", "crossfader"));
        value["beatFx"] = beatFx_ ? beatFx_->state() : QJsonObject{};
        value["masterGain"] = ControlObject::get(ConfigKey("[Master]", "gain"));
        value["headphoneGain"] = ControlObject::get(ConfigKey("[Master]", "headGain"));
        value["headphoneMix"] = ControlObject::get(ConfigKey("[Master]", "headMix"));
        auto channels = value["channels"].toObject();
        for (int index = 0; index < 4; ++index) {
            auto channel = channels[names[index]].toObject(); channel["available"] = available_;
            channel["orientation"] = ControlObject::get(ConfigKey(groups[index], "orientation"));
            channel["gain"] = ControlObject::get(ConfigKey(groups[index], "volume")); channel["pfl"] = ControlObject::get(ConfigKey(groups[index], "pfl")) > 0;
            channel["eqLow"] = ControlObject::get(ConfigKey(groups[index], "filterLow"));
            channel["eqMid"] = ControlObject::get(ConfigKey(groups[index], "filterMid"));
            channel["eqHigh"] = ControlObject::get(ConfigKey(groups[index], "filterHigh"));
            channel["colorFx"] = QJsonObject{{"effect",colorNames_[index]},{"amount",colorAmounts_[index]},{"parameters",effectParameters(effects_->getQuickEffectChain(groups[index])->getEffectSlot(0))}};
            channel["trim"] = ControlObject::get(ConfigKey(groups[index], "pregain"));
            channel["filter"] = effects_->getQuickEffectChain(groups[index])->getSuperParameter() * 2.0 - 1.0;
            const auto fxGroup = StandardEffectChain::formatEffectChainGroup(index);
            channel["fx"] = QJsonObject{{"effect", fxEffects_[index]}, {"enabled", ControlObject::get(ConfigKey(fxGroup, "enabled")) > 0}, {"mix", ControlObject::get(ConfigKey(fxGroup, "mix"))},{"depth",fxDepths_[index]},{"parameters",effectParameters(effects_->getStandardEffectChain(index)->getEffectSlot(0))}};
            channels[names[index]] = channel;
        }
        value["channels"] = channels;
        return value;
    }
    void load(int index, const QString& path, quint64 generation) override {
        pitchbend(index, 0);
        releaseScratch(index);
        disableFx(index);
        deckReady_[index]=false;deckErrors_[index].clear();
        generations_[index] = generation;
        for (const auto& control : {"slip_enabled","reverse","reverseroll","pitch_adjust"}) ControlObject::set(ConfigKey(groups[index],control),0);
        tracks_[index] = Track::newTemporary(path);
        decks_[index]->getEngineBuffer()->loadTrack(tracks_[index], false, nullptr);
    }
    void unload(int index) override {
        pitchbend(index, 0);
        releaseScratch(index);
        disableFx(index);
        deckReady_[index]=false;deckErrors_[index].clear();
        ++generations_[index]; tracks_[index].reset();
        ControlObject::set(ConfigKey(groups[index], "play"), 0);
        decks_[index]->getEngineBuffer()->ejectTrack();
    }
    QString colorFx(int index, const QString& name, double value) override {
        if (name != "filter" && name != "echo" && name != "pitchshift" && name != "whitenoise") return "Unknown Color FX";
        const auto manifest = builtinEffect(name);
        if (!manifest) return "Color FX processor unavailable";
        const auto chain = effects_->getQuickEffectChain(groups[index]);
        const auto slot = chain->getEffectSlot(0);
        const bool active = std::abs(value) > 0.005;
        // Publish the slot's enable state before loading, and through the static
        // setter. EffectSlot only forwards it to the audio thread from its own
        // valueChanged handler and from the tail of loadEffectInner(), and
        // EffectSlot::setEnabled() writes through the slot's own ControlObject,
        // which suppresses valueChanged for its own writes. Selecting a Color FX
        // while the knob was parked at zero otherwise loaded a processor the
        // engine still considered disabled, and the new effect stayed silent.
        ControlObject::set(ConfigKey(QuickEffectChain::formatEffectSlotGroup(groups[index]), "enabled"), active ? 1 : 0);
        if (slot->id() != manifest->id()) slot->loadEffectWithDefaults(manifest);
        chain->setSuperParameter(name == "echo" || name == "whitenoise" ? std::abs(value) : (value+1)/2, true);
        ControlObject::set(ConfigKey(chain->group(),"enabled"), active ? 1 : 0);
        colorNames_[index]=name; colorAmounts_[index]=value;
        return {};
    }
    QString beatFx(const QJsonObject& params) override { return beatFx_ ? beatFx_->configure(params) : "Beat FX unavailable"; }
    QJsonObject samplerState() const override { return samplers_ ? samplers_->state() : QJsonObject{}; }
    QString samplerCommand(const QString& op, const QJsonObject& params) override { return samplers_ ? samplers_->command(op, params) : "Sampler unavailable"; }
    void play(int index, bool enabled) override { ControlObject::set(ConfigKey(groups[index], "play"), enabled ? 1 : 0); }
    void seek(int index, double ms) override {
        if (tracks_[index] && tracks_[index]->getDuration() > 0) ControlObject::set(ConfigKey(groups[index], "playposition"), ms / (1000.0 * tracks_[index]->getDuration()));
    }
    // ネイティブ演奏入力 (inputSequence>0) と RPC のポインタ操作 (inputSequence==0) は
    // 同じデッキを共有する。begin が所有権を取り、もう一方の遅れた move/end/abort は
    // 新しいジェスチャーを止めない。ロード・アンロード等の生存管理は releaseScratch()
    // から所有権に関係なく解放する。
    void scratch(int index, const QString& phase, double ms, double capturedNativeUs = 0, bool keepalive = false, quint64 inputSequence = 0) override {
        if (!decks_[index]) return;
        const auto origin = inputSequence > 0 ? ScratchOrigin::Native : ScratchOrigin::Control;
        const bool release = phase == "end" || phase == "abort";
        if (phase == "begin") scratchOwner_[index] = origin;
        else if (scratchOwner_[index] != origin) return;
        else if (release) scratchOwner_[index] = ScratchOrigin::None;
        if (phase == "begin") pitchbend(index, 0);
        const double sampleRate = tracks_[index] ? tracks_[index]->getSampleRate().value() : 44100;
        // "abort" は曲の入れ替えなどでの強制解放。制動も着地もせず即座に離す。
        decks_[index]->requestScratch(!release, phase == "begin", ms * sampleRate * 2.0 / 1000.0, sampleRate, phase != "abort", capturedNativeUs, keepalive, inputSequence);
    }
    // ライフサイクル側の解放。所有者が誰であっても必ずジェスチャーを終わらせる。
    void releaseScratch(int index) {
        if (!decks_[index]) return;
        scratchOwner_[index] = ScratchOrigin::Control;
        scratch(index, "abort", 0);
    }
    bool scratching(int index) const override { return decks_[index] && decks_[index]->scratching(); }
    QJsonObject waveformCommand(const QString& op,const QJsonObject& p) override {
        if (!waveforms_) waveforms_=std::make_unique<waveform::Manager>();
        if(op=="waveform.ensure") {
            if(p["sourcePath"].isString())return waveforms_->ensure(p["sourcePath"].toString(),p["sourceGeneration"].toDouble());
            const auto index=QString("ABCD").indexOf(p["deck"].toString());
            if(p["deck"].toString().size()!=1 || index<0 || index>3 || !tracks_[index] || p["loadGeneration"].toDouble()!=double(generations_[index])) return {{"error","STALE_ASSET"}};
            return waveforms_->ensure(tracks_[index]->getLocation(),generations_[index]);
        }
        if(op=="waveform.requestRange") {
            if(p["sourcePath"].isString()){
                const auto binding=waveforms_->ensure(p["sourcePath"].toString(),p["sourceGeneration"].toDouble());
                if(binding["assetKey"]!=p["assetKey"])return {{"error","STALE_ASSET"}};
                return waveforms_->requestRange(p);
            }
            const auto index=QString("ABCD").indexOf(p["deck"].toString());
            if(p["deck"].toString().size()!=1||index<0||!tracks_[index]||p["loadGeneration"].toDouble()!=double(generations_[index]))return {{"error","STALE_ASSET"}};
            const auto binding=waveforms_->ensure(tracks_[index]->getLocation(),generations_[index]);
            if(binding["assetKey"]!=p["assetKey"])return {{"error","STALE_ASSET"}};
            return waveforms_->requestRange(p);
        }
        if(op=="waveform.cancelRequest"){waveforms_->cancelRequest(p["requestId"].toString());return {};}
        if(op=="waveform.invalidate"){waveforms_->invalidate(p["assetKey"].toString());return {};}
        if(op=="waveform.manifest")return waveforms_->manifest(p["assetKey"].toString());
        if(op=="waveform.acquireReadLease")return waveforms_->lease(p["assetKey"].toString(),p["resourceKey"].toString());
        if(op=="waveform.releaseReadLease"){waveforms_->release(p["leaseId"].toString());return {};}
        return {{"error","UNKNOWN_OP"}};
    }
    QJsonObject clockPoints(int index) override { return decks_[index]->clockPoints(); }
    QJsonObject timingTrace(int index) override {
        auto trace = decks_[index]->timingTrace();
        trace["nativeQuantize"] = ControlObject::get(ConfigKey(groups[index], "quantize"));
        return trace;
    }
    void tempo(int index, double value) override { ControlObject::set(ConfigKey(groups[index], "rate_ratio"), value); }
    void pitchbend(int index, double value) override {
        // Wheel is an additive audio-speed correction, separate from rate_ratio/BPM.
        const auto generation = ++bendGenerations_[index];
        ControlObject::set(ConfigKey(groups[index], "wheel"), value);
        if (value != 0) QTimer::singleShot(150, this, [this, index, generation] {
            if (bendGenerations_[index] == generation) pitchbend(index, 0);
        });
    }
    void keylock(int index, bool enabled) override { ControlObject::set(ConfigKey(groups[index], "keylock"), enabled ? 1 : 0); }
    void trackKey(int index, const QString& key) override { if (tracks_[index]) tracks_[index]->setKeyText(key); }
    void performanceControl(int index, const QString& control, double value) override { ControlObject::set(ConfigKey(groups[index], control), value); }
    void sync(int index, bool enabled) override { ControlObject::set(ConfigKey(groups[index], "sync_enabled"), enabled ? 1 : 0); }
    void setSyncLeader(int index) override { ControlObject::set(ConfigKey(groups[index], "sync_leader"), 1); }
    int syncLeader() const override {
        for (int index = 0; index < 4; ++index) {
            if (ControlObject::get(ConfigKey(groups[index], "sync_leader")) > 0) return index;
        }
        return -1;
    }
    void quantize(int index, bool enabled) override { placementQuantize_[index] = enabled; }
    void hotcue(int index, int cue, const QString& action, std::optional<double> ms) override {
        const QString prefix = QStringLiteral("hotcue_%1_").arg(cue + 1);
        if (action == "set" && ms && tracks_[index]) {
            trigger(index, prefix + "clear");
            // Native Track cue creation also supports a cue at exactly frame 0,
            // which the legacy hotcue_position setter deliberately rejects.
            tracks_[index]->createAndAddCue(mixxx::CueType::HotCue, cue,
                    mixxx::audio::FramePos(*ms * tracks_[index]->getSampleRate().value() / 1000.0),
                    mixxx::audio::kInvalidFramePos);
        } else trigger(index, prefix + (action == "set" ? "setcue" : action == "jump" ? "goto" : "clear"));
    }
    void loop(int index, double start, double end) override {
        loopEnable(index, false);
        // Clear old bounds first: new start may lie beyond the previous end.
        ControlObject::set(ConfigKey(groups[index], "loop_start_position"), -1);
        ControlObject::set(ConfigKey(groups[index], "loop_end_position"), -1);
        ControlObject::set(ConfigKey(groups[index], "loop_start_position"), engineSamples(index, start));
        ControlObject::set(ConfigKey(groups[index], "loop_end_position"), engineSamples(index, end));
    }
    void loopEnable(int index, bool enabled) override { ControlObject::set(ConfigKey(groups[index], "loop_enabled"), enabled ? 1 : 0); }
    void beatjump(int index, double beats) override {
        // This is a value command (ignoreNops=false), not a push button.
        // Sending a release value would queue a second, zero-beat seek.
        ControlObject::set(ConfigKey(groups[index], "beatjump"), beats);
    }
    void beatloop(int index, double beats) override {
        const auto& track = tracks_[index];
        const auto grid = track ? track->getBeats() : nullptr;
        if (!grid) return;
        const double sampleRate = track->getSampleRate().value();
        auto start = mixxx::audio::FramePos(positionMs(index) * sampleRate / 1000.0);
        if (placementQuantize_[index]) {
            if (beats >= 1) start = grid->findClosestBeat(start);
            else {
                mixxx::audio::FramePos prev, next;
                if (grid->findPrevNextBeats(start, &prev, &next, false) && next > prev) {
                    const double length = (next - prev) * beats;
                    start = prev + std::round((start - prev) / length) * length;
                }
            }
        }
        const auto end = grid->findNBeatsFromPosition(start, beats);
        if (!start.isValid() || !end.isValid() || end <= start) return;
        loop(index, start.value() * 1000.0 / sampleRate, end.value() * 1000.0 / sampleRate);
        loopEnable(index, true);
    }
    QJsonObject performanceState(int index) const override {
        QJsonArray cues;
        const double samplesPerMs = tracks_[index] ? tracks_[index]->getSampleRate().value() * 2.0 / 1000.0 : 0;
        for (int cue = 1; cue <= 16; ++cue) {
            const auto position = ControlObject::get(ConfigKey(groups[index], QStringLiteral("hotcue_%1_position").arg(cue)));
            cues.append(position >= 0 && samplesPerMs > 0 ? QJsonValue(position / samplesPerMs) : QJsonValue(QJsonValue::Null));
        }
        bool extended = false;
        for (int cue = 8; cue < cues.size(); ++cue) if (!cues[cue].isNull()) extended = true;
        if (!extended) while (cues.size() > 8) cues.removeLast();
        const auto start = ControlObject::get(ConfigKey(groups[index], "loop_start_position"));
        const auto end = ControlObject::get(ConfigKey(groups[index], "loop_end_position"));
        QJsonValue region = QJsonValue::Null;
        if (start >= 0 && end > start && samplesPerMs > 0) region = QJsonObject{{"startMs", start / samplesPerMs}, {"endMs", end / samplesPerMs}, {"enabled", ControlObject::get(ConfigKey(groups[index], "loop_enabled")) > 0}};
        return {{"hotCues", cues}, {"loopRegion", region}, {"quantize", placementQuantize_[index]},
            {"keylock", ControlObject::get(ConfigKey(groups[index], "keylock")) > 0},
            {"musicalKey", ControlObject::get(ConfigKey(groups[index], "key"))},
            {"keyShift", ControlObject::get(ConfigKey(groups[index], "pitch_adjust"))},
            {"slip", ControlObject::get(ConfigKey(groups[index], "slip_enabled")) > 0},
            {"reverse", ControlObject::get(ConfigKey(groups[index], "reverse")) > 0},
            {"slipReverse", ControlObject::get(ConfigKey(groups[index], "reverseroll")) > 0}};
    }
    void eq(int index, const QString& band, double gain) override {
        ControlObject::set(ConfigKey(groups[index], band == "low" ? "filterLow" : band == "mid" ? "filterMid" : "filterHigh"), gain);
    }
    void filter(int index, double value) override { colorFx(index, "filter", value); }
    void trim(int index, double gain) override { ControlObject::set(ConfigKey(groups[index], "pregain"), gain); }
    EffectManifestPointer builtinEffect(const QString& name) const {
        return effects_->getBackendManager()->getManifest(
            QStringLiteral("org.mixxx.effects.") + name, EffectBackendType::BuiltIn);
    }

    /** 掛かりを落とし、次回に必ず載せ直させる。曲の入れ替え時に使う。 */
    void disableFx(int index) {
        ControlObject::set(ConfigKey(StandardEffectChain::formatEffectChainGroup(index), "enabled"), 0);
        fxLoaded_[index] = false;
    }

    bool fx(int index, const QString& effect, bool enabled, double mix, double depth) override {
        const auto chain = effects_->getStandardEffectChain(index);
        // The pad chain gates on the chain switch below, so its one slot is
        // always on. Publish that through the static setter and before any load:
        // EffectSlot only forwards its enable state to the audio thread from its
        // own valueChanged handler and from the tail of loadEffectInner(), and
        // EffectSlot::setEnabled() writes through the slot's own ControlObject,
        // which suppresses valueChanged for its own writes. Until now this held
        // only because loading a track forces the reload below.
        ControlObject::set(ConfigKey(StandardEffectChain::formatEffectSlotGroup(index, 0), "enabled"), 1);
        if (!fxLoaded_[index] || fxEffects_[index] != effect) {
            const auto manifest = builtinEffect(effect);
            if (!manifest) return false;
            // Loading the manifest the slot already holds is a no-op, which
            // leaves the metaknob unlinked and the effect silent. Whichever pad
            // matched the slot's current effect stayed dead. Pass through a
            // different manifest first so the load is always a real transition.
            if (const auto other = builtinEffect(effect == QLatin1String("reverb") ? "echo" : "reverb")) {
                chain->getEffectSlot(0)->loadEffectWithDefaults(other);
            }
            chain->getEffectSlot(0)->loadEffectWithDefaults(manifest);
            fxEffects_[index] = effect;
            fxLoaded_[index] = true;
        }
        // Apply on every call, not only when the effect name changes. The rack
        // is pre-seeded with "echo", so a first press of that pad used to skip
        // this block entirely and stayed silent. Empty racks also start with a
        // zero metaknob, which mutes Echo's send and Flanger's mix.
        chain->getEffectSlot(0)->setMetaParameter(metaParameterFor(effect, depth), true);
        ControlObject::set(ConfigKey(chain->group(), "mix"), mix);
        ControlObject::set(ConfigKey(chain->group(), "enabled"), enabled ? 1 : 0);
        fxDepths_[index]=depth;
        return true;
    }

    /**
     * "depth" always means "more effect", but the metaknob does not: on the
     * filters its centre is the neutral setting and either side cuts. Map the
     * one onto the other so a pad at full depth is audible for every effect.
     */
    static double metaParameterFor(const QString& effect, double depth) {
        // Filters are neutral at the centre and cut towards either end.
        if (effect == QLatin1String("filter") || effect == QLatin1String("moogladder4filter")) {
            return 0.5 - depth * 0.5;
        }
        // Auto Pan's metaknob is its LFO rate: at the top the pan is too fast to
        // hear as movement, so keep full depth inside the range that reads.
        if (effect == QLatin1String("autopan")) {
            return 0.2 + depth * 0.35;
        }
        return depth;
    }
    bool resetFx() override {
        if (beatFx_) beatFx_->stop();
        if (!effects_) return false;
        bool changed = false;
        for (int index = 0; index < 4; ++index) {
            const auto key = ConfigKey(StandardEffectChain::formatEffectChainGroup(index), "enabled");
            if (ControlObject::get(key) > 0) {
                ControlObject::set(key, 0);
                changed = true;
            }
        }
        return changed;
    }
    bool beatgrid(int index, double bpm, double firstBeatMs, const std::optional<QVector<double>>& beatTimesMs) override {
        const auto& track = tracks_[index];
        if (!track || !track->getSampleRate().isValid()) return false;
        // Use the same immutable Beats replacement as Mixxx's own grid editor.
        // Track emits beatsUpdated to EngineBuffer/BpmControl; neither the
        // decoder nor the playhead is restarted. This runs on the Qt main thread.
        mixxx::BeatsPointer beats;
        if (beatTimesMs) {
            QVector<mixxx::audio::FramePos> positions;
            positions.reserve(beatTimesMs->size());
            for (double time : *beatTimesMs) positions.append(mixxx::audio::FramePos(time * track->getSampleRate().value() / 1000.0));
            beats = mixxx::Beats::fromBeatPositions(track->getSampleRate(), positions);
        } else {
            beats = mixxx::Beats::fromConstTempo(track->getSampleRate(),
                    mixxx::audio::FramePos(firstBeatMs * track->getSampleRate().value() / 1000.0),
                    mixxx::Bpm(bpm));
        }
        return beats && track->trySetBeats(beats);
    }
    QJsonObject beatgridState(int index) const override {
        const auto& track = tracks_[index];
        const auto beats = track ? track->getBeats() : nullptr;
        if (!beats || !track->getSampleRate().isValid() || (beats->getMarkers().empty() && (!beats->getLastMarkerBpm().isValid() || beats->getLastMarkerBpm().value()<=0))) return {};
        const double millisecondsPerFrame = 1000.0 / track->getSampleRate().value();
        QJsonArray markers;for(const auto& marker:beats->getMarkers())markers.append(QJsonObject{{"positionFrames",marker.position().value()},{"beatsTillNext",marker.beatsTillNextMarker()}});
        return {{"markers",markers},{"version", beats->getVersion()},
                {"constantTempo", beats->hasConstantTempo()},
                {"markerCount", static_cast<qint64>(beats->getMarkers().size())},
                {"firstBeatMs", beats->firstBeat().value() * millisecondsPerFrame},
                {"lastMarkerMs", beats->getLastMarkerPosition().value() * millisecondsPerFrame},
                {"lastMarkerBpm", beats->getLastMarkerBpm().value()}};
    }
    double effectiveBpm(int index) const override { return ControlObject::get(ConfigKey(groups[index], "bpm")); }
    double playbackRate(int index) const override { return ControlObject::get(ConfigKey(groups[index], "rate_ratio")); }
    double positionMs(int index) const override {
        return tracks_[index] ? ControlObject::get(ConfigKey(groups[index], "playposition")) * tracks_[index]->getDuration() * 1000.0 : 0;
    }
    // Mixxx が内部のクオンタイズに使うのと同じ「最も近い拍」。位置を丸めるのはこちらの仕事で、
    // Mixxx の quantize を握らせたままにしない（キュー登録が再生位置まで引っ張ってしまうため）。
    double quantizedPositionMs(int index) const override {
        const auto& track = tracks_[index];
        if (!track || !track->getSampleRate().isValid()) return positionMs(index);
        const auto beats = track->getBeats();
        if (!beats) return positionMs(index);
        const auto closest = beats->findClosestBeat(mixxx::audio::FramePos(positionMs(index) * track->getSampleRate().value() / 1000.0));
        if (!closest.isValid()) return positionMs(index);
        const double ms = closest.value() * 1000.0 / track->getSampleRate().value();
        return ms <= track->getDuration() * 1000.0 ? ms : positionMs(index);
    }
    bool playing(int index) const override { return ControlObject::get(ConfigKey(groups[index], "play")) > 0; }
    void gain(int index, double value) override { ControlObject::set(ConfigKey(groups[index], "volume"), value); }
    void orientation(int index, int value) override { ControlObject::set(ConfigKey(groups[index], "orientation"), value); }
    void pfl(int index, bool enabled) override { ControlObject::set(ConfigKey(groups[index], "pfl"), enabled ? 1 : 0); }
    void masterGain(double value) override { ControlObject::set(ConfigKey("[Master]", "gain"), value); }
    void crossfader(double value) override { ControlObject::set(ConfigKey("[Master]", "crossfader"), value); }
    QString recordingDirectory(const QString& path) override {
        if (!recorder_ || path.isEmpty()) return {};
        // RecordingManager は録音のたびにこの設定を読み直し、無ければ作る。
        // だからプロセスを立て直さなくても次の録音から新しい場所になる。
        settings_->set(ConfigKey(RECORDING_PREF_KEY, "Directory"), path);
        return recorder_->getRecordingDir();
    }

    QJsonArray recordingFormats() const override {
        QJsonArray formats;
        for (const auto& format : EncoderFactory::getFactory().getFormats()) {
            // The factory advertises the AAC family, but this build has no
            // FDK-AAC encoder: recording never starts ("could not start the
            // recording"). Measured for every format; only these five produce
            // a file, so do not offer a choice that silently fails.
            if (format.internalName.startsWith(QLatin1String("AAC")) ||
                format.internalName.startsWith(QLatin1String("HE-AAC"))) {
                continue;
            }
            formats.append(QJsonObject{{"name", format.internalName}, {"label", format.label},
                                       {"lossless", format.lossless}, {"extension", format.fileExtension}});
        }
        return formats;
    }
    QString recordingFormat(const QString& name) override {
        if (!recorder_) return {};
        // 使えない形式を受け付けると、録音のたびに既定へ落ちて気付けない。
        for (const auto& format : EncoderFactory::getFactory().getFormats()) {
            if (format.internalName != name) continue;
            settings_->set(ConfigKey(RECORDING_PREF_KEY, "Encoding"), name);
            return name;
        }
        return {};
    }

    QJsonObject recording() const override {
        const quint64 frames = recorder_ ? recorder_->recordingFrames() : 0;
        const quint32 sampleRate = recorder_ ? recorder_->recordingSampleRateHz() : 0;
        const qint64 frameElapsedMs = sampleRate ? static_cast<qint64>((frames * 1000ULL) / sampleRate) : 0;
        return {{"active", recordingActive_},
                {"stopping", recordingStopping_},
                {"format", settings_->getValueString(ConfigKey(RECORDING_PREF_KEY, "Encoding"))},
                {"formats", recordingFormats()},
                {"path", recordingPath_.isEmpty() ? QJsonValue::Null : QJsonValue(recordingPath_)},
                {"startedAt", recordingStartedAt_.isEmpty() ? QJsonValue::Null : QJsonValue(recordingStartedAt_)},
                {"elapsedMs", frameElapsedMs > 0 ? frameElapsedMs : recordedElapsedMs_},
                {"sampleRateHz", static_cast<qint64>(sampleRate)},
                {"frameCount", static_cast<qint64>(frames)},
                {"timelineQuality", "recording_frame_clock"},
                {"error", recordingError_.isEmpty() ? QJsonValue::Null : QJsonValue(recordingError_)}};
    }
    void startRecording() override {
        if (!recorder_ || recordingActive_ || recordingPending_ || recordingStopping_ || recorder_->isRecordingActive()) return;
        if (recordingCooldown_.isValid() && recordingCooldown_.elapsed() < 1100) {
            recordingError_ = "Wait one second before starting another recording to keep filenames unique";
            if (recordingChanged) recordingChanged(recording());
            return;
        }
        recordingPath_.clear(); recordingStartedAt_.clear(); recordedElapsedMs_ = 0; recordingError_.clear();
        recordingTimer_.invalidate();
        recordingPending_ = true;
        recorder_->startRecording();
    }
    void stopRecording() override {
        if (!recorder_ || recordingStopping_ || (!recordingActive_ && !recordingPending_)) return;
        recordedElapsedMs_ = recordingTimer_.isValid() ? recordingTimer_.elapsed() : 0;
        recordingStopping_ = true;
        if (!recordingPending_) recorder_->stopRecording();
        if (recordingChanged) recordingChanged(recording());
    }
private:
    std::unique_ptr<waveform::Manager> waveforms_;
    QString channelName(int handle) const {for(const auto& channel:effects_->registeredInputChannels())if(channel.handle().handle()==handle)return channel.name();for(const auto& channel:effects_->registeredOutputChannels())if(channel.handle().handle()==handle)return channel.name();return {};}
    void tryCaptureGraph(){
        if(snapshotWrite_.valid()){
            if(snapshotWrite_.wait_for(std::chrono::seconds(0))==std::future_status::ready){auto result=snapshotWrite_.get();capturedGraph_=result.first;if(!result.second.isEmpty())capturedGraph_["dspStateAssets"]=result.second;else if(capturedDspReady_){capturedGraph_["dspStateComplete"]=false;capturedGraph_["requiresHistoricalDsp"]=true;}}
            return;
        }
        if(!snapshotPending_||restoring_)return;
        if(!snapshotTransfer_){
            snapshotProcessor_=nullptr;preparedDsp_={};capturedDspReady_=false;preparedKeylock_.clear();preparedFx_.clear();preparedFxSlots_.clear();
            for(const auto& slot:activeFxSlots())if(junction::fx::supported(slot->id())){preparedFxSlots_.push_back(slot);preparedFx_.push_back(junction::fx::prepare(*slot,[this](int handle){return channelName(handle);}));}
            for(int i=0;i<4;i++)if(tracks_[i]&&(ControlObject::get(ConfigKey(groups[i],"keylock"))>0||std::abs(ControlObject::get(ConfigKey(groups[i],"pitch_adjust")))>0.00001)){preparedKeylock_.emplace_back();preparedKeylock_.back().deck=i;}
            const auto beat=beatFx_->state();if(beat["enabled"].toBool()){snapshotProcessor_=junction::ddj::latest(beat["processor"].toString());if(snapshotProcessor_)preparedDsp_=snapshotProcessor_->prepare([this](int handle){return channelName(handle);});}
            snapshotTransfer_=renderDriver_.requestTransfer(junction::GraphDriver::None,junction::RenderMode::Cold);return;
        }
        if(audioBridge_.inputReaders.load(std::memory_order_acquire)||!renderDriver_.transferComplete(snapshotTransfer_))return;
        if(snapshotProcessor_&&snapshotProcessor_==junction::ddj::latest(preparedDsp_.processor)&&!preparedDsp_.routes.empty())capturedDspReady_=snapshotProcessor_->captureInto(preparedDsp_);
        bool keylockFailed=false;for(auto& state:preparedKeylock_){const auto ok=junction::keylock::capture(*decks_[state.deck]->getEngineBuffer(),state);if(!ok)keylockFailed=true;if(qEnvironmentVariableIsSet("PLUMDECK_JUNCTION_TRACE"))qWarning()<<"junction keylock capture"<<state.deck<<ok<<state.position<<state.speed<<state.pitch<<state.processor.virtualPitch<<state.processor.virtualTempo;}
        bool fxFailed=false;for(size_t i=0;i<preparedFx_.size();i++)if(!junction::fx::capture(*preparedFxSlots_[i],preparedFx_[i]))fxFailed=true;
        auto graph=snapshotStoppedGraph();if(fxFailed){graph["snapshotError"]="FXの状態をまだ引き継げません";preparedFx_.clear();}if(keylockFailed){graph["snapshotError"]="キー固定処理の状態をまだ引き継げません";preparedKeylock_.clear();}
        renderDriver_.requestTransfer(junction::GraphDriver::Realtime,junction::RenderMode::Performing);audioBridge_.inputsEnabled.store(true,std::memory_order_release);
        snapshotPending_=false;snapshotTransfer_=0;snapshotProcessor_=nullptr;
        if(!capturedDspReady_&&preparedKeylock_.empty()&&preparedFx_.empty()){capturedGraph_=graph;return;}
        const auto path=profile_.filePath(QStringLiteral("junction-dsp.bin"));
        auto snapshot=std::move(preparedDsp_);auto keylocks=std::move(preparedKeylock_);const auto keylockPath=profile_.filePath("junction-keylock.bin");
        auto fxStates=std::move(preparedFx_);const auto fxPath=profile_.filePath("junction-fx.bin");preparedFxSlots_.clear();
        snapshotWrite_=std::async(std::launch::async,[path,keylockPath,fxPath,graph,snapshot=std::move(snapshot),keylocks=std::move(keylocks),fxStates=std::move(fxStates)]() mutable {
            QJsonArray assets;
            if(!snapshot.routes.empty()&&junction::ddj::write(path,snapshot))assets.append(QJsonObject{{"path",path},{"format","plumdeck-ddj-dsp-v1"},{"fingerprint",QString::fromLatin1(junction::ddj::fingerprint)},{"processor",snapshot.processor},{"byteSize",double(QFileInfo(path).size())}});
            if(!keylocks.empty()&&junction::keylock::write(keylockPath,keylocks))assets.append(QJsonObject{{"path",keylockPath},{"format",junction::keylock::format},{"fingerprint",junction::keylock::fingerprint},{"byteSize",double(QFileInfo(keylockPath).size())}});
            if(!fxStates.empty()&&junction::fx::write(fxPath,fxStates))assets.append(QJsonObject{{"path",fxPath},{"format",junction::fx::format},{"fingerprint",junction::fx::fingerprint},{"byteSize",double(QFileInfo(fxPath).size())}});
            const int expected=int(!snapshot.routes.empty())+int(!keylocks.empty())+int(!fxStates.empty());
            if(assets.size()!=expected)graph["snapshotError"]="FXの引き継ぎ状態を保存できません";
            return std::make_pair(graph,assets);
        });
    }
    std::vector<EffectSlotPointer> activeFxSlots() const {
        std::vector<EffectSlotPointer> activeSlots;
        for(int i=0;i<4;i++){
            if(std::abs(colorAmounts_[i])>.005)activeSlots.push_back(effects_->getQuickEffectChain(groups[i])->getEffectSlot(0));
            if(ControlObject::get(ConfigKey(StandardEffectChain::formatEffectChainGroup(i),"enabled"))>0)activeSlots.push_back(effects_->getStandardEffectChain(i)->getEffectSlot(0));
        }
        if(beatFx_->state()["enabled"].toBool())activeSlots.push_back(beatFx_->getEffectSlot(0));
        return activeSlots;
    }
    EffectSlotPointer findFxSlot(const QString& group) const {
        for(int i=0;i<4;i++)for(const auto& slot:{effects_->getQuickEffectChain(groups[i])->getEffectSlot(0),effects_->getStandardEffectChain(i)->getEffectSlot(0)})if(slot->getGroup()==group)return slot;
        auto beat=beatFx_->getEffectSlot(0);return beat->getGroup()==group?beat:EffectSlotPointer{};
    }
    static QJsonObject effectParameters(const EffectSlotPointer& slot){QJsonObject result;for(const auto& map:{slot->getLoadedParameters(),slot->getHiddenParameters()})for(const auto& values:map)for(const auto& parameter:values)result[parameter->manifest()->id()]=parameter->getValue();return result;}
    bool restoreEffectParameters(const EffectSlotPointer& slot,const QJsonObject& values){
        for(const auto& map:{slot->getLoadedParameters(),slot->getHiddenParameters()})for(const auto& parameters:map)for(const auto& parameter:parameters){const auto value=values[parameter->manifest()->id()];if(value.isUndefined())continue;if(!value.isDouble()||value.toDouble()<parameter->manifest()->getMinimum()||value.toDouble()>parameter->manifest()->getMaximum()){restoreError_="Invalid Junction FX parameter";return false;}parameter->setValue(value.toDouble());parameter->updateEngineState();}return true;
    }
    static const std::array<const char*,20>& graphControls(){static const std::array<const char*,20> keys={"rate","rateRange","keylock","pitch_adjust","slip_enabled","reverse","reverseroll","volume","pregain","filterLow","filterMid","filterHigh","orientation","loop_start_position","loop_end_position","loop_enabled","cue_point","sync_enabled","sync_leader","rate_ratio"};return keys;}
    void tryFinalizeGraphRestore(){
        if(!restoring_||!samplers_||!samplers_->junctionLoaded())return;
        for(int i=0;i<4;++i)if(tracks_[i]&&!deckReady_[i])return;
        if(!restoreTransfer_){restoreTransfer_=renderDriver_.requestTransfer(aligning_?junction::GraphDriver::Replay:junction::GraphDriver::None,aligning_?junction::RenderMode::WarmOffline:junction::RenderMode::Cold);return;}
        if(audioBridge_.inputReaders.load(std::memory_order_acquire)!=0)return;
        if(!renderDriver_.transferComplete(restoreTransfer_))return;
        // Drop any input-only device pointer queued just before ownership
        // closed; its hardware memory can be reused while replay is running.
        if(microphone_)microphone_->receiveBuffer(AudioInput(AudioPathType::Microphone,0,mixxx::audio::ChannelCount::stereo(),0),nullptr,0);
        // One owner released at a block boundary. All controls and queued
        // exact seeks become visible together to the next realtime block.
        if(aligning_){
            if(auto* runtime=junctionRuntime_.load(std::memory_order_acquire))alignTarget_=std::max(alignTarget_,runtime->currentMediaFrame());
            const auto origin=originalGraph_["atMediaFrame"].toString().toULongLong();
            const auto delta=alignTarget_-origin;
            const auto target=restoreStartRender_+(delta/48000)*44100+(delta%48000)*44100/48000;
            // Device sinks read their separate idle buffers while Replay owns
            // the graph. Bound each main-thread burst; decoding and Qt control
            // delivery continue between bursts, never a second EngineMixer.
            for(unsigned block=0;block<8&&renderDriver_.clock().renderFrame()<target;++block){
                const auto grant=renderDriver_.beginBlock(junction::GraphDriver::Replay);if(!grant.mayProcess)return;
                const unsigned frames=unsigned(std::min<quint64>(256,target-renderDriver_.clock().renderFrame()));
                mixer_->process(frames*2);
                renderDriver_.clock().advance(frames);
                if(grant.releaseRequested){renderDriver_.acknowledgeRelease(junction::GraphDriver::Replay,renderDriver_.clock().renderFrame(),grant.generation);return;}
            }
            if(renderDriver_.clock().renderFrame()<target)return;
            if(auto* runtime=junctionRuntime_.load(std::memory_order_acquire)){const auto source=renderDriver_.clock().renderFrame(),rendered=source-restoreStartRender_;runtime->setCaptureAnchor(source,origin+(rendered/44100)*48000+(rendered%44100)*48000/44100);}
            const auto release=renderDriver_.requestTransfer(junction::GraphDriver::Realtime,junction::RenderMode::ArmedRealtime);
            renderDriver_.acknowledgeRelease(junction::GraphDriver::Replay,renderDriver_.clock().renderFrame(),release);
            if(!renderDriver_.transferComplete(release))return;
            aligning_=false;
        }else{
            for(int i=0;i<4;++i)applyGraphDeck(i);
            restoreError_=samplers_->finalizeJunctionRestore();
            restoreStartRender_=renderDriver_.clock().renderFrame();
            if(auto* runtime=junctionRuntime_.load(std::memory_order_acquire)){bool valid=false;const auto origin=originalGraph_["atMediaFrame"].toString().toULongLong(&valid);if(valid)runtime->setCaptureAnchor(restoreStartRender_,origin);}
            applyGraphMixer();pendingMixer_={};
            if(!pendingFx_.empty()){effects_->getEngineEffectsManager()->onCallbackStart();for(const auto& snapshot:pendingFx_){auto slot=findFxSlot(snapshot.group);if(!slot||!junction::fx::restore(*slot,snapshot,[this](const QString& name){return handles_->handleForGroup(name).handle();}))restoreError_="FX state or routes differ from checkpoint";}pendingFx_.clear();}
            for(const auto& snapshot:pendingDsp_){auto* processor=junction::ddj::latest(snapshot.processor);if(!processor||!processor->restore(snapshot,[this](const QString& name){return handles_->handleForGroup(name).handle();}))restoreError_="DSP processor routes differ from checkpoint";}
            pendingDsp_.clear();
            for(const auto& state:pendingKeylock_){const auto ok=junction::keylock::restore(*decks_[state.deck]->getEngineBuffer(),state);if(!ok)restoreError_="Keylock DSP state differs from checkpoint";if(qEnvironmentVariableIsSet("PLUMDECK_JUNCTION_TRACE"))qWarning()<<"junction keylock restore"<<state.deck<<ok<<state.position<<state.speed<<state.pitch<<state.processor.virtualPitch<<state.processor.virtualTempo;}
            pendingKeylock_.clear();
        }
        restoring_=false;restoreTransfer_=0;
        renderDriver_.requestTransfer(junction::GraphDriver::Realtime,junction::RenderMode::ArmedRealtime);
        audioBridge_.inputsEnabled.store(true,std::memory_order_release);
    }
    void applyGraphDeck(int i){
        if(restoreDecks_[i].isEmpty())return;
        const auto row=restoreDecks_[i];restoreDecks_[i]={};
        const double rate=tracks_[i]->getSampleRate().value();
        if(row["sourceSampleRateHz"].toDouble()!=rate||row["positionFrames"].toDouble()>tracks_[i]->getDuration()*rate){deckErrors_[i]="Resolved asset format or duration differs from graph";return;}
        const auto grid=row["beatgrid"].toObject();
        if(!grid["markers"].toArray().isEmpty()||grid["lastMarkerBpm"].toDouble()>0){
            std::vector<mixxx::BeatMarker> markers;
            for(const auto value:grid["markers"].toArray()){const auto marker=value.toObject();markers.emplace_back(mixxx::audio::FramePos(marker["positionFrames"].toDouble()),marker["beatsTillNext"].toInt());}
            const auto beats=mixxx::Beats::fromBeatMarkers(tracks_[i]->getSampleRate(),markers,mixxx::audio::FramePos(grid["lastMarkerMs"].toDouble()*rate/1000),mixxx::Bpm(grid["lastMarkerBpm"].toDouble()));
            if(!beats||!tracks_[i]->trySetBeats(beats)){deckErrors_[i]="Junction beatgrid could not be restored";return;}
        }
        const auto controls=row["controls"].toObject();
        for(const auto* key:graphControls())if(controls.contains(key))ControlObject::set(ConfigKey(groups[i],key),controls[key].toDouble());
        const auto performance=row["performance"].toObject();placementQuantize_[i]=performance["quantize"].toBool();
        const auto cues=performance["hotCues"].toArray();for(int c=0;c<cues.size()&&c<16;++c)if(cues[c].isDouble())ControlObject::set(ConfigKey(groups[i],QStringLiteral("hotcue_%1_position").arg(c+1)),engineSamples(i,cues[c].toDouble()));
        decks_[i]->getEngineBuffer()->queueNewPlaypos(mixxx::audio::FramePos(row["positionFrames"].toDouble()),EngineBuffer::SEEK_EXACT);
        play(i,row["play"].toBool());restoreAfter_[i]=decks_[i]->junctionAudioBlocks()+2;
    }
    void applyGraphMixer(){
        if(pendingMixer_.isEmpty())return;
        const auto m=pendingMixer_;masterGain(m["masterGain"].toDouble(.5));crossfader(m["crossfader"].toDouble());
        const auto channels=m["channels"].toObject();
        for(int i=0;i<4;++i){const auto ch=channels[names[i]].toObject();const auto color=ch["colorFx"].toObject();const auto colorError=colorFx(i,color["effect"].toString("filter"),color["amount"].toDouble());if(!colorError.isEmpty())restoreError_=colorError;restoreEffectParameters(effects_->getQuickEffectChain(groups[i])->getEffectSlot(0),color["parameters"].toObject());const auto effect=ch["fx"].toObject();if(!fx(i,effect["effect"].toString("echo"),effect["enabled"].toBool(),effect["mix"].toDouble(),effect["depth"].toDouble(.5)))restoreError_="Junction FX processor unavailable";restoreEffectParameters(effects_->getStandardEffectChain(i)->getEffectSlot(0),effect["parameters"].toObject());}
        auto beat=m["beatFx"].toObject();if(beat["auto"].toBool())beat.remove("bpm");if(!beat.isEmpty()){const auto error=beatFx_->restoreSemantic(beat);if(!error.isEmpty())restoreError_=error;}
    }
    bool snapshotPending_=false,capturedDspReady_=false;
    quint64 snapshotTransfer_=0;
    junction::ddj::Processor* snapshotProcessor_=nullptr;
    junction::ddj::Snapshot preparedDsp_;
    std::vector<junction::ddj::Snapshot> pendingDsp_;
    QJsonObject capturedGraph_;
    std::future<std::pair<QJsonObject,QJsonArray>> snapshotWrite_;
    junction::keylock::Snapshot preparedKeylock_,pendingKeylock_;
    junction::fx::Snapshot preparedFx_,pendingFx_;
    std::vector<EffectSlotPointer> preparedFxSlots_;
    std::atomic<quint64> audioCaptureSequence_{0};
    std::atomic<junction::Runtime*> junctionRuntime_{nullptr};
    junction::AudioBridge audioBridge_;
    junction::ReplayDriver renderDriver_{44100,0};
    junction::DriveGrant blockGrant_;
    junction::PrivatePreview preview_;
    std::array<QJsonObject,4> restoreDecks_;
    std::array<quint64,4> restoreAfter_{};
    std::array<bool,4> deckReady_{};
    std::array<QString,4> deckErrors_;
    std::array<double,4> fxDepths_{};
    QJsonObject pendingMixer_;
    bool restoring_=false,aligning_=false;
    QJsonObject originalGraph_;
    quint64 alignTarget_=0,restoreStartRender_=0;
    quint64 restoreTransfer_=0;
    QString restoreError_;
    void saveScratchDiagnostics() {
        QJsonObject deckStates;
        bool changed = false, active = false;
        for (int index = 0; index < 4; ++index) {
            if (!decks_[index]) continue;
            auto state = decks_[index]->audioDiagnostics();
            active |= state["scratching"].toBool();
            const auto buffers = state["buffers"].toDouble();
            changed |= buffers != diagnosticBuffers_[index];
            diagnosticBuffers_[index] = buffers;
            state["durationMs"] = tracks_[index] ? tracks_[index]->getDuration() * 1000.0 : 0;
            state["tempoRate"] = playbackRate(index);
            state["effectiveBpm"] = effectiveBpm(index);
            deckStates[names[index]] = state;
        }
        if (changed) {
            diagnosticDirty_ = true;
            diagnosticHistory_.append(QJsonObject{{"timeMs", static_cast<double>(QDateTime::currentMSecsSinceEpoch())},
                {"decks", deckStates}, {"mixer", mixer()},
                {"masterPeak", ControlObject::get(ConfigKey("[Main]", "vu_meter"))}});
            while (diagnosticHistory_.size() > 40) diagnosticHistory_.removeFirst();
        }
        // Persist after release; disk flushes must not delay incoming scratch
        // commands on the Qt thread while the user is moving the platter.
        if (active || !diagnosticDirty_) return;
        // Bounded per-process file, retained after the gesture so reproduction
        // doesn't require synchronizing with a short monitor window.
        const auto path = QDir::temp().filePath(QString("plumdeck-scratch-%1.json").arg(QCoreApplication::applicationPid()));
        QSaveFile file(path);
        if (file.open(QIODevice::WriteOnly)) {
            file.setPermissions(QFileDevice::ReadOwner | QFileDevice::WriteOwner);
            file.write(QJsonDocument(QJsonObject{{"schema", 1}, {"output", output_}, {"samples", diagnosticHistory_}}).toJson(QJsonDocument::Compact));
            if (file.commit()) diagnosticDirty_ = false;
        }
    }
    void trigger(int index, const QString& control) {
        ControlObject::set(ConfigKey(groups[index], control), 1);
        ControlObject::set(ConfigKey(groups[index], control), 0);
    }
    double engineSamples(int index, double ms) const { return ms * tracks_[index]->getSampleRate().value() * 2.0 / 1000.0; }
    QTemporaryDir profile_;
    UserSettingsPointer settings_;
    std::unique_ptr<mixxx::ControlIndicatorTimer> indicator_;
    ChannelHandleFactoryPointer handles_;
    std::unique_ptr<ControlObject> deckCount_, samplerCount_, previewCount_;
    std::unique_ptr<EffectsManager> effects_;
    std::unique_ptr<EngineMixer> mixer_;
    std::unique_ptr<RecordingManager> recorder_;
    std::array<ScratchDeck*, 4> decks_{};
    enum class ScratchOrigin { None, Control, Native };
    std::array<ScratchOrigin, 4> scratchOwner_{};
    std::array<QString,4> colorNames_{"filter","filter","filter","filter"};
    std::array<double,4> colorAmounts_{};
    std::unique_ptr<BeatFx> beatFx_;
    std::unique_ptr<SamplerBank> samplers_;
    EngineMicrophone* microphone_ = nullptr;
    QString micDevice_, micDeviceKey_, micProblem_;
    int micChannel_ = 0;
    double micGain_ = 1, micDuckingStrength_ = 0.65;
    bool micEnabled_ = false, micDuckingEnabled_ = false;
    std::unique_ptr<SoundManager> sound_;
    SoundDevicePointer outputDevice_;
    int masterStart_ = 0, pflStart_ = -1;
    std::array<TrackPointer, 4> tracks_;
    std::array<quint64, 4> generations_{};
    std::array<double, 4> diagnosticBuffers_{};
    QJsonArray diagnosticHistory_;
    bool diagnosticDirty_ = false;
    std::array<quint64, 4> bendGenerations_{};
    std::array<bool, 4> placementQuantize_{true, true, true, true};
    std::array<QString, 4> fxEffects_;
    // 起動直後はチェーンがまだ engine に繋がっておらず、ここでのロードは効かない。
    // 「名前を覚えている」ことと「実際にスロットに載っている」ことを分けて持つ。
    std::array<bool, 4> fxLoaded_{};
    QElapsedTimer recordingTimer_, recordingCooldown_;
    QString recordingPath_, recordingStartedAt_, recordingError_;
    qint64 recordedElapsedMs_ = 0;
    bool recordingActive_ = false, recordingPending_ = false, recordingStopping_ = false;
    bool available_ = false, started_ = false, pflAvailable_ = false;
    QString problem_ = "Mixxx audio initialization has not completed", output_;
};
}
std::unique_ptr<PlaybackBackend> makeBackend() { return std::make_unique<MixxxBackend>(); }
