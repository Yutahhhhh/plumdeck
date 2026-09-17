#pragma once
#include <atomic>
#include <array>
#include <QtGlobal>
namespace junction {
struct AudioBridge {
    using Before = void(*)(void*,unsigned);
    using After = void(*)(void*,float*,float*,unsigned);
    void* context=nullptr;
    Before before=nullptr;
    After after=nullptr;
    static constexpr unsigned maxFrames=16384;
    /// What the main and headphone device sinks play: copies of the engine
    /// buffers after the booth mute and private preview are applied.
    std::array<float,maxFrames*2> deviceMaster{},devicePfl{};
    /// LOCAL NEXT return bus, written by EngineMixer::process: the post-fader
    /// local channels as they enter the crossfader buses, times main gain.
    /// Never the channel named by `localReturnExcluded` (JUNCTION MASTER) and
    /// never a microphone, so the return can echo neither the remote DJ nor
    /// the room. Written and read on the audio thread only.
    std::array<float,maxFrames*2> localReturn{};
    std::atomic<int> localReturnExcluded{-1};
    /// OUTGOING tail: when nonzero only these channel handles (bits 0..63)
    /// reach the bus. `localReturnFixedGain` (>= 0) replaces main gain; it is
    /// unity so J, like Program, never follows a DJ's master (booth) knob.
    std::atomic<quint64> localReturnMask{0};
    std::atomic<float> localReturnFixedGain{1};
    /// Program: the main mix before main gain, written by EngineMixer::process.
    /// The master knob stays the DJ's booth volume and never moves the venue.
    std::array<float,maxFrames*2> programPre{};
    std::atomic<bool> programPreWritten{false};
    std::atomic<bool> localReturnWritten{false};
    float localReturnGainOld=0;
};
// Changed only before SoundManager starts or after its callbacks have stopped.
extern std::atomic<AudioBridge*> audioBridge;
inline void beforeAudio(unsigned frames) noexcept {
    auto* bridge=audioBridge.load(std::memory_order_acquire);
    if(bridge && bridge->before) bridge->before(bridge->context,frames);
}
inline const float* deviceOutput(bool pfl) noexcept {
    auto* bridge=audioBridge.load(std::memory_order_acquire);
    // SoundManager caches source addresses at setConfig, so these pointers
    // must stay stable for the whole stream.
    return bridge?(pfl?bridge->devicePfl.data():bridge->deviceMaster.data()):nullptr;
}
inline void afterAudio(float* master,float* pfl,unsigned frames) noexcept {
    auto* bridge=audioBridge.load(std::memory_order_acquire);
    if(bridge && bridge->after) bridge->after(bridge->context,master,pfl,frames);
}
}
