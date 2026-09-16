#include "harness.h"
#include "junction/junction_input.h"
#include <algorithm>
#include <cmath>
#include <optional>
#include <vector>
using namespace junction;
namespace {
constexpr unsigned kWire=kWireSampleRate;

/// Feeds the deck the way a decoded network stream does: contiguous blocks on
/// one epoch/generation, each labelled with the media frame of its first sample.
struct Feeder {
    quint64 epoch=7,generation=3,media=0,source=0,sequence=0,phase=0;
    unsigned rate=kWire;
    void push(JunctionInput& input,unsigned frames) {
        std::vector<float> pcm(size_t(frames)*2);
        for(unsigned i=0;i<frames;++i){
            const float v=.5f*float(std::sin(2*3.14159265358979*440*double(phase+i)/double(rate)));
            pcm[size_t(i)*2]=v;pcm[size_t(i)*2+1]=v;
        }
        phase+=frames;
        PcmBlockInfo info;
        info.epoch=epoch;info.generation=generation;info.mediaFrame=media;info.sourceFrame=source;
        info.sequence=sequence++;info.frameCount=frames;info.sampleRateHz=rate;info.channels=2;
        input.write(pcm.data(),info);
        source+=frames;media+=mediaFrameAdvance(frames,rate);
    }
    /// Media frame just past everything written so far.
    quint64 mediaEnd() const {return media;}
};

float peak(const float* pcm,unsigned frames) {
    float p=0;for(unsigned i=0;i<frames*2;++i)p=std::max(p,std::abs(pcm[i]));return p;
}
/// Largest sample-to-sample step, carried across callback boundaries. A click
/// is exactly a step much larger than the signal's own slew rate.
struct StepMeter {
    float left=0,right=0,worst=0;bool started=false;
    void feed(const float* pcm,unsigned frames) {
        for(unsigned i=0;i<frames;++i){
            if(started){
                worst=std::max(worst,std::abs(pcm[i*2]-left));
                worst=std::max(worst,std::abs(pcm[i*2+1]-right));
            }
            left=pcm[i*2];right=pcm[i*2+1];started=true;
        }
    }
};
// A 440 Hz tone at 0.5 amplitude slews at most ~0.032 per frame at 44.1 kHz;
// a 64-frame equal-power seam adds at most ~0.025. Anything past this is a click.
constexpr float kClickThreshold=.08f;

// Twenty minutes at 100 ppm is 120 ms of raw drift: without the ASRC
// controller the slow case drains the whole nominal buffer and starves, and
// the fast case walks away from the target for good.
constexpr quint64 kDriftBand=2880;   // 60 ms either side of nominal.
constexpr quint64 kSettledBand=960;  // 20 ms once the controller has converged.
struct DriftRun {
    quint64 minLatency=~0ULL,maxLatency=0,underruns=0,realignments=0,mediaEnd=0;
    std::optional<quint64> rendered;
    quint64 latencyAtEnd=0;
};
/// Plays `seconds` of a peer whose clock runs `ppm` fast (negative: slow),
/// pulling 512-frame callbacks exactly as the device would.
DriftRun runDrift(double ppm,double seconds) {
    JunctionInput input;Feeder feeder;DriftRun run;
    constexpr unsigned kCallback=512,kBlock=960;
    // Wire frames the peer has produced per output frame we have consumed.
    const double scale=double(kWire)/double(JunctionInput::kOutputRate)*(1.0+ppm/1e6);
    const double preload=double(JunctionInput::kNominalLatency48k)+kBlock;
    std::vector<float> out(size_t(kCallback)*2);
    quint64 outFrames=0;
    const quint64 total=quint64(seconds*JunctionInput::kOutputRate);
    while(outFrames<total){
        while(double(feeder.source)<double(outFrames)*scale+preload)feeder.push(input,kBlock);
        input.read(out.data(),kCallback);
        outFrames+=kCallback;
        if(outFrames>JunctionInput::kOutputRate){
            const auto latency=input.latencyFrames48k();
            run.minLatency=std::min(run.minLatency,latency);
            run.maxLatency=std::max(run.maxLatency,latency);
        }
    }
    run.latencyAtEnd=input.latencyFrames48k();
    run.rendered=input.renderedMediaFrame();
    run.underruns=input.underruns();run.realignments=input.realignments();
    run.mediaEnd=feeder.mediaEnd();
    return run;
}
}

JTEST("junction-input","stays silent until primed, then plays, then counts a starved callback") {
    JunctionInput input;Feeder feeder;std::vector<float> out(512*2);
    input.read(out.data(),512);CHECK_EQ(peak(out.data(),512),0.f);
    CHECK(!input.renderedMediaFrame().has_value());
    for(int n=0;n<8;++n)feeder.push(input,960);
    CHECK(input.receiving());
    input.read(out.data(),512);CHECK(peak(out.data(),512)>0.f);
    CHECK(input.renderedMediaFrame().has_value());
    CHECK(input.latencyFrames48k()>0ULL);
    std::vector<float> drain(size_t(16384)*2);input.read(drain.data(),16384);
    CHECK_EQ(input.underruns(),1ULL);
    input.read(out.data(),512);CHECK_EQ(peak(out.data(),512),0.f);
    CHECK(!input.renderedMediaFrame().has_value());
}

JTEST("junction-input","reset drops buffered audio and waits for a new prime") {
    JunctionInput input;Feeder feeder;std::vector<float> out(512*2);
    for(int n=0;n<8;++n)feeder.push(input,960);
    input.reset();
    input.read(out.data(),512);CHECK_EQ(peak(out.data(),512),0.f);
    for(int n=0;n<8;++n)feeder.push(input,960);
    input.read(out.data(),512);CHECK(peak(out.data(),512)>0.f);
}

JTEST("junction-input","a new stream generation never shares the ring with the old one") {
    JunctionInput input;Feeder feeder;std::vector<float> out(512*2);
    for(int n=0;n<8;++n)feeder.push(input,960);
    input.read(out.data(),512);
    const auto before=input.renderedMediaFrame();CHECK(before.has_value());
    // A reconnect: new generation, media timeline jumps forward.
    feeder.generation=4;feeder.media+=kWire*30;feeder.sequence=0;
    input.read(out.data(),512);
    for(int n=0;n<8;++n)feeder.push(input,960);
    input.read(out.data(),512);
    const auto after=input.renderedMediaFrame();
    CHECK(after.has_value());
    CHECK(*after>*before+kWire*20);
}

JTEST("junction-input","the rendered media frame maps exactly onto the session timeline") {
    JunctionInput input;Feeder feeder;std::vector<float> out(512*2);
    for(int n=0;n<8;++n)feeder.push(input,960);
    input.read(out.data(),512);  // primes at the target delay
    for(int block=0;block<6;++block){
        feeder.push(input,960);
        // Latency is the distance from the mark the producer published, so the
        // frame the next callback renders is fixed exactly, not approximately.
        const quint64 latency=input.latencyFrames48k();
        const quint64 expected=feeder.mediaEnd()-latency;
        input.read(out.data(),512);
        CHECK_EQ(input.realignments(),0ULL);
        CHECK_EQ(input.underruns(),0ULL);
        const auto rendered=input.renderedMediaFrame();
        CHECK(rendered.has_value());
        CHECK_EQ(*rendered,expected);
        // And it must stay inside the band a real seam could ever occupy.
        CHECK(feeder.mediaEnd()-*rendered<JunctionInput::kMaxSeamLag48k);
    }
}

JTEST("junction-input","a starved callback with nothing available fades out instead of clicking") {
    JunctionInput input;Feeder feeder;std::vector<float> out(512*2);
    for(int n=0;n<8;++n)feeder.push(input,960);
    input.read(out.data(),512);
    // Drain the ring to exactly empty, then ask for a block with zero frames
    // available: the whole callback is the tail of the previous one.
    std::vector<float> drain(size_t(16384)*2);
    input.read(drain.data(),16384);
    CHECK_EQ(input.underruns(),1ULL);
    StepMeter meter;
    input.read(out.data(),512);
    meter.feed(out.data(),512);
    CHECK_EQ(input.underruns(),1ULL);          // an empty hold is not a new underrun
    CHECK_EQ(peak(out.data(),512),0.f);        // the tail was already spent
    CHECK(meter.worst<kClickThreshold);
    // Priming again must come back through a fade, not a step.
    for(int n=0;n<8;++n)feeder.push(input,960);
    input.read(out.data(),512);
    meter.feed(out.data(),512);
    CHECK(peak(out.data(),512)>0.f);
    CHECK(meter.worst<kClickThreshold);
}

JTEST("junction-input","flooding and pausing the stream never clicks") {
    JunctionInput input;Feeder feeder;std::vector<float> out(512*2);StepMeter meter;
    for(int n=0;n<8;++n)feeder.push(input,960);
    for(int n=0;n<4;++n){input.read(out.data(),512);meter.feed(out.data(),512);feeder.push(input,960);}
    CHECK_EQ(input.realignments(),0ULL);
    // A stalled session thread dumps a burst: far past target+250 ms.
    for(int n=0;n<60;++n)feeder.push(input,960);
    input.read(out.data(),512);meter.feed(out.data(),512);
    CHECK_EQ(input.realignments(),1ULL);
    CHECK(input.latencyFrames48k()<quint64(JunctionInput::kTargetFill+JunctionInput::kMaxExcess)*kWire/JunctionInput::kOutputRate);
    CHECK(peak(out.data(),512)>0.f);
    // Then the peer pauses: several starved callbacks, then it comes back.
    for(int n=0;n<40;++n){input.read(out.data(),512);meter.feed(out.data(),512);}
    CHECK(input.underruns()>=1ULL);
    for(int n=0;n<8;++n)feeder.push(input,960);
    for(int n=0;n<8;++n){input.read(out.data(),512);meter.feed(out.data(),512);feeder.push(input,960);}
    CHECK(peak(out.data(),512)>0.f);
    CHECK(meter.worst<kClickThreshold);
}

JTEST("junction-input","a reader paused past the whole ring never emits stale overwritten audio") {
    JunctionInput input;Feeder feeder;std::vector<float> out(512*2);StepMeter meter;
    for(int n=0;n<8;++n)feeder.push(input,960);
    input.read(out.data(),512);meter.feed(out.data(),512);
    const auto before=input.renderedMediaFrame();CHECK(before.has_value());
    // More than the fixed two-second capacity arrives while the callback is
    // paused. On wake the reader must jump to the current target, not replay
    // the region the producer has overwritten under the old cursor.
    for(int n=0;n<130;++n)feeder.push(input,960);
    input.read(out.data(),512);meter.feed(out.data(),512);
    const auto after=input.renderedMediaFrame();CHECK(after.has_value());
    CHECK(*after>*before+kWire);
    CHECK(input.realignments()>=1ULL);
    CHECK(meter.worst<kClickThreshold);
}

JTEST("junction-input","a one-shot alignment request is crossfaded and never inherited") {
    JunctionInput input;Feeder feeder;std::vector<float> out(512*2);StepMeter meter;
    for(int n=0;n<20;++n)feeder.push(input,960);
    input.read(out.data(),512);meter.feed(out.data(),512);
    const auto before=input.renderedMediaFrame();CHECK(before.has_value());
    input.requestAlign(-441);  // 10 ms back
    input.read(out.data(),512);meter.feed(out.data(),512);
    CHECK_EQ(input.realignments(),1ULL);
    const auto moved=input.renderedMediaFrame();CHECK(moved.has_value());
    // Without the request the next callback would land 512 output frames on:
    // the request pulled it back to roughly 71.
    CHECK(*moved>*before);
    CHECK(*moved<*before+300);
    CHECK(meter.worst<kClickThreshold);
    // The request is consumed: the next callbacks move on normally.
    for(int n=0;n<4;++n){input.read(out.data(),512);meter.feed(out.data(),512);}
    CHECK_EQ(input.realignments(),1ULL);
    CHECK(meter.worst<kClickThreshold);
    // Clamped, and dropped at a seam rather than inherited by the next stream.
    input.requestAlign(1000000);
    input.reset();
    for(int n=0;n<8;++n)feeder.push(input,960);
    input.read(out.data(),512);
    CHECK_EQ(input.realignments(),1ULL);
}

JTEST("junction-input","a peer clock 100 ppm fast keeps the buffer bounded for a long set") {
    const DriftRun run=runDrift(100.0,1200.0);
    CHECK_EQ(run.underruns,0ULL);
    CHECK_EQ(run.realignments,0ULL);
    CHECK(run.maxLatency<JunctionInput::kNominalLatency48k+kDriftBand);
    CHECK(run.minLatency>JunctionInput::kNominalLatency48k-kDriftBand);
    // The controller does not merely stay in range, it pulls back to target.
    CHECK_NEAR(run.latencyAtEnd,JunctionInput::kNominalLatency48k,kSettledBand);
    CHECK(run.rendered.has_value());
    const quint64 lag=run.mediaEnd-*run.rendered;
    CHECK(lag<JunctionInput::kMaxSeamLag48k);
    // The rendered frame sits exactly one callback ahead of the buffered delay
    // measured after that callback -- the mapping did not drift with the clock.
    CHECK(lag>run.latencyAtEnd);
    CHECK(lag-run.latencyAtEnd<2*mediaFrameAdvance(512,JunctionInput::kOutputRate));
}

JTEST("junction-input","a peer clock 100 ppm slow keeps the buffer bounded for a long set") {
    const DriftRun run=runDrift(-100.0,1200.0);
    CHECK_EQ(run.underruns,0ULL);
    CHECK_EQ(run.realignments,0ULL);
    CHECK(run.maxLatency<JunctionInput::kNominalLatency48k+kDriftBand);
    CHECK(run.minLatency>JunctionInput::kNominalLatency48k-kDriftBand);
    CHECK_NEAR(run.latencyAtEnd,JunctionInput::kNominalLatency48k,kSettledBand);
    CHECK(run.rendered.has_value());
    CHECK(run.mediaEnd-*run.rendered<JunctionInput::kMaxSeamLag48k);
    CHECK(run.mediaEnd-*run.rendered>run.latencyAtEnd);
}

JTEST("junction-input","a takeover at zero volume never auto-releases until heard and then faded") {
    constexpr qint64 s=1000000000LL;InputRelease release;release.begin(0);
    // Stale controls: the deck was already silent when the operator took over.
    for(qint64 t=0;t<=10*s;t+=s/10)CHECK(!release.observe(false,true,t));
    CHECK(release.active());
    // The operator hears the outgoing DJ, then fades them out.
    CHECK(!release.observe(true,true,10*s));
    CHECK(!release.observe(false,true,10*s+1));
    CHECK(!release.observe(false,true,11*s));
    CHECK(release.observe(false,true,10*s+1+InputRelease::kSilentNanos));
}

JTEST("junction-input","raising the deck again restarts the release fade") {
    constexpr qint64 s=1000000000LL;InputRelease release;release.begin(0);
    CHECK(!release.observe(true,true,0));CHECK(!release.observe(false,true,s));
    CHECK(!release.observe(true,true,2*s));CHECK(!release.observe(false,true,2*s+s/10));
    CHECK(!release.observe(false,true,3*s));
    CHECK(release.observe(false,true,2*s+s/10+InputRelease::kSilentNanos));
    release.clear();CHECK(!release.active());CHECK(!release.observe(false,true,10*s));
}

JTEST("junction-input","a stream that stops arriving releases without waiting for a fade") {
    constexpr qint64 s=1000000000LL;InputRelease release;release.begin(s);
    CHECK(!release.observe(true,false,2*s));             // first sighting arms the clock
    CHECK(!release.observe(true,false,3*s));
    CHECK(!release.observe(true,false,2*s+InputRelease::kStreamEndedNanos-1));
    CHECK(release.observe(true,false,2*s+InputRelease::kStreamEndedNanos));
    // And a hold that outlasts any plausible mix ends on its own, even while
    // the outgoing DJ is still audible.
    InputRelease held;held.begin(s);
    CHECK(!held.observe(true,true,10*s));
    CHECK(held.observe(true,true,s+InputRelease::kMaxHoldNanos));
}

JTEST("junction-input","the rendered frame is exact for a 44.1 kHz source too") {
    // The wire is 48 kHz, but nothing in the mapping may assume it: a source
    // already at the output rate goes through the same mark arithmetic, and a
    // naive frame count would be 8.8% wrong the other way.
    JunctionInput input;Feeder feeder;feeder.rate=JunctionInput::kOutputRate;
    std::vector<float> out(512*2);
    for(int n=0;n<8;++n)feeder.push(input,882);
    input.read(out.data(),512);
    for(int block=0;block<6;++block){
        feeder.push(input,882);
        const quint64 expected=feeder.mediaEnd()-input.latencyFrames48k();
        input.read(out.data(),512);
        const auto rendered=input.renderedMediaFrame();
        CHECK(rendered.has_value());
        CHECK_EQ(*rendered,expected);
    }
    CHECK_EQ(input.underruns(),0ULL);
    CHECK_EQ(input.realignments(),0ULL);
}

JTEST("junction-input","the takeover anchor is the frame the deck rendered, not the clock") {
    // What the deck rendered is one buffer behind where the local clock puts
    // the callback. The seam is the former; the latter is only a sanity band.
    constexpr quint64 kElapsed=48000*600;
    const quint64 rendered=kElapsed-JunctionInput::kNominalLatency48k;
    CHECK_EQ(TakeoverAnchor::resolve(rendered,kElapsed),rendered);
    // Nothing rendered: the deck was starved, so there is no remote audio in
    // this block to align to and the clock is all that is left.
    CHECK_EQ(TakeoverAnchor::resolve(std::nullopt,kElapsed),kElapsed);
    // Inside the band, including its two edges.
    CHECK_EQ(TakeoverAnchor::resolve(kElapsed-JunctionInput::kMaxSeamLag48k,kElapsed),kElapsed-JunctionInput::kMaxSeamLag48k);
    CHECK_EQ(TakeoverAnchor::resolve(kElapsed+JunctionInput::kMaxSeamLead48k,kElapsed),kElapsed+JunctionInput::kMaxSeamLead48k);
    // Outside it the report is a bug or a stale stream, never a seam.
    CHECK_EQ(TakeoverAnchor::resolve(kElapsed-JunctionInput::kMaxSeamLag48k-1,kElapsed),kElapsed);
    CHECK_EQ(TakeoverAnchor::resolve(kElapsed+JunctionInput::kMaxSeamLead48k+1,kElapsed),kElapsed);
    CHECK_EQ(TakeoverAnchor::resolve(0,kElapsed),kElapsed);
}

JTEST("junction-input","a live deck hands the takeover an anchor inside the seam band") {
    // End to end on the reader: whatever the deck reports in a callback is
    // accepted as the seam, and it is behind the clock by the buffered delay
    // rather than by an unbounded amount.
    JunctionInput input;Feeder feeder;std::vector<float> out(512*2);
    for(int n=0;n<8;++n)feeder.push(input,960);
    input.read(out.data(),512);
    // The session clock at this callback: the peer's newest frame plus the
    // network delay it took to arrive. The deck is rendering further back.
    const quint64 elapsed=feeder.mediaEnd()+kWire/50;
    const auto rendered=input.renderedMediaFrame();
    CHECK(rendered.has_value());
    CHECK_EQ(TakeoverAnchor::resolve(rendered,elapsed),*rendered);
    CHECK(elapsed-*rendered>input.latencyFrames48k());
    // The old wall-clock seam would have started Program here instead, tens of
    // milliseconds past the audio actually being rendered.
    CHECK(elapsed-*rendered>kWire/100);
}
