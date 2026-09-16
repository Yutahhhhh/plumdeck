#include "junction_input.h"
#include <samplerate.h>
#include <algorithm>
#include <cmath>
namespace junction {
namespace {
constexpr double kHalfPi=1.57079632679489661923;
static_assert(std::atomic<float>::is_always_lock_free,
        "JUNCTION audio samples must stay lock-free in realtime callbacks");
AsrcController::Config inputConfig() {
    AsrcController::Config config;
    config.targetFillFrames=JunctionInput::kTargetFill;
    config.deadbandFrames=882;
    config.maxRatioDeviation=.002;
    return config;
}
}
JunctionInput::JunctionInput():ring_(new std::atomic<float>[size_t(2*kOutputRate)*2]),capacity_(2*kOutputRate),controller_(inputConfig()) {
    for(size_t i=0;i<size_t(capacity_)*2;++i)ring_[i].store(0.f,std::memory_order_relaxed);
    int error=0;src_=src_new(SRC_SINC_FASTEST,2,&error);
}
JunctionInput::~JunctionInput() {if(src_)src_delete(src_);}

// ---- producer (session thread) --------------------------------------------

quint64 JunctionInput::fill() const {
    const auto w=writePos_.load(std::memory_order_acquire);
    const auto r=std::max(readPos_.load(std::memory_order_acquire),floorPos_.load(std::memory_order_acquire));
    return w>r?w-r:0;
}
void JunctionInput::publishMark(quint64 position,quint64 mediaFrame) noexcept {
    // Acquire on the odd transition keeps the following relaxed field stores
    // behind the "writer active" publication; the final release publishes
    // the completed tuple to readers that observed the next even value.
    markSeq_.fetch_add(1,std::memory_order_acq_rel);
    markPos_.store(position,std::memory_order_relaxed);
    markMedia_.store(mediaFrame,std::memory_order_relaxed);
    markGeneration_.store(resetGeneration_.load(std::memory_order_relaxed),std::memory_order_relaxed);
    markSeq_.fetch_add(1,std::memory_order_release);
    marked_.store(true,std::memory_order_release);
}
void JunctionInput::store(const float* samples,quint64 frames,quint64 mediaEnd) {
    if(!frames)return;
    // A block larger than the whole ring can only keep its tail; anything else
    // would publish a mark for frames that were overwritten before they existed.
    if(frames>capacity_){samples+=size_t(frames-capacity_)*2;frames=capacity_;}
    const auto w=writePos_.load(std::memory_order_relaxed);
    const auto end=w+frames;
    // Retire slots before touching them. A reader that was already copying is
    // detected by its second floor check below and replaces the block with a
    // fade; a new reader cannot enter the retired range.
    if(end>capacity_&&end-capacity_>floorPos_.load(std::memory_order_relaxed))
        floorPos_.store(end-capacity_,std::memory_order_release);
    for(quint64 i=0;i<frames;++i){
        const auto slot=size_t((w+i)%capacity_);
        ring_[slot*2].store(samples[size_t(i)*2],std::memory_order_relaxed);
        ring_[slot*2+1].store(samples[size_t(i)*2+1],std::memory_order_relaxed);
    }
    writePos_.store(end,std::memory_order_release);
    publishMark(end,mediaEnd);
}
void JunctionInput::write(const float* interleaved,const PcmBlockInfo& info) {
    if(!src_||!interleaved||!info.frameCount||!info.sampleRateHz||info.channels!=2)return;
    lastWriteAt_.store(monotonicNanos(),std::memory_order_relaxed);
    const bool continues=converting_&&info.epoch==sourceEpoch_&&info.generation==sourceGeneration_&&
            info.sampleRateHz==inputRate_&&info.mediaFrame==expectedMedia_;
    if(!continues){
        // Two mappings must never share the ring: restart the converter and
        // make everything behind the seam unreadable before the new audio lands.
        src_reset(src_);controller_.reset();
        marked_.store(false,std::memory_order_release);
        floorPos_.store(writePos_.load(std::memory_order_acquire),std::memory_order_release);
        resetGeneration_.fetch_add(1,std::memory_order_acq_rel);
        inputRate_=info.sampleRateHz;sourceEpoch_=info.epoch;sourceGeneration_=info.generation;converting_=true;
    }
    expectedMedia_=info.mediaFrame+mediaFrameAdvance(info.frameCount,info.sampleRateHz);
    const double ratio=double(kOutputRate)/double(inputRate_)*controller_.update(double(fill()));
    converted_.resize((size_t(double(info.frameCount)*ratio*1.02)+64)*2);
    SRC_DATA data{};
    data.data_in=interleaved;data.input_frames=long(info.frameCount);
    data.data_out=converted_.data();data.output_frames=long(converted_.size()/2);
    data.src_ratio=ratio;
    if(src_process(src_,&data)||data.output_frames_gen<=0)return;
    // The mark is the media frame just past the *input actually consumed*, at
    // the ring position just past the frames it produced.
    store(converted_.data(),quint64(data.output_frames_gen),
            info.mediaFrame+mediaFrameAdvance(quint64(data.input_frames_used),info.sampleRateHz));
}
void JunctionInput::reset() {
    if(src_)src_reset(src_);
    controller_.reset();
    converting_=false;inputRate_=0;sourceEpoch_=0;sourceGeneration_=0;expectedMedia_=0;
    marked_.store(false,std::memory_order_release);
    floorPos_.store(writePos_.load(std::memory_order_acquire),std::memory_order_release);
    resetGeneration_.fetch_add(1,std::memory_order_acq_rel);
}
void JunctionInput::requestAlign(qint64 frames) noexcept {
    pendingAlign_.store(std::clamp(frames,-kMaxAlignFrames,kMaxAlignFrames),std::memory_order_release);
}
quint64 JunctionInput::latencyFrames48k() const {return mediaFrameAdvance(fill(),kOutputRate);}
std::optional<quint64> JunctionInput::renderedMediaFrame() const {
    if(!rendering_.load(std::memory_order_acquire))return std::nullopt;
    return renderedMedia_.load(std::memory_order_acquire);
}
bool JunctionInput::receiving() const {
    const auto at=lastWriteAt_.load(std::memory_order_relaxed);
    return at&&monotonicNanos()-at<250000000LL;
}

// ---- reader (audio thread) -------------------------------------------------

void JunctionInput::refreshMark() noexcept {
    for(int attempt=0;attempt<4;++attempt){
        const auto before=markSeq_.load(std::memory_order_acquire);
        if(before&1)continue;
        const auto position=markPos_.load(std::memory_order_relaxed);
        const auto media=markMedia_.load(std::memory_order_relaxed);
        const auto generation=markGeneration_.load(std::memory_order_relaxed);
        if(markSeq_.load(std::memory_order_acquire)!=before)continue;
        if(!marked_.load(std::memory_order_acquire)||generation!=readerGeneration_){readMarked_=false;return;}
        readMarkPos_=position;readMarkMedia_=media;readMarked_=true;return;
    }
    // Contended: keep the previous mark for this block rather than a torn one.
}
std::optional<quint64> JunctionInput::mediaAt(quint64 position) const noexcept {
    if(!readMarked_)return std::nullopt;
    if(position<=readMarkPos_){
        const auto back=mediaFrameAdvance(readMarkPos_-position,kOutputRate);
        if(back>readMarkMedia_)return std::nullopt;
        return readMarkMedia_-back;
    }
    return readMarkMedia_+mediaFrameAdvance(position-readMarkPos_,kOutputRate);
}
bool JunctionInput::readable(quint64 position,unsigned frames,quint64 floor,quint64 write) const noexcept {
    return position>=floor&&write>=position&&write-position>=frames;
}
unsigned JunctionInput::crossfade(float* out,quint64 from,quint64 to,unsigned frames) noexcept {
    for(unsigned i=0;i<frames;++i){
        const double t=double(i)/double(frames);
        const float out_gain=float(std::cos(t*kHalfPi)),in_gain=float(std::sin(t*kHalfPi));
        const auto a=size_t((from+i)%capacity_),b=size_t((to+i)%capacity_);
        out[i*2]=ring_[a*2].load(std::memory_order_relaxed)*out_gain+ring_[b*2].load(std::memory_order_relaxed)*in_gain;
        out[i*2+1]=ring_[a*2+1].load(std::memory_order_relaxed)*out_gain+ring_[b*2+1].load(std::memory_order_relaxed)*in_gain;
    }
    return frames;
}
unsigned JunctionInput::jump(float* out,quint64 from,quint64 to,unsigned frames,quint64 floor,quint64 write) noexcept {
    const unsigned span=unsigned(std::min<quint64>(kFadeFrames,frames));
    if(!span||!readable(to,span,floor,write))return 0;
    // The old position may already have been overwritten; then there is nothing
    // to read. Fade the last sample actually emitted to zero, then fade the
    // current stream in; this avoids both a callback-boundary step and a large
    // phase jump between unrelated points in the waveform.
    if(!readable(from,span,floor,write)){
        const unsigned total=std::min(frames,2*kFadeFrames),down=total/2,up=total-down;
        if(!down||!up||!readable(to,total,floor,write)){fadeIn_=true;return 0;}
        for(unsigned i=0;i<down;++i){
            const float gain=float(down-i)/float(down);
            out[i*2]=tailLeft_*gain;out[i*2+1]=tailRight_*gain;
        }
        for(unsigned i=0;i<up;++i){
            const float gain=float(i)/float(up);
            const auto slot=size_t((to+down+i)%capacity_);
            out[(down+i)*2]=ring_[slot*2].load(std::memory_order_relaxed)*gain;
            out[(down+i)*2+1]=ring_[slot*2+1].load(std::memory_order_relaxed)*gain;
        }
        fadeIn_=false;return total;
    }
    return crossfade(out,from,to,span);
}
void JunctionInput::copyOut(float* out,quint64 position,unsigned frames) noexcept {
    for(unsigned i=0;i<frames;++i){
        const auto slot=size_t((position+i)%capacity_);
        out[i*2]=ring_[slot*2].load(std::memory_order_relaxed);out[i*2+1]=ring_[slot*2+1].load(std::memory_order_relaxed);
    }
}
void JunctionInput::starve(float* out,unsigned frames) noexcept {
    const unsigned ramp=std::min(frames,kFadeFrames);
    for(unsigned i=0;i<ramp;++i){
        const float gain=float(ramp-i-1)/float(ramp);
        out[i*2]=tailLeft_*gain;out[i*2+1]=tailRight_*gain;
    }
    if(frames>ramp)std::fill_n(out+size_t(ramp)*2,size_t(frames-ramp)*2,0.f);
    tailLeft_=tailRight_=0.f;
}
void JunctionInput::read(float* out,unsigned frames) noexcept {
    const auto generation=resetGeneration_.load(std::memory_order_acquire);
    if(generation!=readerGeneration_){
        readerGeneration_=generation;readMarked_=false;primed_=false;
        rendering_.store(false,std::memory_order_release);
        readPos_.store(floorPos_.load(std::memory_order_acquire),std::memory_order_release);
        // A request aimed at the old stream must not survive the seam.
        pendingAlign_.store(0,std::memory_order_relaxed);
    }
    if(!frames)return;
    refreshMark();
    const auto w=writePos_.load(std::memory_order_acquire),floor=floorPos_.load(std::memory_order_acquire);
    const auto previous=readPos_.load(std::memory_order_relaxed);
    const bool overtaken=primed_&&previous<floor;
    auto r=std::max(previous,floor);
    if(!primed_){
        if(!(w>r&&w-r>=kTargetFill)){
            rendering_.store(false,std::memory_order_release);
            starve(out,frames);return;
        }
        r=std::max(w-kTargetFill,floor);
        primed_=true;fadeIn_=true;
    }
    quint64 target=r;
    bool moved=overtaken;
    if(const qint64 align=pendingAlign_.exchange(0,std::memory_order_acq_rel);align){
        const auto shifted=align>0?target+quint64(align)
                                  :(target>quint64(-align)?target-quint64(-align):0);
        target=std::clamp(shifted,floor,w);
        moved=target!=r;
    }
    if(w>target&&w-target>kTargetFill+kMaxExcess){target=w-kTargetFill;moved=true;}
    if(target<floor){target=floor;moved=true;}
    unsigned produced=0;
    quint64 cursor=target;
    if(moved){
        realignments_.fetch_add(1,std::memory_order_relaxed);
        produced=jump(out,overtaken?previous:r,target,frames,floor,w);
        cursor=target+produced;
    }
    if(const auto media=mediaAt(target);media)renderedMedia_.store(*media,std::memory_order_release);
    const quint64 available=w>cursor?w-cursor:0;
    const unsigned n=unsigned(std::min<quint64>(available,frames-produced));
    copyOut(out+size_t(produced)*2,cursor,n);
    if(fadeIn_&&n){
        const unsigned ramp=std::min(n,kFadeFrames);
        for(unsigned i=0;i<ramp;++i){
            const float gain=float(i)/float(ramp);
            out[(produced+i)*2]*=gain;out[(produced+i)*2+1]*=gain;
        }
        fadeIn_=false;
    }
    produced+=n;cursor+=n;
    const auto latestFloor=floorPos_.load(std::memory_order_acquire);
    if(latestFloor>target){
        // The producer wrapped while this callback copied. Never emit a mix of
        // old and newly overwritten samples; spend this callback fading the
        // previously emitted tail and re-prime from the current stream.
        rendering_.store(false,std::memory_order_release);primed_=false;
        realignments_.fetch_add(1,std::memory_order_relaxed);
        starve(out,frames);readPos_.store(latestFloor,std::memory_order_release);return;
    }
    if(produced){
        tailLeft_=out[size_t(produced-1)*2];tailRight_=out[size_t(produced-1)*2+1];
        rendering_.store(mediaAt(target).has_value(),std::memory_order_release);
    } else rendering_.store(false,std::memory_order_release);
    if(produced<frames){
        starve(out+size_t(produced)*2,frames-produced);
        primed_=false;underruns_.fetch_add(1,std::memory_order_relaxed);
    }
    readPos_.store(cursor,std::memory_order_release);
}
}
