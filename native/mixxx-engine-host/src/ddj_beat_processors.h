#pragma once
#include "effects/backends/effectmanifest.h"
#include "effects/backends/effectprocessor.h"
#include "engine/effects/engineeffectparameter.h"
#include "control/controlobject.h"
#include <algorithm>
#include <array>
#include <atomic>
#include <cmath>
#include <vector>
#include "effects/backends/builtin/echoeffect.h"
#include "effects/backends/builtin/reverbeffect.h"
#include "effects/backends/builtin/tremoloeffect.h"
#include "effects/backends/builtin/flangereffect.h"
#include "effects/backends/builtin/phasereffect.h"
#include "effects/backends/builtin/pitchshifteffect.h"

// A host-only timing adapter. AUTO forwards source beat information unchanged;
// TAP supplies a fixed beat length to the same processor, including on master.
template<class Processor> class DdjTimedProcessor : public EffectProcessor {
public:
    DdjTimedProcessor() {
        for(int deck=0;deck<4;++deck) {
            const auto group=QStringLiteral("[Channel%1]").arg(deck+1);
            tempos_[deck]=ControlObject::getControl(ConfigKey(group,"bpm"));
            playing_[deck]=ControlObject::getControl(ConfigKey(group,"play"));
            leaders_[deck]=ControlObject::getControl(ConfigKey(group,"sync_leader"));
        }
    }
    static QString getId() { return QString(Processor::getId()).replace("org.mixxx.","org.plumdeck."); }
    static EffectManifestPointer getManifest() {
        auto m=Processor::getManifest(); m->setId(getId());
        auto bpm=m->addParameter(); bpm->setId("manual_bpm"); bpm->setName("Manual BPM (0 = AUTO)");
        bpm->setValueScaler(EffectManifestParameter::ValueScaler::Linear); bpm->setRange(0,0,300);
        return m;
    }
    void initialize(const QSet<ChannelHandleAndGroup>& a,const QSet<ChannelHandleAndGroup>& b,const mixxx::EngineParameters& p) override { processor_.initialize(a,b,p); }
    void initializeInputChannel(ChannelHandle c,const mixxx::EngineParameters& p) override { processor_.initializeInputChannel(c,p); }
    bool hasStatesForInputChannel(ChannelHandle c) const override { return processor_.hasStatesForInputChannel(c); }
    void loadEngineEffectParameters(const QMap<QString,EngineEffectParameterPointer>& p) override { bpm_=p.value("manual_bpm"); processor_.loadEngineEffectParameters(p); }
    SINT getGroupDelayFrames() override { return processor_.getGroupDelayFrames(); }
    void process(const ChannelHandle& a,const ChannelHandle& b,const CSAMPLE* in,CSAMPLE* out,
            const mixxx::EngineParameters& p,EffectEnableState enabled,const GroupFeatureState& source) override {
        auto features=source;
        if(bpm_->value()>=40) { features.beat_length=GroupFeatureBeatLength{60.0/bpm_->value(),1}; features.beat_fraction_buffer_end.reset(); }
        else if(!features.beat_length.has_value()) {
            // Master/microphone buses do not carry a track's beat metadata.
            // Follow the sync leader, otherwise the first playing deck.
            double tempo=120;
            for(int pass=0;pass<2;++pass) {
                bool found=false;
                for(int deck=0;deck<4;++deck) {
                    const auto* chosen=pass==0?leaders_[deck]:playing_[deck];
                    if(chosen && chosen->get()>0 && tempos_[deck] && tempos_[deck]->get()>=40) {
                        tempo=tempos_[deck]->get(); found=true; break;
                    }
                }
                if(found) break;
            }
            features.beat_length=GroupFeatureBeatLength{60.0/tempo,1};
        }
        processor_.process(a,b,in,out,p,enabled,features);
    }
private:
    Processor processor_;
    EngineEffectParameterPointer bpm_;
    std::array<ControlObject*,4> tempos_{}, playing_{}, leaders_{};
};

// Host-owned implementations of the eight selector positions absent from
// Mixxx. These are independent DSP, not bit-identical Pioneer algorithms.
struct DdjBeatState : EffectState {
    explicit DdjBeatState(const mixxx::EngineParameters& p) : EffectState(p), ring(size_t(p.sampleRate()) * 8, 0) {}
    std::vector<float> ring; // Four seconds, stereo, allocated off the audio thread.
    size_t write = 0, captured = 0, loopLength = 0, loopStart = 0, loopRead = 0;
    double phase = 0, delay = 0, lastBeats = 0;
    std::array<double, 2> low{}, feedback{};
    std::array<double, 6> oscillators{};
};

template<int Mode> class DdjBeatProcessor : public EffectProcessor {
public:
    struct OwnedRoute {int input,output;unsigned rate;std::unique_ptr<DdjBeatState> state;};
    std::vector<OwnedRoute> routes_;
    std::atomic<size_t> publishedRoutes_{0};
    DdjBeatProcessor(){routes_.reserve(1024);}
    QSet<ChannelHandleAndGroup> outputs_;
    void initialize(const QSet<ChannelHandleAndGroup>& inputs,const QSet<ChannelHandleAndGroup>& outputs,const mixxx::EngineParameters& parameters) override {outputs_=outputs;for(const auto& input:inputs)initializeInputChannel(input.handle(),parameters);}
    void initializeInputChannel(ChannelHandle input,const mixxx::EngineParameters& parameters) override {for(const auto& output:outputs_){if(routes_.size()>=1024)return;routes_.push_back({input.handle(),output.handle().handle(),unsigned(parameters.sampleRate()),std::make_unique<DdjBeatState>(parameters)});publishedRoutes_.store(routes_.size(),std::memory_order_release);}}
    bool hasStatesForInputChannel(ChannelHandle input) const override {for(const auto& route:routes_)if(route.input==input.handle())return true;return false;}
    SINT getGroupDelayFrames() override{return 0;}
    void process(const ChannelHandle& input,const ChannelHandle& output,const CSAMPLE* in,CSAMPLE* out,const mixxx::EngineParameters& parameters,EffectEnableState enabled,const GroupFeatureState& features) override {
        const auto count=publishedRoutes_.load(std::memory_order_acquire);for(size_t i=0;i<count;++i){auto& route=routes_[i];if(route.input==input.handle()&&route.output==output.handle()){processChannel(route.state.get(),in,out,parameters,enabled,features);return;}}
        std::copy_n(in,parameters.samplesPerBuffer(),out);
    }
    static QString getId() {
        static const char* names[] = {"lowcutecho","mtdelay","spiral","enigmajet","sliproll","roll","mobiussaw","mobiustri"};
        return QStringLiteral("org.mixxx.effects.") + names[Mode];
    }
    static EffectManifestPointer getManifest() {
        EffectManifestPointer m(new EffectManifest());
        m->setId(getId()); m->setName(getId()); m->setShortName(getId());
        m->setAuthor("plumdeck"); m->setVersion("1.0");
        m->setDescription("plumdeck beat-synchronized processor");
        m->setAddDryToWet(false); m->setEffectRampsFromDry(false);
        auto beats=m->addParameter(); beats->setId("beats"); beats->setName("Beats");
        beats->setValueScaler(EffectManifestParameter::ValueScaler::Linear); beats->setRange(.125, .5, 2);
        auto depth=m->addParameter(); depth->setId("depth"); depth->setName("Depth");
        depth->setValueScaler(EffectManifestParameter::ValueScaler::Linear); depth->setRange(0, .5, 1);
        return m;
    }
    void loadEngineEffectParameters(const QMap<QString, EngineEffectParameterPointer>& p) override {
        beats_=p.value("beats"); depth_=p.value("depth");
    }
    void processChannel(DdjBeatState* s, const CSAMPLE* in, CSAMPLE* out,
            const mixxx::EngineParameters& p, EffectEnableState enabled, const GroupFeatureState& features) {
        renderState(s,in,out,p,enabled,features,beats_->value(),depth_->value());
    }
    static void renderState(DdjBeatState* s,const CSAMPLE* in,CSAMPLE* out,const mixxx::EngineParameters& p,EffectEnableState enabled,const GroupFeatureState& features,double beats,double depth) {
        const double rate=p.sampleRate();
        const size_t capacity=s->ring.size()/2;
        const double seconds=features.beat_length.has_value()?features.beat_length->seconds:.5;
        const double target=std::clamp(seconds*beats*rate, 8.0, double(capacity-2));
        if (enabled==EffectEnableState::Enabling) {
            // No clearing large delay buffers in the callback: captured bounds
            // invalidate old samples in O(1), including repeated ON/OFF.
            s->write=0; s->captured=0; s->loopRead=0; s->loopLength=0; s->phase=0;
            s->low={}; s->feedback={}; s->delay=target;
        }
        if constexpr (Mode==4) {
            // SLIP ROLL resamples the incoming audio when its beat size changes.
            if(s->lastBeats!=beats) { s->captured=0; s->write=0; s->loopLength=0; s->loopRead=0; }
        }
        s->lastBeats=beats;
        const auto read=[&](double back,int ch) {
            back=std::clamp(back,1.0,double(capacity-2));
            if(back>double(s->captured)) return 0.0;
            double at=std::fmod(double(s->write)+capacity-back,double(capacity));
            size_t a=size_t(at); double f=at-a;
            return double(s->ring[2*a+ch])*(1-f)+double(s->ring[2*((a+1)%capacity)+ch])*f;
        };
        for(int frame=0;frame<p.framesPerBuffer();++frame) {
            s->phase=std::fmod(s->phase+1.0/target,1.0);
            // Smooth delay changes; SPIRAL deliberately bends the repeated pitch.
            s->delay+=(target-s->delay)*(Mode==2?.00015:.002);
            double oscillator=0;
            if constexpr(Mode>=6) {
                // Octave-spaced, crossfaded oscillators make a continuous
                // rising/falling Shepard sweep without a pitch reset click.
                const double direction=depth<.5?-1:1;
                for(int voice=0;voice<6;++voice) {
                    double octave=std::fmod(voice+direction*s->phase+6.0,6.0);
                    double hz=55*std::pow(2.0,octave);
                    s->oscillators[voice]=std::fmod(s->oscillators[voice]+hz/rate,1.0);
                    double wave=Mode==6?2*s->oscillators[voice]-1:1-4*std::abs(s->oscillators[voice]-.5);
                    double window=std::sin(3.141592653589793*octave/6);
                    oscillator+=wave*window*window/6;
                }
            }
            for(int ch=0;ch<2;++ch) {
                const double x=in[frame*2+ch]; double wet=x, write=x;
                if constexpr(Mode==0) {
                    s->low[ch]+=(x-s->low[ch])*(1-std::exp(-2*3.141592653589793*250/rate));
                    wet=read(s->delay,ch); write=x-s->low[ch]+wet*(.2+.55*depth);
                } else if constexpr(Mode==1) {
                    wet=.5*read(s->delay*.5,ch)+.3*read(s->delay*.75,1-ch)+.2*read(s->delay,ch);
                    write=x+wet*.35;
                } else if constexpr(Mode==2) {
                    wet=read(s->delay*(.85+.15*std::sin(6.283185307179586*s->phase)),ch);
                    s->low[ch]+=.25*(wet-s->low[ch]); write=x+std::tanh(s->low[ch])*(.3+.55*depth);
                } else if constexpr(Mode==3) {
                    double a=depth<.5?1-s->phase:s->phase, b=std::fmod(a+.5,1.0);
                    double w=std::pow(std::sin(3.141592653589793*a),2);
                    wet=x+.7*(w*read(rate*(.0005+.014*a),ch)+(1-w)*read(rate*(.0005+.014*b),ch));
                    write=x+.45*s->feedback[ch]; s->feedback[ch]=std::tanh(wet);
                } else if constexpr(Mode==4 || Mode==5) {
                    if(s->captured>=size_t(target)) {
                        if(!s->loopLength) s->loopRead=0;
                        s->loopLength=size_t(target); s->loopStart=0;
                    }
                    if(s->loopLength) {
                        const size_t length=s->loopLength;
                        const size_t cursor=s->loopRead%length;
                        wet=s->ring[2*((s->loopStart+cursor)%capacity)+ch];
                        // Short fade at repeat boundaries prevents hard clicks.
                        const double edge=std::min(1.0,double(std::min(cursor,length-1-cursor))/std::min(64.0,double(length)/4));
                        wet*=edge;
                    }
                } else { wet=x+oscillator*.35; }
                if constexpr(Mode==4 || Mode==5) {
                    // Retain the initial capture up to capacity so ROLL can
                    // expand its beat size as well as shrink it. SLIP ROLL
                    // restarts this capture on beat changes instead.
                    if(s->captured<capacity) s->ring[2*s->write+ch]=float(write);
                } else s->ring[2*s->write+ch]=float(std::clamp(write,-4.0,4.0));
                const double ramp=enabled==EffectEnableState::Enabling?double(frame+1)/p.framesPerBuffer():enabled==EffectEnableState::Disabling?1-double(frame+1)/p.framesPerBuffer():enabled==EffectEnableState::Disabled?0:1;
                out[frame*2+ch]=float(x+(wet-x)*ramp);
            }
            s->write=(s->write+1)%capacity; s->captured=std::min(capacity,s->captured+1); ++s->loopRead;
        }
    }
private:
    EngineEffectParameterPointer beats_, depth_;
};
