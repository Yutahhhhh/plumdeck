#pragma once
#include <algorithm>
#include <array>
#include <atomic>
#include <cmath>
#include <cstddef>

// Linear sample amplitude measured on the audio callback. No allocation or
// locks. The peak is held until the meter is read, so a 20 ms reader never
// misses a transient between two blocks; RMS is the latest block's.
// GUI meters are snapshots, never logarithmic Mixxx vu_meter values
// masquerading as peak or RMS amplitudes.
class AudioMeter {
public:
    /** Interleaved stereo; also tracks the per-side peak and a clip flag. */
    void process(const float* samples, std::size_t count) noexcept {
        std::array<float, 2> peaks{};
        double energy = 0; bool clipped = false;
        if (samples) for (std::size_t i = 0; i < count; ++i) {
            const float value = std::isfinite(samples[i]) ? samples[i] : 0;
            const float level = std::abs(value);
            peaks[i & 1] = std::max(peaks[i & 1], level);
            clipped = clipped || level >= 1.0f;
            energy += double(value) * value;
        }
        for (int side = 0; side < 2; ++side) hold(side_[side], peaks[side]);
        hold(peak_, std::max(peaks[0], peaks[1]));
        if (clipped) clip_.store(true, std::memory_order_relaxed);
        rms_.store(count ? float(std::sqrt(energy / count)) : 0, std::memory_order_relaxed);
    }
    /** Peak since the previous call; resets the hold. Single reader only. */
    float takePeak() const noexcept { return peak_.exchange(0, std::memory_order_relaxed); }
    /** Left/right peak since the previous call. Single reader only. */
    float takeSidePeak(int side) const noexcept { return side_[side & 1].exchange(0, std::memory_order_relaxed); }
    /** A sample reached full scale (>= 1.0) since the previous call. */
    bool takeClip() const noexcept { return clip_.exchange(false, std::memory_order_relaxed); }
    float rms() const noexcept { return rms_.load(std::memory_order_relaxed); }
private:
    static void hold(std::atomic<float>& slot, float peak) noexcept {
        float held = slot.load(std::memory_order_relaxed);
        while (peak > held && !slot.compare_exchange_weak(held, peak, std::memory_order_relaxed)) {}
    }
    static_assert(std::atomic<float>::is_always_lock_free);
    mutable std::atomic<float> peak_{0};
    mutable std::array<std::atomic<float>, 2> side_{};
    mutable std::atomic<bool> clip_{false};
    std::atomic<float> rms_{0};
};
