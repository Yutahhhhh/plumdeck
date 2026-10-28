#pragma once

// Ring length for delay/roll processors constructed next. Each processor owns
// one ring per route (input x output), ~5.6 MB per route at 16 s, so the host
// sets a short ring while loading PAD FX (pads use 2 beats or fewer) and keeps
// 16 s for BEAT FX. Read and written on the main thread only (effect loads).
inline double& ddjRingSeconds() { static double seconds = 16; return seconds; }
struct DdjRingScope {
    explicit DdjRingScope(double seconds) : previous_(ddjRingSeconds()) { ddjRingSeconds() = seconds; }
    ~DdjRingScope() { ddjRingSeconds() = previous_; }
    DdjRingScope(const DdjRingScope&) = delete;
    DdjRingScope& operator=(const DdjRingScope&) = delete;
private:
    double previous_;
};
