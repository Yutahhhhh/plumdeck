import { padPreset } from "./pad-presets.ts";
import { padFxSounding } from "./pad-fx-send.ts";
import type { DeckId, EngineSnapshot, MetersPayload } from "../../types/dj-engine";

const DECKS: DeckId[] = ["A", "B", "C", "D"];
export type MidiAction = { control: string; deck?: DeckId; value: number; pressed?: boolean; shift?: boolean; slot?: number; mode?: number; surface?: "platter" | "side"; vinyl?: boolean; search?: boolean };
/** Addresses verified against native/dj-engine-host/tests-node/fixtures/ddj1000-midi-map.json (official MIDI table).
 * Channel numbers here are zero-based. SRT mappings are deliberately excluded. */
export class Ddj1000Decoder {
  private msb = new Map<number, number>();
  private held = new Set<number>();
  private running = 0;
  private bytes: number[] = [];
  private sysex = false;
  reset() { this.msb.clear(); this.held.clear(); this.running = 0; this.bytes = []; this.sysex = false; }
  feed(bytes: readonly number[]): MidiAction[] {
    const actions: MidiAction[] = [];
    for (const byte of bytes) {
      if (!Number.isInteger(byte) || byte < 0 || byte > 255) continue;
      if (byte >= 0xf8) continue; // Real-time bytes may interrupt any message.
      if (byte === 0xf0) { this.sysex = true; this.running = 0; this.bytes = []; continue; }
      if (byte === 0xf7) { this.sysex = false; continue; }
      if (this.sysex) continue;
      if (byte & 0x80) { this.running = byte < 0xf0 ? byte : 0; this.bytes = []; continue; }
      if (!this.running) continue;
      this.bytes.push(byte);
      const size = [0xc0, 0xd0].includes(this.running & 0xf0) ? 1 : 2;
      if (this.bytes.length === size) {
        if (size === 2) { const action = this.decode(this.running, this.bytes[0], this.bytes[1]); if (action) actions.push(action); }
        this.bytes = [];
      }
    }
    return actions;
  }
  private decode(status: number, key: number, value: number): MidiAction | null {
    const channel = status & 15, type = status & 0xf0, deck = DECKS[channel];
    if (type === 0x90 || type === 0x80) {
      const pressed = type === 0x90 && value > 0, id = channel * 128 + key;
      if (pressed === this.held.has(id)) return null;
      if (pressed) this.held.add(id); else this.held.delete(id);
      if (channel >= 7 && channel <= 14) {
        const padDeck = DECKS[Math.floor((channel - 7) / 2)], mode = key >> 4;
        const page = (key >> 3) & 1, pad = key & 7, slot = page * 8 + pad;
        return { control: ["hotcue", "padFx", "beatJumpPad", "sampler", "keyboard", "padFx", "beatLoopPad", "keyShift"][mode], deck: padDeck, value, pressed, shift: channel % 2 === 0, slot, mode };
      }
      if (channel === 6) {
        if (key <= 3) return { control: "colorSelect", value: key, pressed };
        if (key >= 0x10 && key <= 0x13) return { control: "colorState", value: key - 0x10, pressed };
        if (key >= 0x46 && key <= 0x49) return { control: "load", deck: DECKS[key - 0x46], value, pressed };
        const global: Record<number, string> = { 0x65: "back", 0x66: "browseView", 0x7a: "browseView", 0x68: "related", 0x69: "samplerCue", 0x6e: "samplerCue", 0x63: "hardwareMasterCue", 0x62: "hardwareMasterCue" };
        if (global[key]) return { control: global[key], value, pressed };
      }
      if (channel === 4) {
        if ([0x4a,0x4b,0x66,0x6b].includes(key)) return {control: key === 0x4a ? "fxBeatDown" : key === 0x4b ? "fxBeatUp" : key === 0x66 ? "fxAuto" : "fxTap", value, pressed};
        if (key === 0x43) return {control:"fxRelease",value,pressed};
        if (key >= 0x20 && key <= 0x2d) return { control: "fxSelect", value: key - 0x20, pressed };
        if (key >= 0x10 && key <= 0x16) return { control: "fxAssign", value: key - 0x10, pressed };
        if (key === 0x47 || key === 0x46) return { control: key === 0x46 ? "fxState" : "fxToggle", value, pressed };
      }
      if (!deck) return null;
      const leftShift = [1,2,3,4,5,6,7,8], rightShift = [9,0x7a,0x7b,0x7c,0x7d,0x7e,0x7f,0];
      const pageMode = key >= 0x24 && key <= 0x2b ? key - 0x24 : key >= 0x2c && key <= 0x33 ? key - 0x2c : leftShift.includes(key) ? leftShift.indexOf(key) : rightShift.indexOf(key);
      if (pageMode >= 0 && ![0x26,0x2e].includes(key)) return {control:"padPage", deck, value: key >= 0x24 && key <= 0x2b || leftShift.includes(key) ? -1 : 1, pressed, mode:pageMode, shift:leftShift.includes(key)||rightShift.includes(key)};
      const controls: Record<number, string> = {
        0x0b: "play", 0x47: "play", 0x0c: "cue", 0x48: "start", 0x36: "touch", 0x67: "touch", 0x3f: "shift",
        0x65: "keySync", 0x1c: "keySync", 0x64: "keyReset", 0x1f: "keyReset",
        0x1a: "keylock", 0x60: "tempoRange", 0x58: "sync", 0x5c: "master", 0x35: "quantize",
        0x3d: "memorySave", 0x3e: "memoryDelete", 0x4c: "loopInAdjust", 0x66: "faderStart", 0x52: "faderStop",
        0x10: "loopIn", 0x11: "loopOut", 0x4d: "reloop", 0x14: "loop", 0x50: "reloop",
        // 0x72's status/channel disagreement in the official table remains
        // unresolved; preserve its existing routing until captured on hardware.
        0x54: "pfl", 0x17: "vinyl", 0x72: "deckSelect",
        0x5e: "previous", 0x5f: "next", 0x70: "searchBack", 0x71: "searchForward", 0x51: "cuePrevious", 0x53: "cueNext",
        0x1b: "hotcueMode", 0x1e: "fxMode", 0x20: "jumpMode", 0x6d: "loopMode", 0x6b: "fxMode",
        0x22: "sampler", 0x69: "keyboard", 0x6f: "keyShift", 0x40: "slip", 0x15: "slipReverse", 0x38: "reverse",
        0x26: "jumpRangeDown", 0x2e: "jumpRangeUp",
        0x16: "assignLeft", 0x1d: "assignThru", 0x18: "assignRight",
      };
      return controls[key] ? { control: controls[key], deck, value, pressed, shift: this.held.has(channel * 128 + 0x3f), ...(key === 0x6b ? {mode:5} : key === 0x1e ? {mode:1} : {}) } : null;
    }
    if (type !== 0xb0) return null;
    if (channel === 6 && [0x40, 0x64].includes(key)) return { control: key === 0x40 ? "browse" : "zoom", value: value < 64 ? value : value - 128 };
    if (deck) {
      const jog: Record<number, Partial<MidiAction>> = {
        0x22: { control: "jog", surface: "platter", vinyl: true },
        0x23: { control: "jog", surface: "platter", vinyl: false },
        0x21: { control: "nudge", surface: "side" },
        0x26: { control: "searchJog", surface: "side", shift: true },
        0x29: { control: "searchJog", surface: "platter", search: true },
        0x1f: { control: "jog", surface: "platter", shift: true },
      };
      if (jog[key]) return { control: "jog", deck, value: value - 64, ...jog[key] };
    }
    // Do not emit a coarse MSB step before its LSB arrives. Each pair is consumed
    // once; stale coarse values cannot turn unrelated CCs into jumps.
    const base = key >= 32 ? key - 32 : key;
    const controls: Record<number, string> = channel === 6
      ? { 3: "samplerGain", 0x1f: "crossfader", 0x17: "filterA", 0x18: "filterB", 0x19: "filterC", 0x1a: "filterD" }
      : channel === 4 ? { 2: "fxMix" } : deck ? { 0: "tempo", 5: "tempo", 4: "trim", 7: "eqHigh", 11: "eqMid", 15: "eqLow", 19: "gain" } : {};
    if (!controls[base]) return null;
    const id = channel * 32 + base;
    if (key < 32) { this.msb.set(id, value); return null; }
    const high = this.msb.get(id); if (high === undefined) return null;
    this.msb.delete(id);
    return { control: controls[base], deck, value: (high * 128 + value) / 16383 };
  }
}

/** Delta feedback is handled by the transport. Always derive LEDs from the
 * acknowledged engine state, so mouse and hardware controls stay in sync. */
/** Set once the owner has checked B0+ch CC 02 on DDJ-1000 hardware. */
export const DDJ1000_LEVEL_CC_VERIFIED = false;

export function ddj1000Feedback(snapshot: EngineSnapshot | null, meters?: MetersPayload | null): number[][] {
  const out: number[][] = [];
  for (let ch = 0; ch < 4; ch++) {
    const deck = snapshot?.decks[DECKS[ch]], mixer = snapshot?.mixer.channels[DECKS[ch]];
    const loaded = Boolean(deck?.track), playing = deck?.status === "playing";
    const note = (key: number, on: boolean | number) => out.push([0x90 + ch, key, typeof on === "boolean" ? on ? 127 : 0 : on]);
    const cc14 = (key: number, value: number, max = 16383) => { const v = Number.isFinite(value) ? Math.max(0, Math.min(max, Math.round(value))) : 0; out.push([0xb0 + ch, key, v >> 7], [0xb0 + ch, key + 32, v & 127]); };
    note(0x0b, playing); note(0x47, playing); note(0x0c, loaded && !playing);
    note(0x1a, Boolean(deck?.keylock)); note(0x58, Boolean(deck?.syncEnabled)); note(0x5a, Boolean(deck?.syncEnabled));
    note(0x59, loaded && deck?.syncLeader === DECKS[ch]);
    note(0x35, Boolean(deck?.quantize)); note(0x54, Boolean(mixer?.pfl));
    for (const key of [0x10, 0x11, 0x14, 0x4d]) note(key, Boolean(deck?.loopRegion?.enabled));
    note(0x5b, loaded ? 1 : 0); note(0x5d, loaded ? 0 : 127);
    const seconds = Math.max(0, Math.floor((deck?.positionMs ?? 0) / 1000));
    note(0x42, Math.min(99, Math.floor(seconds / 60))); note(0x43, seconds % 60); note(0x44, 0);
    // The official maximum is 0x4E/0x0F (9999), not the full 14-bit range.
    const displayBpm = deck?.track?.bpm && deck.track.bpm > 0
      ? deck.track.bpm * (deck.rate ?? 1) : deck?.effectiveBpm ?? 0;
    cc14(0x15, displayBpm * 10, 9999);
    cc14(0x16, (deck?.rate ?? 1) * 5000, 9999);
    cc14(0x14, ((deck?.positionMs ?? 0) % 1800 + 1800) % 1800 / 1800 * 359);
    for (let slot = 0; slot < 8; slot++) {
      // Manufacturer's pad channels are 8/10/12/14 (one-based).
      out.push([0x97 + ch * 2, slot, deck?.hotCues[slot] != null ? 127 : 0]);
    }

    for (let slot=0;slot<8;slot++) {
      out.push([0x97+ch*2,0x10+slot,loaded ? padFxSounding(padPreset(1,slot,snapshot?.engine?.capabilities),mixer,snapshot?.engine?.capabilities) ? 127:32:0]);
      for (const mode of [2,6]) out.push([0x97+ch*2,(mode<<4)+slot,loaded?32:0]);
    }
    // Legacy, undocumented output: the official table does NOT define a deck
    // level meter. Retained pending visual hardware verification; B4 02/22 is
    // BEAT FX LEVEL/DEPTH input and must not be substituted for this address.
    // Until verified it keeps sending 0, as it did before hosts had meters.
    const peak = DDJ1000_LEVEL_CC_VERIFIED ? meters?.channels[DECKS[ch]]?.peak ?? 0 : 0;
    out.push([0xb0 + ch, 2, Math.round(Math.max(0, Math.min(1, peak)) * 127)]);
    out.push([0x9f, ch, loaded ? 127 : 0]);
  }
  return out;
}

/** Additional pad pages and status lamps, sent by the transport in bounded batches. */
export function ddj1000ExtendedFeedback(snapshot: EngineSnapshot | null, sampler?: {slots:{status:string}[];pfl:boolean}): number[][] {
  const out:number[][]=[];
  for(let ch=0;ch<4;ch++) {
    const deck=snapshot?.decks[DECKS[ch]], mixer=snapshot?.mixer.channels[DECKS[ch]], loaded=Boolean(deck?.track);
    const note=(key:number,on:boolean)=>out.push([0x90+ch,key,on?127:0]);
    for(let slot=8;slot<16;slot++) out.push([0x97+ch*2,slot,deck?.hotCues[slot]!=null?127:0]);
    for (let slot = 0; slot < 16; slot++) {
      for (const mode of [1,5]) out.push([0x97+ch*2,(mode<<4)+slot,loaded ? padFxSounding(padPreset(mode,slot,snapshot?.engine?.capabilities),mixer,snapshot?.engine?.capabilities) ? 127:32:0]);
      for (const mode of [2,6]) out.push([0x97+ch*2,(mode<<4)+slot,loaded?32:0]);
      for (const mode of [4,7]) out.push([0x97+ch*2,(mode<<4)+slot,loaded?32:0]);
      const sample=sampler?.slots[(slot&7)+(ch%2===0?8:0)];
      out.push([0x97+ch*2,0x30+slot,sample?.status==="playing"?127:sample?.status==="ready"?32:0]);
    }
    note(0x40,Boolean(deck?.slip)); note(0x15,Boolean(deck?.slipReverse)); note(0x38,Boolean(deck?.reverse));
  }
  if (sampler) out.push([0x96,0x69,sampler.pfl?127:0]);
  if (snapshot?.mixer.beatFx) out.push([0x94,0x47,snapshot.mixer.beatFx.enabled?127:0]);
  return out;
}
