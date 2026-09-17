#pragma once
#include "audio_clock.h"
#include "pcm_ring.h"
#include <QtGlobal>
#include <atomic>
#include <memory>
#include <optional>
#include <vector>
struct SRC_STATE_tag;
namespace junction {
/// Another DJ's audio as a local mixer channel (the JUNCTION deck). Decoded
/// 48 kHz blocks arrive on the session thread; the engine callback reads
/// 44.1 kHz frames. Single producer, single consumer, no locks on either side.
///
/// Every block carries the session media frame of its first sample, and the
/// reader publishes the media frame it is currently rendering. That is the
/// only sound basis for a seam: wall-clock time and the instantaneous ring
/// fill both drift away from the frame the sound card is actually playing.
///
/// The mapping survives the sample-rate conversion because the producer
/// publishes one (ring position, media frame) mark per block -- the position
/// just past the frames it wrote, and the media frame just past the input it
/// consumed -- and the reader converts its own distance from that mark with
/// mediaFrameAdvance(). The residual error is the converter's own group delay,
/// a constant well under a millisecond, not the 8.8% a naive 44.1/48 mix-up
/// costs and not the tens of milliseconds a wall-clock estimate costs.
class JunctionInput {
public:
    static constexpr unsigned kOutputRate=44100;
    static constexpr unsigned kTargetFill=5292;   // 120 ms absorbs Opus packet and tick jitter.
    static constexpr unsigned kMaxExcess=11025;   // Beyond target+250 ms the reader skips ahead.
    /// Equal-power ramp used for every discontinuity the reader creates.
    static constexpr unsigned kFadeFrames=64;     // ~1.5 ms.
    /// Largest one-shot seam correction the reader will apply, in output frames.
    static constexpr qint64 kMaxAlignFrames=2205; // 50 ms.
    /// `kTargetFill` expressed in wire (48 kHz) frames.
    static constexpr quint64 kNominalLatency48k=quint64(kTargetFill)*kWireSampleRate/kOutputRate;
    /// How far behind the session clock a rendered frame may plausibly be:
    /// the whole buffer the reader tolerates, plus a fifth of a second of
    /// network and clock-estimate slack. A reported frame outside this band
    /// is not a seam, it is a bug or a stale stream, and must not be used.
    static constexpr quint64 kMaxSeamLag48k=
            quint64(kTargetFill+kMaxExcess)*kWireSampleRate/kOutputRate+kWireSampleRate/5;
    /// A rendered frame is never genuinely *ahead* of the local session clock;
    /// only the clock estimate's own slack can put it there.
    static constexpr quint64 kMaxSeamLead48k=kWireSampleRate/10; // 100 ms.
    JunctionInput();
    ~JunctionInput();
    JunctionInput(const JunctionInput&)=delete;
    JunctionInput& operator=(const JunctionInput&)=delete;
    /// Session thread. Interleaved stereo; `info` must describe these frames.
    /// A block that does not continue the previous one (new epoch, new stream
    /// generation, new rate, or a gap in the media timeline) restarts the
    /// converter and drops what is buffered: two mappings must never share the
    /// ring, or the reported frame is wrong for everything behind the seam.
    void write(const float* interleaved,const PcmBlockInfo& info);
    /// Session thread. Drops buffered audio; the reader fades back in once primed.
    void reset();
    /// Audio thread. Always fills `frames` stereo frames; silence when starved.
    void read(float* out,unsigned frames) noexcept;
    /// Session thread. Asks the reader to move `frames` output frames forward
    /// (positive) or back (negative) once, with a crossfade. The request is
    /// consumed by the next `read()` and leaves no state behind, so a later
    /// handoff can never inherit it. Replaces any request not yet consumed.
    void requestAlign(qint64 frames) noexcept;
    /// Current buffered delay in 48 kHz session frames.
    quint64 latencyFrames48k() const;
    /// Session media frame (48 kHz) of the first sample the last `read()`
    /// emitted. Empty until a block has been written and the reader has
    /// primed. Read from the audio thread inside the same callback, this is
    /// exactly the remote frame that block of local master is carrying.
    std::optional<quint64> renderedMediaFrame() const;
    quint64 underruns() const {return underruns_.load(std::memory_order_relaxed);}
    /// Times the reader had to jump inside the buffer (flood, stale reader, or
    /// a seam alignment). Each jump is crossfaded.
    quint64 realignments() const {return realignments_.load(std::memory_order_relaxed);}
    bool receiving() const;
private:
    quint64 fill() const;
    /// Producer: publishes the (position, media frame) pair the reader maps
    /// against. Seqlock, because the pair must be read as one.
    void publishMark(quint64 position,quint64 mediaFrame) noexcept;
    /// Reader: refreshes its private copy of the mark. Bounded retries; a
    /// contended read simply keeps the previous mark for this block.
    void refreshMark() noexcept;
    /// Reader: media frame of a ring position, from the private mark.
    std::optional<quint64> mediaAt(quint64 position) const noexcept;
    /// True when `frames` output frames starting at `position` are readable.
    bool readable(quint64 position,unsigned frames,quint64 floor,quint64 write) const noexcept;
    /// Producer: appends converted frames, overwriting the oldest when the
    /// ring is full, and publishes the mark for their end.
    void store(const float* samples,quint64 frames,quint64 mediaEnd);
    /// Reader: moves from `from` to `to`, spending at most `kFadeFrames` of
    /// this block's output on making the move inaudible. Returns the output
    /// frames consumed; playback continues at `to` plus that many.
    unsigned jump(float* out,quint64 from,quint64 to,unsigned frames,quint64 floor,quint64 write) noexcept;
    /// Crossfades `frames` output frames from `from` onto `to`. Returns the
    /// frames written; both positions must have that many frames readable.
    unsigned crossfade(float* out,quint64 from,quint64 to,unsigned frames) noexcept;
    void copyOut(float* out,quint64 position,unsigned frames) noexcept;
    /// Ramps the last sample emitted down to silence, then holds silence. Used
    /// for every starved tail, including one that starts with zero frames.
    void starve(float* out,unsigned frames) noexcept;
    // The producer may retire/overwrite a slot while a paused reader wakes.
    // Atomic samples make that overlap well-defined; the reader validates the
    // floor again after copying and discards the block if it was overtaken.
    std::unique_ptr<std::atomic<float>[]> ring_;
    const quint64 capacity_;
    std::atomic<quint64> writePos_{0},readPos_{0},floorPos_{0},resetGeneration_{0},underruns_{0},realignments_{0};
    std::atomic<qint64> lastWriteAt_{0},pendingAlign_{0};
    // Seqlock: the ring position whose sample is `markMedia_`. Written by the
    // producer, read by the audio thread and by snapshot callers.
    std::atomic<quint64> markSeq_{0},markPos_{0},markMedia_{0},markGeneration_{0};
    std::atomic<quint64> renderedMedia_{0};
    std::atomic<bool> marked_{false},rendering_{false};
    quint64 readerGeneration_=0,readMarkPos_=0,readMarkMedia_=0;
    bool primed_=false,fadeIn_=false,readMarked_=false;
    float tailLeft_=0,tailRight_=0;
    SRC_STATE_tag* src_=nullptr;
    unsigned inputRate_=0;
    // Producer-private continuity state of the stream being converted.
    quint64 sourceEpoch_=0,sourceGeneration_=0,expectedMedia_=0;
    bool converting_=false;
    AsrcController controller_;
    std::vector<float> converted_;
};
/// The one-shot Lite->Mac seam.
///
/// When this computer takes over from a Lite DJ, its first captured block of
/// local master is *carrying* that DJ's audio, delayed by whatever the JUNCTION
/// deck buffered. The only frame that block is truly at is the one the deck
/// reported rendering in the same realtime callback. The local session clock
/// says where the callback happened, which is that frame plus an unknown
/// buffer -- useful as a sanity band, never as the seam itself.
///
/// `resolve` therefore takes the rendered frame whenever it falls inside the
/// band a real seam can occupy, and only falls back to the clock when the deck
/// reported nothing (it was starved, so there is no remote audio to align to)
/// or reported something impossible.
struct TakeoverAnchor {
    /// `rendered`: what `JunctionInput::renderedMediaFrame()` returned in this
    /// callback. `elapsed`: the session frame the local clock puts it at.
    static quint64 resolve(std::optional<quint64> rendered,quint64 elapsed) noexcept {
        if(!rendered)return elapsed;
        const auto frame=*rendered;
        const bool plausible=frame<=elapsed?elapsed-frame<=JunctionInput::kMaxSeamLag48k
                                           :frame-elapsed<=JunctionInput::kMaxSeamLead48k;
        return plausible?frame:elapsed;
    }
};

/// When the new operator has faded the outgoing DJ out of the JUNCTION deck.
/// Controls that were already silent at takeover never count as a fade: the
/// deck must first have been audible to the new operator.
///
/// Two fallbacks keep an outgoing stream from living forever: a peer whose
/// audio has stopped arriving is already gone, and a hold that outlasts any
/// plausible mix is ended so the sender is not locked out of its own decks.
class InputRelease {
public:
    static constexpr qint64 kSilentNanos=1500000000LL;
    /// A stream that stopped arriving is treated as released. Old PlumDeck Lite
    /// builds cut their track the moment they lose the operator role.
    static constexpr qint64 kStreamEndedNanos=2000000000LL;
    /// Upper bound on the whole release, however the new operator mixes.
    static constexpr qint64 kMaxHoldNanos=300000000000LL; // 5 minutes.
    /// `heard`: the receiver already had the deck audible when the hold began
    /// (a fader-start DJ is READY only with J at unity).
    void begin(qint64 nowNanos,bool heard=false) {active_=true;heard_=heard;silentSince_=0;startedAt_=nowNanos;goneSince_=0;}
    void clear() {active_=false;heard_=false;silentSince_=0;startedAt_=0;goneSince_=0;}
    bool active() const {return active_;}
    /// True once the deck has stayed faded for `kSilentNanos` after being heard,
    /// or once one of the two fallbacks has expired.
    bool observe(bool audible,bool receiving,qint64 nowNanos) {
        if(!active_)return false;
        if(startedAt_&&nowNanos-startedAt_>=kMaxHoldNanos)return true;
        if(receiving)goneSince_=0;
        else if(!goneSince_)goneSince_=nowNanos;
        else if(nowNanos-goneSince_>=kStreamEndedNanos)return true;
        if(audible){heard_=true;silentSince_=0;return false;}
        if(!heard_)return false;
        if(!silentSince_){silentSince_=nowNanos;return false;}
        return nowNanos-silentSince_>=kSilentNanos;
    }
private:
    bool active_=false,heard_=false;
    qint64 silentSince_=0,startedAt_=0,goneSince_=0;
};
}
