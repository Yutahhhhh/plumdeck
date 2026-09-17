#pragma once
// Fixed-capacity single-producer / single-consumer PCM block queue.
//
// The producer side runs inside an audio callback, so push() must not
// allocate, lock, or block (02 §1 rule 5). Capacity is reserved once at
// construction; overflow is counted and reported, never resolved by growing
// the buffer.
//
// Three rules this file exists to enforce, each of which was a real bug at
// some point in this implementation:
//
//  1. Payload and metadata are published together by one release store of the
//     write index, and consumed together. A separate "latest block" snapshot
//     both races with the producer and describes frames the consumer has not
//     reached yet.
//  2. A pop never silently merges across a discontinuity. Blocks from two
//     epochs, two stream generations or two sample rates are different audio;
//     labelling them with the first block's metadata puts frames on the wrong
//     place in the timeline. popRun() stops at the seam and says so.
//  3. The ring stores frames in the *producer's* domain (44.1 kHz for the local
//     Mixxx graph, 48 kHz for decoded network audio) while mediaFrame counts
//     48 kHz wire frames. Converting a within-block offset therefore goes
//     through mediaFrameAdvance(); adding a native offset to a wire frame is
//     wrong by 8.8% at 44.1 kHz.
#include "ids.h"
#include <algorithm>
#include <atomic>
#include <cstring>
#include <vector>
#include <QtGlobal>

namespace junction {

/// Converts an offset expressed in `sampleRateHz` frames into wire (48 kHz)
/// frames. Integer rational arithmetic; the whole-second part is exact so the
/// error cannot accumulate across a long set.
inline quint64 mediaFrameAdvance(quint64 offsetFrames, quint32 sampleRateHz) {
    if (sampleRateHz == 0 || sampleRateHz == kWireSampleRate) return offsetFrames;
    const quint64 seconds = offsetFrames / sampleRateHz;
    const quint64 remainder = offsetFrames % sampleRateHz;
    return seconds * kWireSampleRate + (remainder * kWireSampleRate) / sampleRateHz;
}

/// Metadata that must travel with every block so the far end can place it on
/// the shared media timeline (05 §5). A block is meaningless without it.
struct PcmBlockInfo {
    quint64 epoch = 0;
    /// Session media frame (48 kHz) of this block's first sample.
    quint64 mediaFrame = 0;
    /// Producer-local output frame counter, in `sampleRateHz` frames.
    quint64 sourceFrame = 0;
    /// Producer block counter. A gap means blocks were dropped upstream.
    quint64 sequence = 0;
    /// Bumped whenever the producer restarts its stream (reconnect, device
    /// change). Old and new generations must never be concatenated.
    quint64 generation = 0;
    /// Frames in this block, in `sampleRateHz` frames.
    quint32 frameCount = 0;
    quint32 sampleRateHz = 0;
    quint16 channels = 0;
    /// True when the producer emitted silence deliberately (03 §5: keep the
    /// stream alive through quiet passages instead of looking disconnected).
    bool silent = false;
    /// Frames already consumed from the block this metadata came from. Lets a
    /// worker that needs exact fractional phase redo the conversion itself
    /// instead of trusting the rounded mediaFrame.
    quint32 frameOffsetInBlock = 0;

    /// Whether `next` continues this block with no seam. Sample rate, channel
    /// count, epoch and stream generation must all match and the sequence must
    /// be the immediate successor.
    bool continuesInto(const PcmBlockInfo& next) const {
        return epoch == next.epoch && generation == next.generation &&
                sampleRateHz == next.sampleRateHz && channels == next.channels &&
                next.sequence == sequence + 1;
    }
};

/// Why a run of frames stopped short of the requested length.
enum class PopStop {
    /// The requested number of frames was delivered.
    Filled,
    /// Nothing more is queued.
    Empty,
    /// The next block is a different epoch/generation/rate. Caller must decide
    /// (flush the encoder, restart the resampler, re-arm the router) and call
    /// again to cross it.
    Discontinuity,
};

struct PopResult {
    quint32 frames = 0;
    PopStop stop = PopStop::Empty;
    /// Describes exactly the frames returned. Zero-filled when frames == 0.
    PcmBlockInfo info;
};

class PcmRing {
public:
    /// `blockSlots` blocks of at most `maxBlockFrames` frames each. Both fixed
    /// for the lifetime of the ring; nothing here ever reallocates.
    PcmRing(quint32 blockSlots, quint32 maxBlockFrames, quint16 channels)
        : channels_(channels), maxBlockFrames_(maxBlockFrames), blockSlots_(std::max<quint32>(blockSlots, 2)),
          samples_(static_cast<size_t>(blockSlots_) * maxBlockFrames * channels, 0.0f),
          infos_(blockSlots_) {}

    quint16 channels() const { return channels_; }
    quint32 maxBlockFrames() const { return maxBlockFrames_; }
    quint32 blockSlots() const { return blockSlots_; }

    // ---- producer (audio thread) -----------------------------------------

    /// Copies one block. Returns false without consuming anything when the
    /// queue is full or the block does not fit; the caller must not retry or
    /// wait inside the callback.
    bool push(const float* interleaved, const PcmBlockInfo& info) {
        if (info.frameCount == 0 || info.frameCount > maxBlockFrames_ || info.channels != channels_) {
            rejected_.fetch_add(1, std::memory_order_relaxed);
            return false;
        }
        const quint64 write = write_.load(std::memory_order_relaxed);
        const quint64 read = read_.load(std::memory_order_acquire);
        if (write - read >= blockSlots_) {
            overflows_.fetch_add(1, std::memory_order_relaxed);
            return false;
        }
        const size_t slot = static_cast<size_t>(write % blockSlots_);
        std::memcpy(samples_.data() + slot * maxBlockFrames_ * channels_, interleaved,
                static_cast<size_t>(info.frameCount) * channels_ * sizeof(float));
        infos_[slot] = info;
        infos_[slot].frameOffsetInBlock = 0;
        // Release: the slot contents above become visible to a consumer that
        // acquire-loads write_ and therefore sees this slot as readable.
        write_.store(write + 1, std::memory_order_release);
        return true;
    }

    // ---- consumer (one worker or one output callback) --------------------

    /// Frames queued right now, counting across discontinuities. Use
    /// contiguousFrames() when the answer has to be playable as one run.
    quint32 availableFrames() {
        sync();
        return availableFrames_;
    }

    /// Frames available before the next seam. This is the number a caller may
    /// rely on being one continuous piece of audio.
    quint32 contiguousFrames() {
        sync();
        quint64 cursor = read_.load(std::memory_order_relaxed);
        if (cursor >= knownWrite_) return 0;
        quint32 total = infos_[static_cast<size_t>(cursor % blockSlots_)].frameCount - headOffset_;
        while (cursor + 1 < knownWrite_) {
            const PcmBlockInfo& current = infos_[static_cast<size_t>(cursor % blockSlots_)];
            const PcmBlockInfo& next = infos_[static_cast<size_t>((cursor + 1) % blockSlots_)];
            if (!current.continuesInto(next)) break;
            total += next.frameCount;
            ++cursor;
        }
        return total;
    }

    /// Takes at most the remainder of the head block. The returned metadata
    /// describes exactly the frames delivered.
    PopResult popBlock(float* out, quint32 maxFrames) {
        PopResult result;
        sync();
        const quint64 read = read_.load(std::memory_order_relaxed);
        if (read >= knownWrite_ || maxFrames == 0) {
            result.stop = PopStop::Empty;
            underruns_.fetch_add(1, std::memory_order_relaxed);
            return result;
        }
        result.frames = takeFromHead(out, maxFrames, &result.info);
        result.stop = result.frames == maxFrames ? PopStop::Filled : PopStop::Discontinuity;
        return result;
    }

    /// Fills up to `frames`, crossing block boundaries only while the blocks
    /// are contiguous. Stops at a seam and reports it rather than mislabelling
    /// the audio. Whatever it delivered has already been consumed.
    PopResult popRun(float* out, quint32 frames) {
        PopResult result;
        sync();
        if (frames == 0) return result;
        quint32 produced = 0;
        bool captured = false;
        PcmBlockInfo previous;
        while (produced < frames) {
            const quint64 read = read_.load(std::memory_order_relaxed);
            if (read >= knownWrite_) { result.stop = PopStop::Empty; break; }
            const PcmBlockInfo& head = infos_[static_cast<size_t>(read % blockSlots_)];
            if (captured && !previous.continuesInto(head)) {
                result.stop = PopStop::Discontinuity;
                discontinuities_.fetch_add(1, std::memory_order_relaxed);
                break;
            }
            previous = head;
            PcmBlockInfo taken;
            const quint32 got = takeFromHead(out + static_cast<size_t>(produced) * channels_, frames - produced, &taken);
            if (got == 0) { result.stop = PopStop::Empty; break; }
            if (!captured) { result.info = taken; captured = true; }
            produced += got;
        }
        if (captured) result.info.frameCount = produced;
        result.frames = produced;
        if (produced == frames) result.stop = PopStop::Filled;
        if (produced == 0) underruns_.fetch_add(1, std::memory_order_relaxed);
        return result;
    }

    /// Metadata of the frames the *next* pop would return, without consuming.
    bool peek(PcmBlockInfo* out) {
        sync();
        const quint64 read = read_.load(std::memory_order_relaxed);
        if (read >= knownWrite_) return false;
        *out = headInfo(read);
        return true;
    }

    /// Drops everything currently readable. Used when a stream generation
    /// changes and old packets must not be mixed with new ones (03 §5).
    void drop() {
        sync();
        read_.store(knownWrite_, std::memory_order_release);
        headOffset_ = 0;
        availableFrames_ = 0;
    }

    // ---- diagnostics ------------------------------------------------------
    quint64 overflows() const { return overflows_.load(std::memory_order_relaxed); }
    quint64 underruns() const { return underruns_.load(std::memory_order_relaxed); }
    quint64 rejected() const { return rejected_.load(std::memory_order_relaxed); }
    quint64 discontinuities() const { return discontinuities_.load(std::memory_order_relaxed); }
    /// Bytes actually reserved, so the RSS budget in 08 §7 is measured rather
    /// than estimated.
    size_t reservedBytes() const { return samples_.size() * sizeof(float) + infos_.size() * sizeof(PcmBlockInfo); }

private:
    /// Head metadata adjusted for frames already consumed from that block.
    PcmBlockInfo headInfo(quint64 read) const {
        PcmBlockInfo info = infos_[static_cast<size_t>(read % blockSlots_)];
        info.mediaFrame += mediaFrameAdvance(headOffset_, info.sampleRateHz);
        info.sourceFrame += headOffset_;
        info.frameCount -= headOffset_;
        info.frameOffsetInBlock = headOffset_;
        return info;
    }

    /// Copies at most `maxFrames` from the head block. Caller must have
    /// sync()ed and confirmed the head exists.
    quint32 takeFromHead(float* out, quint32 maxFrames, PcmBlockInfo* info) {
        const quint64 read = read_.load(std::memory_order_relaxed);
        const size_t slot = static_cast<size_t>(read % blockSlots_);
        const PcmBlockInfo& stored = infos_[slot];
        const quint32 remaining = stored.frameCount - headOffset_;
        const quint32 take = std::min(remaining, maxFrames);
        if (take == 0) return 0;
        if (info) {
            *info = headInfo(read);
            info->frameCount = take;
        }
        std::memcpy(out, samples_.data() + (slot * maxBlockFrames_ + headOffset_) * channels_,
                static_cast<size_t>(take) * channels_ * sizeof(float));
        headOffset_ += take;
        availableFrames_ -= take;
        if (headOffset_ == stored.frameCount) {
            headOffset_ = 0;
            read_.store(read + 1, std::memory_order_release);
        }
        return take;
    }

    /// Folds newly published blocks into the cached frame count. Safe because
    /// the acquire pairs with the producer's release store.
    void sync() {
        const quint64 write = write_.load(std::memory_order_acquire);
        while (knownWrite_ < write) {
            availableFrames_ += infos_[static_cast<size_t>(knownWrite_ % blockSlots_)].frameCount;
            ++knownWrite_;
        }
    }

    quint16 channels_;
    quint32 maxBlockFrames_;
    quint32 blockSlots_;
    std::vector<float> samples_;
    std::vector<PcmBlockInfo> infos_;
    std::atomic<quint64> write_{0}, read_{0};
    std::atomic<quint64> overflows_{0}, underruns_{0}, rejected_{0}, discontinuities_{0};
    // Consumer-private. Only the single consumer thread touches these.
    quint64 knownWrite_ = 0;
    quint32 headOffset_ = 0;
    quint32 availableFrames_ = 0;
};

} // namespace junction
