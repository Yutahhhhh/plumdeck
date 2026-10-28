#pragma once
#include <algorithm>
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
    void process(const float* samples, std::size_t count) noexcept {
        float peak = 0; double energy = 0;
        if (samples) for (std::size_t i = 0; i < count; ++i) {
            const float value = std::isfinite(samples[i]) ? samples[i] : 0;
            peak = std::max(peak, std::abs(value)); energy += double(value) * value;
        }
        float held = peak_.load(std::memory_order_relaxed);
        while (peak > held && !peak_.compare_exchange_weak(held, peak, std::memory_order_relaxed)) {}
        rms_.store(count ? float(std::sqrt(energy / count)) : 0, std::memory_order_relaxed);
    }
    /** Peak since the previous call; resets the hold. Single reader only. */
    float takePeak() const noexcept { return peak_.exchange(0, std::memory_order_relaxed); }
    float rms() const noexcept { return rms_.load(std::memory_order_relaxed); }
private:
    static_assert(std::atomic<float>::is_always_lock_free);
    mutable std::atomic<float> peak_{0};
    std::atomic<float> rms_{0};
};
