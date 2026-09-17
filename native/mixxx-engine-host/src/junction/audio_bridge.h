#pragma once
#include <atomic>
#include <array>
#include <QtGlobal>
namespace junction {
struct AudioBridge {
    using Before = bool(*)(void*,unsigned);
    using After = void(*)(void*,float*,float*,unsigned);
    void* context=nullptr;
    Before before=nullptr;
    After after=nullptr;
    static constexpr unsigned maxFrames=16384;
    std::array<float,maxFrames*2> idleMaster{},idlePfl{};
    /// LOCAL NEXT return bus, written by EngineMixer::process: the post-fader
    /// local channels as they enter the crossfader buses, times main gain.
    /// Never the channel named by `localReturnExcluded` (JUNCTION MASTER) and
    /// never a microphone, so the return can echo neither the remote DJ nor
    /// the room. Owned by whichever driver currently processes the graph.
    std::array<float,maxFrames*2> localReturn{};
    std::atomic<int> localReturnExcluded{-1};
    /// OUTGOING tail: when nonzero only these channel handles (bits 0..63)
    /// reach the bus, and `localReturnFixedGain` (>= 0) replaces main gain so
    /// the tail never follows the outgoing DJ's master or booth knobs.
    std::atomic<quint64> localReturnMask{0};
    std::atomic<float> localReturnFixedGain{-1};
    std::atomic<bool> localReturnWritten{false};
    float localReturnGainOld=0;
    std::atomic<bool> idle{false};
    std::atomic<bool> inputsEnabled{true};
    std::atomic<unsigned> inputReaders{0};
};
// Changed only before SoundManager starts or after its callbacks have stopped.
extern std::atomic<AudioBridge*> audioBridge;
inline bool beforeAudio(unsigned frames) noexcept {
    auto* bridge=audioBridge.load(std::memory_order_acquire);
    const bool process=!bridge || !bridge->before || bridge->before(bridge->context,frames);
    if(bridge)bridge->idle.store(!process,std::memory_order_release);
    return process;
}
inline const float* idleOutput(bool pfl) noexcept {
    auto* bridge=audioBridge.load(std::memory_order_acquire);
    // SoundManager caches source addresses at setConfig, so these pointers
    // must be stable throughout both realtime and offline ownership.
    return bridge?(pfl?bridge->idlePfl.data():bridge->idleMaster.data()):nullptr;
}
class InputGuard final {
public:
    InputGuard() noexcept {
        bridge_=audioBridge.load(std::memory_order_acquire);
        if(!bridge_){allowed_=true;return;}
        if(!bridge_->inputsEnabled.load(std::memory_order_acquire))return;
        bridge_->inputReaders.fetch_add(1,std::memory_order_acq_rel);
        if(bridge_->inputsEnabled.load(std::memory_order_acquire)){allowed_=true;counted_=true;}
        else bridge_->inputReaders.fetch_sub(1,std::memory_order_release);
    }
    ~InputGuard(){if(counted_)bridge_->inputReaders.fetch_sub(1,std::memory_order_release);}
    explicit operator bool() const noexcept{return allowed_;}
    InputGuard(const InputGuard&)=delete;
    InputGuard& operator=(const InputGuard&)=delete;
private:
    AudioBridge* bridge_=nullptr;
    bool allowed_=false,counted_=false;
};
inline void afterAudio(float* master,float* pfl,unsigned frames) noexcept {
    auto* bridge=audioBridge.load(std::memory_order_acquire);
    if(bridge && bridge->after) {
        if(bridge->idle.load(std::memory_order_relaxed)){master=bridge->idleMaster.data();pfl=bridge->idlePfl.data();}
        bridge->after(bridge->context,master,pfl,frames);
    }
}
}
