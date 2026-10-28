// Offline conformance for the actual DSP used by the Mixxx host. No devices.
#include "../src/ddj_beat_processors.h"
#include "../src/beat_fx.h"
#include <cstdlib>
#include <iostream>
#include <stdexcept>
#include <string>

namespace {
void require(bool value, const char* message) {
    if (!value) throw std::runtime_error(message);
}
void close(double actual, double expected, double tolerance, const char* message) {
    require(std::abs(actual - expected) <= tolerance, message);
}

template<int Mode> void dryAtTransitions() {
    constexpr int frames = 64;
    const mixxx::EngineParameters p(mixxx::audio::SampleRate(8000), frames);
    DdjBeatState state(p);
    GroupFeatureState features;
    features.beat_length = GroupFeatureBeatLength{.5, 1};
    std::array<float, frames * 2> in{}, out{};
    for (int frame=0; frame<frames; ++frame) {
        in[2*frame] = float(.2 * std::sin(frame * .2));
        in[2*frame+1] = float(.15 * std::cos(frame * .17));
    }
    for (auto enabled : {EffectEnableState::Enabling, EffectEnableState::Enabled, EffectEnableState::Disabling}) {
        DdjBeatProcessor<Mode>::renderState(&state, in.data(), out.data(), p, enabled, features, 4, 1);
        for (size_t i=0; i<out.size(); ++i) close(out[i], in[i], 1e-7, "dry signal changed before the first delay tap");
    }
}

// An impulse identifies the true delay in samples, independently of a UI value.
void echoImpulse(int rate, double beatSeconds, double beats) {
    constexpr int frames = 128;
    const mixxx::EngineParameters p(mixxx::audio::SampleRate(rate), frames);
    DdjBeatState state(p);
    GroupFeatureState features;
    features.beat_length = GroupFeatureBeatLength{beatSeconds, 1};
    const auto expectedFrame = size_t(std::llround(beatSeconds * beats * rate));
    std::array<float, frames * 2> in{}, out{};
    // The send fades in over ~10 ms; let it settle before the impulse.
    const size_t settle = size_t(rate * .3 / frames + 1) * frames;
    for (size_t start=0; start<settle; start+=frames)
        DdjBeatProcessor<8>::renderState(&state, in.data(), out.data(), p,
            start ? EffectEnableState::Enabled : EffectEnableState::Enabling, features, beats, .5, 1, .5);
    size_t firstReturn = 0;
    double returnLevel = 0;
    for (size_t start=0; start<expectedFrame+frames; start+=frames) {
        in.fill(0);
        if (!start) in[0] = in[1] = .25;
        // Release send after the impulse block: the buffered impulse must survive.
        DdjBeatProcessor<8>::renderState(&state, in.data(), out.data(), p,
            EffectEnableState::Enabled, features, beats, .5, start ? 0 : 1, .5);
        if (!start) close(out[0], .25, 1e-7, "echo removed the initial dry impulse");
        for (int frame=0; frame<frames; ++frame) {
            if (start+frame && std::abs(out[2*frame]) > .01 && !firstReturn) {
                firstReturn = start+frame; returnLevel = out[2*frame];
            }
        }
    }
    require(firstReturn == expectedFrame, "echo delay did not match the requested beat interval");
    close(returnLevel, .25, 1e-6, "echo return gain changed");
    in.fill(0);
    DdjBeatProcessor<8>::renderState(&state, in.data(), out.data(), p,
        EffectEnableState::Enabling, features, beats, .5, 1, .5);
    for (auto sample : out) close(sample, 0, 1e-7, "re-enabling echo leaked its previous delay buffer");
    std::cout << "echo " << rate << " Hz, " << beats << " beats, " << beatSeconds
              << " s/beat: " << firstReturn << " frames\n";
}

// The send fades in from zero when the effect turns on, so the first repeat
// does not start with a click.
void sendFadesIn() {
    constexpr int frames=64, rate=8000;
    const mixxx::EngineParameters p(mixxx::audio::SampleRate(rate), frames);
    DdjBeatState state(p);
    GroupFeatureState features;
    features.beat_length=GroupFeatureBeatLength{.5, 1};
    std::array<float, frames*2> in{}, out{};
    in.fill(.1f);
    DdjBeatProcessor<8>::renderState(&state,in.data(),out.data(),p,EffectEnableState::Enabling,features,1,.5,1,0);
    require(state.send > .3 && state.send < .7, "send did not ramp over ~10 ms");
}

// Closing the send removes new input from the delay but preserves its old tail.
template<int Mode> void releaseSend() {
    constexpr int frames=64, rate=8000;
    const mixxx::EngineParameters p(mixxx::audio::SampleRate(rate), frames);
    DdjBeatState released(p), reference(p);
    GroupFeatureState features;
    features.beat_length=GroupFeatureBeatLength{.5, 1};
    std::array<float, frames*2> in{}, silence{}, out{}, expected{};
    bool tail=false;
    // 40 blocks (320 ms) of send fade-in, the impulse, then the send closes
    // and fades out over 25 blocks (200 ms) of silence; then fresh input.
    for (int block=0; block<240; ++block) {
        const bool fresh = block >= 66;
        in.fill(fresh ? .1f : 0);
        silence.fill(0);
        if (block == 40) in[0]=in[1]=silence[0]=silence[1]=.25f;
        const auto enabled=block ? EffectEnableState::Enabled : EffectEnableState::Enabling;
        const double send = block <= 40 ? 1 : 0;
        DdjBeatProcessor<Mode>::renderState(&released,in.data(),out.data(),p,enabled,features,1,.5,send);
        DdjBeatProcessor<Mode>::renderState(&reference,silence.data(),expected.data(),p,enabled,features,1,.5,send);
        if (!fresh) continue;
        for (size_t i=0;i<out.size();++i) {
            close(out[i]-in[i],expected[i],1e-6,"closing the send admitted fresh signal or removed the tail");
            if (std::abs(expected[i])>.001) tail=true;
        }
    }
    require(tail,"delay tail was silent");
}

void longRoll() {
    constexpr int frames = 128, rate = 8000;
    const mixxx::EngineParameters p(mixxx::audio::SampleRate(rate), frames);
    DdjBeatState state(p);
    GroupFeatureState features;
    features.beat_length = GroupFeatureBeatLength{1, 1};
    std::array<float, frames * 2> in{}, out{};
    in.fill(.25);
    // 16 beats at 60 BPM: the longest roll the 16 s ring holds.
    constexpr size_t expected = rate * 16;
    for (size_t start=0; start<=expected; start+=frames) {
        DdjBeatProcessor<5>::renderState(&state, in.data(), out.data(), p,
            start ? EffectEnableState::Enabled : EffectEnableState::Enabling, features, 16, .5);
    }
    require(state.loopLength == expected, "16-beat roll at 60 BPM was truncated");
    close(out[2*100], .25, 1e-6, "roll did not replay the captured signal");
}

void shortBufferModulation() {
    constexpr int frames = 128, rate = 8000;
    const mixxx::EngineParameters p(mixxx::audio::SampleRate(rate), frames);
    DdjBeatState state(p, .02);
    require(state.ring.size() < 400, "modulation effect allocated a long delay buffer");
    GroupFeatureState features;
    features.beat_length = GroupFeatureBeatLength{3, 1};
    std::array<float, frames * 2> in{}, out{};
    DdjBeatProcessor<3>::renderState(&state, in.data(), out.data(), p,
        EffectEnableState::Enabling, features, 16, .5);
    close(state.phase, double(frames)/(rate*48), 1e-12, "short ring incorrectly limited the LFO beat period");
}
}

int main() {
    try {
        dryAtTransitions<0>(); dryAtTransitions<1>(); dryAtTransitions<2>(); dryAtTransitions<8>();
        echoImpulse(44100, .5, 4);
        echoImpulse(48000, .5, 1.0/16);
        echoImpulse(8000, 1, 16);
        sendFadesIn();
        releaseSend<0>(); releaseSend<1>(); releaseSend<2>(); releaseSend<8>();
        longRoll(); shortBufferModulation();
        for (double level : {0.0, .25, .5, 1.0}) {
            close(BeatFx::chainMix("pitchshift", level), 1, 0, "PITCH must remain fully wet at every transposition");
            close(BeatFx::chainMix("echo", level), level, 0, "delay LEVEL must retain its mix control");
        }
        std::cout << "PASS: dry preservation, fractional/long echo, send fade, release tail, reset, long roll, LFO period, PITCH mix\n";
    } catch (const std::exception& e) {
        std::cerr << "FAIL: " << e.what() << '\n';
        return 1;
    }
}
