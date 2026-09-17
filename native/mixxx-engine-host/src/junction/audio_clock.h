#pragma once
// The three clocks of 04 §2, kept deliberately separate.
//
//  1. MediaTimeline   — the shared 48 kHz session frame counter N(t).
//  2. ClockEstimator  — a peer's monotonic clock mapped onto the host's.
//  3. AudioClockAnchor— a device's audio frame counter mapped onto monotonic
//                       time, so 44.1 kHz native frames and 48 kHz wire frames
//                       never get confused.
//
// Nothing here uses wall-clock/UTC time. A user changing the system clock must
// not move the music.
#include <QtGlobal>
#include <cstddef>
#include <deque>
#include <optional>

namespace junction {

/// Monotonic nanoseconds since an arbitrary process-local origin.
qint64 monotonicNanos();

/// The shared media timeline. `T0` is captured once when the session starts;
/// every participant expresses positions as frames since T0 at 48 kHz.
class MediaTimeline {
public:
    MediaTimeline() = default;
    /// Anchors the timeline at `hostMonotonicNanos`. Idempotent per session.
    void start(qint64 hostMonotonicNanos, quint64 originFrame = 0);
    bool started() const { return started_; }
    qint64 originNanos() const { return t0Nanos_; }
    quint64 originFrame() const { return originFrame_; }

    /// N(t) for a host-monotonic instant. Monotonically non-decreasing.
    quint64 frameAt(qint64 hostMonotonicNanos) const;
    quint64 now() const { return frameAt(monotonicNanos()); }
    /// Inverse mapping, for scheduling a future frame.
    qint64 nanosAt(quint64 mediaFrame) const;

    /// Restores a timeline received from the host during a late join.
    void adopt(qint64 t0Nanos, quint64 originFrame);

private:
    bool started_ = false;
    qint64 t0Nanos_ = 0;
    quint64 originFrame_ = 0;
};

/// One round of the NTP-style four-timestamp exchange of 04 §2.2.
struct ClockProbe {
    /// Local monotonic time the probe left this peer.
    qint64 t1 = 0;
    /// Remote monotonic time the probe arrived.
    qint64 t2 = 0;
    /// Remote monotonic time the reply left.
    qint64 t3 = 0;
    /// Local monotonic time the reply arrived.
    qint64 t4 = 0;
};

/// Estimates `C_host ~= a * C_local + b` from probe rounds.
///
/// Offsets are taken from the lowest-RTT rounds only, because a queued packet
/// biases the estimate by half its extra delay. Drift `a` comes from a least
/// squares fit over the retained rounds, and is clamped: a real crystal is
/// within a few hundred ppm, so a wilder slope means the samples are bad, not
/// that the clock is bad.
class ClockEstimator {
public:
    explicit ClockEstimator(size_t window = 64) : window_(window) {}

    /// Feeds one completed round. Rejects rounds with a negative or absurd RTT.
    bool add(const ClockProbe& probe);

    bool ready() const { return samples_.size() >= kMinSamples; }
    /// Drift ratio. 1.0 until enough rounds are in.
    double drift() const { return drift_; }
    /// Offset in nanoseconds at the last accepted round.
    qint64 offsetNanos() const { return offsetNanos_; }

    /// Maps a local monotonic instant onto the host's timeline.
    qint64 toHostNanos(qint64 localNanos) const;
    /// Maps a host instant back onto the local timeline.
    qint64 toLocalNanos(qint64 hostNanos) const;

    /// Drops every sample. Required after suspend/resume or a path change
    /// (04 §2.2): a stale offset must not be treated as still valid.
    void reset();

private:
    struct Sample { qint64 localMid; qint64 offset; qint64 rtt; };
    static constexpr size_t kMinSamples = 4;
    void refit();
    size_t window_;
    std::deque<Sample> samples_;
    double drift_ = 1.0;
    qint64 offsetNanos_ = 0;
    qint64 bestRttNanos_ = 0;
    qint64 anchorLocal_ = 0;
};

/// Ties a device's audio frame counter to monotonic time so the true device
/// rate (never exactly the nominal rate) can be measured.
class AudioClockAnchor {
public:
    explicit AudioClockAnchor(quint32 nominalRateHz) : nominalRateHz_(nominalRateHz) {}
    /// Called once per audio callback with the cumulative frame count and the
    /// monotonic time at that callback. Cheap: no allocation, no locking.
    void observe(quint64 deviceFrames, qint64 monotonicNs);
    /// Deviation from nominal in parts per million.
    double driftPpm() const;
    quint32 nominalRateHz() const { return nominalRateHz_; }
    void reset();

private:
    quint32 nominalRateHz_;
    quint64 observations_ = 0;
    quint64 firstFrames_ = 0, lastFrames_ = 0;
    qint64 firstNanos_ = 0, lastNanos_ = 0;
    double measuredRateHz_ = 0.0;
};

/// Decides the asynchronous-sample-rate-conversion ratio for a device whose
/// clock differs from the session clock (05 §4). Without this a two-hour set
/// drains or floods the Program ring.
///
/// Convention, stated once so no call site has to guess: ratio() is the
/// libsamplerate-style `src_ratio`, i.e. **output frames per input frame**.
/// The ring is filled by the remote producer (the converter's input) and
/// drained by the local output device (its output). A ring that is filling up
/// therefore needs the ratio to go **below** 1.0 so each output block eats more
/// input; a draining ring needs it above 1.0. `update()` applies that sign.
///
/// The controller is deliberately slow: it corrects fill error over tens of
/// seconds and clamps the ratio, because an audible pitch wobble is worse than
/// a slightly off buffer level.
class AsrcController {
public:
    struct Config {
        /// Frames we want sitting in the ring.
        double targetFillFrames = 4800.0;
        /// Largest correction allowed, as a fraction of unity.
        double maxRatioDeviation = 0.005;
        double proportionalGain = 2.0e-7;
        double integralGain = 4.0e-10;
        /// Fill error inside this band is treated as zero, so the ratio settles.
        double deadbandFrames = 240.0;
    };
    AsrcController() = default;
    explicit AsrcController(Config config) : config_(config) {}
    /// `fillFrames` is the current ring occupancy. Returns the output-frames
    /// -per-input-frame ratio to apply to the next block. Above target fill the
    /// result is < 1.0.
    double update(double fillFrames);
    double ratio() const { return ratio_; }
    void reset();
    const Config& config() const { return config_; }

private:
    Config config_;
    double ratio_ = 1.0;
    double integrator_ = 0.0;
};

} // namespace junction
