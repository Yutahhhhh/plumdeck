// Contracts for the three clocks of 04 §2 and the PCM ring of 05 §5.
#include "harness.h"
#include "junction/audio_clock.h"
#include "junction/pcm_ring.h"
#include <thread>
#include <vector>

using namespace junction;

namespace {
constexpr qint64 kSecond = 1000000000LL;
} // namespace

JTEST("media-timeline", "counts 48 kHz frames from T0 and never goes backwards") {
    MediaTimeline timeline;
    timeline.start(1000 * kSecond);
    CHECK_EQ(timeline.frameAt(1000 * kSecond), 0ULL);
    CHECK_EQ(timeline.frameAt(1001 * kSecond), 48000ULL);
    // Before T0 the timeline stays at the origin instead of underflowing.
    CHECK_EQ(timeline.frameAt(999 * kSecond), 0ULL);
    CHECK_EQ(timeline.frameAt(1000 * kSecond + kSecond / 2), 24000ULL);
}

JTEST("media-timeline", "stays exact over a long set instead of drifting in double") {
    MediaTimeline timeline;
    timeline.start(0);
    // Four hours. A naive nanos*48000/1e9 in double loses frames here; the
    // seconds/remainder split must not.
    const qint64 fourHours = 4 * 3600 * kSecond;
    CHECK_EQ(timeline.frameAt(fourHours), 4ULL * 3600ULL * 48000ULL);
    // Round trip through the inverse mapping lands on the same frame.
    const quint64 frame = 4ULL * 3600ULL * 48000ULL + 12345ULL;
    CHECK_EQ(timeline.frameAt(timeline.nanosAt(frame)), frame);
}

JTEST("media-timeline", "a late joiner adopts the host origin rather than inventing one") {
    MediaTimeline host, guest;
    host.start(500 * kSecond);
    guest.adopt(host.originNanos(), host.originFrame());
    CHECK_EQ(guest.frameAt(507 * kSecond), host.frameAt(507 * kSecond));
}

JTEST("clock-estimator", "recovers a known offset and rejects nonsense rounds") {
    ClockEstimator estimator;
    // Remote clock runs 3 s ahead; symmetric 20 ms path.
    const qint64 offset = 3 * kSecond;
    for (int round = 0; round < 8; ++round) {
        const qint64 t1 = round * kSecond;
        const qint64 t2 = t1 + 10000000 + offset;
        const qint64 t3 = t2 + 1000000;
        const qint64 t4 = t3 - offset + 10000000;
        CHECK(estimator.add({t1, t2, t3, t4}));
    }
    CHECK(estimator.ready());
    CHECK_NEAR(estimator.offsetNanos(), offset, 2.0e6);
    CHECK_NEAR(estimator.drift(), 1.0, 1.0e-4);
    // A round whose reply left before it arrived is not a clock sample.
    CHECK(!estimator.add({0, 100, 50, 200}));
    // Nor is a multi-second RTT.
    CHECK(!estimator.add({0, 1, 2, 10 * kSecond}));
}

JTEST("clock-estimator", "ignores queued rounds instead of averaging their bias in") {
    ClockEstimator estimator;
    const qint64 offset = 2 * kSecond;
    for (int round = 0; round < 12; ++round) {
        const qint64 t1 = round * kSecond;
        // Every third round sits in a queue for 200 ms on the return path,
        // which would drag a naive average 100 ms off.
        const qint64 extra = (round % 3 == 0) ? 200000000LL : 0LL;
        const qint64 t2 = t1 + 5000000 + offset;
        const qint64 t3 = t2 + 1000000;
        const qint64 t4 = t3 - offset + 5000000 + extra;
        estimator.add({t1, t2, t3, t4});
    }
    CHECK(estimator.ready());
    CHECK_NEAR(estimator.offsetNanos(), offset, 5.0e6);
}

JTEST("clock-estimator", "reset drops the estimate so a stale offset is not reused") {
    ClockEstimator estimator;
    for (int round = 0; round < 6; ++round)
        estimator.add({round * kSecond, round * kSecond + kSecond, round * kSecond + kSecond + 1000, round * kSecond + 2000});
    CHECK(estimator.ready());
    estimator.reset();
    CHECK(!estimator.ready());
    CHECK_EQ(estimator.offsetNanos(), 0LL);
    CHECK_EQ(estimator.toHostNanos(12345), 12345LL);
}

JTEST("audio-clock-anchor", "measures the real device rate in ppm") {
    AudioClockAnchor anchor(44100);
    // Device actually runs 100 ppm fast.
    const double realRate = 44100.0 * (1.0 + 100.0e-6);
    for (int block = 0; block <= 400; ++block) {
        const quint64 frames = static_cast<quint64>(block) * 256;
        const qint64 nanos = static_cast<qint64>(frames / realRate * 1.0e9);
        anchor.observe(frames, nanos);
    }
    CHECK_NEAR(anchor.driftPpm(), 100.0, 5.0);
}

JTEST("asrc", "drives a skewed ring back to its target fill") {
    // Closed loop: the producer writes at a rate 200 ppm faster than the
    // consumer's nominal rate, so without correction the ring grows without
    // bound. The controller must pull the fill back to target and hold it.
    AsrcController::Config config;
    config.targetFillFrames = 4800;
    AsrcController controller(config);
    double fill = 4800.0;
    const double producerPerBlock = 512.0 * (1.0 + 200.0e-6);
    for (int block = 0; block < 40000; ++block) {
        const double ratio = controller.update(fill);
        // Output side consumes a fixed 512 output frames, which at `ratio`
        // output-per-input frames eats 512/ratio input frames.
        fill += producerPerBlock - 512.0 / ratio;
    }
    CHECK_NEAR(fill, config.targetFillFrames, 400.0);
    // A ring that is over target must be drained, i.e. ratio below unity.
    CHECK(controller.update(config.targetFillFrames + 5000.0) < 1.0);
    // ...and an empty one refilled.
    controller.reset();
    CHECK(controller.update(0.0) > 1.0);
}

JTEST("asrc", "clamps the correction so a stall cannot produce a pitch jump") {
    AsrcController controller;
    for (int i = 0; i < 100000; ++i) controller.update(1000000.0);
    CHECK(controller.ratio() >= 1.0 - controller.config().maxRatioDeviation - 1e-12);
    CHECK(controller.ratio() <= 1.0 + controller.config().maxRatioDeviation + 1e-12);
}

// --- PCM ring --------------------------------------------------------------

namespace {
PcmBlockInfo block(quint64 sequence, quint32 frames, quint32 rate = 48000, quint64 epoch = 1, quint64 generation = 1) {
    PcmBlockInfo info;
    info.epoch = epoch;
    info.generation = generation;
    info.sequence = sequence;
    info.frameCount = frames;
    info.sampleRateHz = rate;
    info.channels = 2;
    info.mediaFrame = sequence * mediaFrameAdvance(frames, rate);
    info.sourceFrame = sequence * frames;
    return info;
}
std::vector<float> ramp(quint32 frames, float base) {
    std::vector<float> out(static_cast<size_t>(frames) * 2);
    for (quint32 index = 0; index < frames; ++index) {
        out[index * 2] = base + static_cast<float>(index);
        out[index * 2 + 1] = -(base + static_cast<float>(index));
    }
    return out;
}
} // namespace

JTEST("pcm-ring", "round trips blocks and reports what it actually reserved") {
    PcmRing ring(8, 512, 2);
    const auto data = ramp(256, 0.0f);
    CHECK(ring.push(data.data(), block(0, 256)));
    CHECK_EQ(ring.availableFrames(), 256u);
    std::vector<float> out(512 * 2, 0.0f);
    const PopResult result = ring.popRun(out.data(), 256);
    CHECK_EQ(result.frames, 256u);
    CHECK_EQ(result.stop, PopStop::Filled);
    CHECK_EQ(out[0], 0.0f);
    CHECK_EQ(out[510], 255.0f);
    CHECK(ring.reservedBytes() >= 8u * 512u * 2u * sizeof(float));
}

JTEST("pcm-ring", "overflow is counted and dropped, never grown into") {
    PcmRing ring(2, 128, 2);
    const auto data = ramp(128, 0.0f);
    CHECK(ring.push(data.data(), block(0, 128)));
    CHECK(ring.push(data.data(), block(1, 128)));
    CHECK(!ring.push(data.data(), block(2, 128)));
    CHECK_EQ(ring.overflows(), 1ULL);
    // A block larger than a slot is rejected rather than truncated.
    CHECK(!ring.push(data.data(), block(3, 999)));
    CHECK_EQ(ring.rejected(), 1ULL);
}

JTEST("pcm-ring", "metadata describes the frames returned, not the newest block") {
    // The regression this guards: reading a "latest block" snapshot handed the
    // consumer the epoch/frame of audio it had not played yet.
    PcmRing ring(8, 256, 2);
    const auto first = ramp(256, 0.0f);
    const auto second = ramp(256, 1000.0f);
    ring.push(first.data(), block(0, 256));
    ring.push(second.data(), block(1, 256));
    std::vector<float> out(256 * 2);
    const PopResult result = ring.popRun(out.data(), 100);
    CHECK_EQ(result.frames, 100u);
    CHECK_EQ(result.info.mediaFrame, 0ULL);
    CHECK_EQ(result.info.sequence, 0ULL);
    const PopResult next = ring.popRun(out.data(), 100);
    // Still inside block 0, 100 frames in.
    CHECK_EQ(next.info.mediaFrame, 100ULL);
    CHECK_EQ(next.info.frameOffsetInBlock, 100u);
}

JTEST("pcm-ring", "44.1 kHz partial pops convert offsets into wire frames") {
    // Adding a native-frame offset straight onto a 48 kHz media frame is wrong
    // by 8.8%; 4410 native frames are exactly 4800 wire frames.
    PcmRing ring(4, 4410, 2);
    const auto data = ramp(4410, 0.0f);
    ring.push(data.data(), block(0, 4410, 44100));
    std::vector<float> out(4410 * 2);
    ring.popRun(out.data(), 2205);
    PcmBlockInfo peeked;
    CHECK(ring.peek(&peeked));
    CHECK_EQ(peeked.mediaFrame, 2400ULL);
    CHECK_EQ(peeked.sourceFrame, 2205ULL);
    CHECK_EQ(peeked.frameCount, 2205u);
}

JTEST("pcm-ring", "a run stops at an epoch seam instead of mislabelling it") {
    PcmRing ring(8, 256, 2);
    const auto data = ramp(256, 0.0f);
    ring.push(data.data(), block(0, 256, 48000, /*epoch*/ 1));
    ring.push(data.data(), block(1, 256, 48000, /*epoch*/ 2));
    std::vector<float> out(1024 * 2);
    const PopResult result = ring.popRun(out.data(), 512);
    CHECK_EQ(result.frames, 256u);
    CHECK_EQ(result.stop, PopStop::Discontinuity);
    CHECK_EQ(result.info.epoch, 1ULL);
    CHECK_EQ(ring.discontinuities(), 1ULL);
    // Calling again crosses the seam and reports the new epoch.
    const PopResult after = ring.popRun(out.data(), 256);
    CHECK_EQ(after.frames, 256u);
    CHECK_EQ(after.info.epoch, 2ULL);
}

JTEST("pcm-ring", "a run stops at a sample-rate or generation seam too") {
    PcmRing ring(8, 512, 2);
    const auto data = ramp(256, 0.0f);
    ring.push(data.data(), block(0, 256, 48000, 1, /*generation*/ 1));
    ring.push(data.data(), block(1, 256, 48000, 1, /*generation*/ 2));
    std::vector<float> out(1024 * 2);
    CHECK_EQ(ring.popRun(out.data(), 512).frames, 256u);

    PcmRing rates(8, 512, 2);
    rates.push(data.data(), block(0, 256, 48000));
    rates.push(data.data(), block(1, 256, 44100));
    CHECK_EQ(rates.popRun(out.data(), 512).frames, 256u);
}

JTEST("pcm-ring", "contiguousFrames only counts what is playable as one run") {
    PcmRing ring(8, 256, 2);
    const auto data = ramp(256, 0.0f);
    ring.push(data.data(), block(0, 256));
    ring.push(data.data(), block(1, 256));
    ring.push(data.data(), block(2, 256, 48000, /*epoch*/ 9));
    CHECK_EQ(ring.availableFrames(), 768u);
    CHECK_EQ(ring.contiguousFrames(), 512u);
}

JTEST("pcm-ring", "survives a real concurrent producer and consumer") {
    // The publication bug this guards was only visible with two threads: the
    // metadata write was not ordered with the index that published it.
    PcmRing ring(64, 256, 2);
    constexpr int kBlocks = 20000;
    std::thread producer([&] {
        for (int index = 0; index < kBlocks;) {
            const auto data = ramp(256, static_cast<float>(index));
            if (ring.push(data.data(), block(static_cast<quint64>(index), 256))) ++index;
            else std::this_thread::yield();
        }
    });
    std::vector<float> out(256 * 2);
    quint64 expected = 0;
    while (expected < kBlocks) {
        const PopResult result = ring.popRun(out.data(), 256);
        if (result.frames == 0) { std::this_thread::yield(); continue; }
        CHECK_EQ(result.info.sequence, expected);
        // The payload must match the metadata that was published with it.
        CHECK_EQ(out[0], static_cast<float>(expected));
        ++expected;
    }
    producer.join();
    CHECK_EQ(expected, static_cast<quint64>(kBlocks));
}
