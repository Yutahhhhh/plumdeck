import type { PadEffect } from "../../types/dj-engine";

/**
 * Host capability for the PAD FX upgrade (H-8): `mixer.fx.set` takes per-pad
 * `beats` and `params`, and pads can load the host's beat processors (ROLL,
 * TRANS gate, SPIRAL, SLIP ROLL, PITCH …) besides the generic effects.
 */
export const PAD_FX_EXTENDED = "mixer.fx.beats";
/** contract (Rust): RELEASE pads (ECHO OUT / BACK SPIN / VINYL BRAKE). */
export const PAD_FX_RELEASE = "mixer.fx.release";

type SlotBase = {
  /** Short plumdeck name shown on the pad. */
  label: string;
  /** Qualifier shown when no beat value applies (HPF, LONG …). */
  variant?: string;
  /** Beat length; sent and shown only when the host accepts per-pad beats. */
  beats?: number;
  /** Latching pad (click on/off) instead of momentary. */
  hold: boolean;
  /** Capabilities the slot needs; without them the bank's fallback slot is used. */
  requires?: readonly string[];
};

/** An effect on the deck's PAD FX chain. */
export type BeatPadSlot = SlotBase & {
  kind: "beat";
  effect: PadEffect;
  /** Dry/wet. Insert types (filter, crush, gate …) always use 1.0 so no dry leaks. */
  mix: number;
  /** Effect macro (intensity). */
  depth: number;
  /** Insert types replace the dry signal instead of adding to it. */
  insert: boolean;
  /** Native effect parameters (host H-8 only), e.g. echo feedback. */
  params?: Record<string, number>;
};

/** Color FX the pad borrows from the channel while held (COLOR knob position −1..1). */
export type ColorPadEffect = "filter" | "echo" | "whitenoise";
export type ColorPadSlot = SlotBase & { kind: "color"; effect: ColorPadEffect; amount: number };

/**
 * PAD FX slot definition. Mouse pads and controller pads read the same slot,
 * so both send identical values (no separate DEPTH slider on screen).
 * This is the slot contract the Rust engine is expected to take over.
 */
export type PadFxSlot = BeatPadSlot | ColorPadSlot;
export type PadPreset = PadFxSlot;

/** Effects that process the dry signal in place; mixing dry back in only weakens them. */
export const INSERT_EFFECTS: readonly PadEffect[] = ["filter", "moogladder4filter", "bitcrusher", "distortion", "autopan", "tremolo", "roll", "sliproll", "pitchshift"];

type BeatOptions = Partial<Omit<BeatPadSlot, "kind" | "effect" | "label" | "insert">>;
const slot = (effect: PadEffect, label: string, options: BeatOptions = {}): BeatPadSlot => {
  const insert = INSERT_EFFECTS.includes(effect);
  return { kind: "beat", effect, label, mix: options.mix ?? 0.5, depth: options.depth ?? 0.5, insert, hold: false, ...options, ...(insert ? { mix: 1 } : {}) };
};
const color = (effect: ColorPadEffect, label: string, amount: number, variant?: string): ColorPadSlot => ({
  kind: "color", effect, label, amount, variant, hold: false,
  requires: effect === "filter" ? ["mixer.filter"] : ["mixer.colorfx"],
});
/** Needs the host's PAD FX upgrade (beat processors, beats and params). */
const extended = (pad: BeatPadSlot): BeatPadSlot => ({ ...pad, requires: [PAD_FX_EXTENDED] });

// plumdeck's own default banks, following the Play redesign (2.7.1). The
// official software publishes no default assignment, and the owner's Pad
// Editor assignment is not readable from the library data. Each entry is
// [slot, fallback]: the fallback keeps the pad working on hosts without the
// slot's capability. Slots that need the Rust engine (HELIX, REV ROLL, SWEEP,
// VINYL BRAKE and the RELEASE pads) stay on their fallback until then.
// The Mixxx filter macro is centred at 0.5 (bypass): below sweeps LPF, above HPF.
type Entry = readonly [PadFxSlot, BeatPadSlot];
const BANKS: Record<1 | 5, readonly Entry[]> = {
  1: [
    [slot("echo", "ECHO", { beats: 1 / 2, mix: 0.5, depth: 0.5 }), slot("echo", "ECHO", { beats: 1 / 2, mix: 0.5, depth: 0.5 })],
    [slot("echo", "ECHO", { beats: 3 / 4, mix: 0.55, depth: 0.6 }), slot("echo", "ECHO", { beats: 3 / 4, mix: 0.55, depth: 0.6 })],
    [extended(slot("roll", "ROLL", { beats: 1 / 4, depth: 1 })), slot("reverb", "REVERB", { variant: "SHORT", mix: 0.45, depth: 0.45 })],
    [extended(slot("roll", "ROLL", { beats: 1 / 8, depth: 1 })), slot("flanger", "FLANGER", { beats: 16, mix: 0.6, depth: 0.7 })],
    [extended(slot("tremolo", "TRANS", { beats: 1 / 4, depth: 1 })), slot("phaser", "PHASER", { beats: 1 / 4, mix: 0.6, depth: 0.7 })],
    [extended(slot("tremolo", "TRANS", { beats: 1 / 8, depth: 1 })), slot("filter", "FILTER", { variant: "HPF", depth: 0.82 })],
    [color("filter", "FILTER", 0.6, "HPF"), slot("bitcrusher", "CRUSH", { depth: 0.6 })],
    // RELEASE: ECHO OUT 1/2 (Rust)
    [slot("autopan", "PAN", { beats: 1 / 2, depth: 0.8 }), slot("autopan", "PAN", { beats: 1 / 2, depth: 0.8 })],
    [slot("echo", "ECHO", { beats: 1, mix: 0.6, depth: 0.65 }), slot("echo", "ECHO", { beats: 1, mix: 0.6, depth: 0.65 })],
    [extended(slot("spiral", "SPIRAL", { beats: 1 / 2, mix: 0.6, depth: 0.7 })), slot("reverb", "REVERB", { variant: "60%", mix: 0.6, depth: 0.7 })],
    [extended(slot("sliproll", "SLIP ROLL", { beats: 1 / 8, depth: 1 })), slot("flanger", "FLANGER", { beats: 8, mix: 0.7, depth: 0.85 })],
    [extended(slot("sliproll", "SLIP ROLL", { beats: 1 / 16, depth: 1 })), slot("phaser", "PHASER", { beats: 1, mix: 0.7, depth: 0.85 })],
    [slot("reverb", "REVERB", { variant: "60%", mix: 0.6, depth: 0.7 }), slot("filter", "FILTER", { variant: "LPF", depth: 0.18 })],
    [slot("flanger", "FLANGER", { beats: 16, mix: 0.6, depth: 0.7 }), slot("distortion", "DIST", { depth: 0.55 })],
    [color("filter", "FILTER", -0.6, "LPF"), slot("moogladder4filter", "LADDER", { variant: "LPF", depth: 0.2 })],
    // RELEASE: VINYL BRAKE 1 (Rust)
    [slot("autopan", "PAN", { beats: 1 / 4, depth: 0.9 }), slot("autopan", "PAN", { beats: 1 / 4, depth: 0.9 })],
  ],
  5: [
    [slot("echo", "ECHO", { beats: 1 / 4, mix: 0.5, depth: 0.55 }), slot("echo", "ECHO", { beats: 1 / 4, mix: 0.5, depth: 0.55 })],
    [extended(slot("echo", "PING PONG", { beats: 1 / 2, mix: 0.55, depth: 0.6, params: { pingpong_amount: 1 } })), slot("reverb", "REVERB", { variant: "LONG", mix: 0.75, depth: 0.9 })],
    [extended(slot("roll", "ROLL", { beats: 1 / 16, depth: 1 })), slot("moogladder4filter", "LADDER", { variant: "LPF", depth: 0.25 })],
    [extended(slot("tremolo", "TRANS", { beats: 1 / 16, depth: 1 })), slot("bitcrusher", "CRUSH", { variant: "HARD", depth: 0.85 })],
    [color("whitenoise", "NOISE", 0.5), slot("phaser", "PHASER", { beats: 4, mix: 0.65, depth: 0.75 })],
    [slot("bitcrusher", "CRUSH", { depth: 0.6 }), slot("flanger", "FLANGER", { beats: 16, mix: 0.75, depth: 0.9 })],
    [extended(slot("pitchshift", "PITCH", { variant: "+", depth: 0.75 })), slot("filter", "FILTER", { variant: "HPF+", depth: 0.92 })],
    // RELEASE: BACK SPIN 1 (Rust)
    [slot("distortion", "DIST", { variant: "HARD", depth: 0.85 }), slot("distortion", "DIST", { variant: "HARD", depth: 0.85 })],
    [slot("reverb", "REVERB", { variant: "LONG", mix: 0.75, depth: 0.9 }), slot("echo", "ECHO", { beats: 2, mix: 0.7, depth: 0.75 })],
    [color("echo", "DUB ECHO", 0.6), slot("reverb", "REVERB", { variant: "WASH", mix: 0.9, depth: 1 })],
    [slot("phaser", "PHASER", { beats: 4, mix: 0.65, depth: 0.75 }), slot("moogladder4filter", "LADDER", { variant: "HPF", depth: 0.78 })],
    // HELIX (Rust)
    [slot("autopan", "PAN", { beats: 1, depth: 1 }), slot("autopan", "PAN", { beats: 1, depth: 1 })],
    // REV ROLL 1/2 (Rust)
    [slot("phaser", "PHASER", { beats: 1 / 2, mix: 0.6, depth: 0.6 }), slot("phaser", "PHASER", { beats: 1 / 2, mix: 0.6, depth: 0.6 })],
    [extended(slot("echo", "DELAY", { beats: 1, mix: 0.6, depth: 0.6, params: { feedback_amount: 0 } })), slot("flanger", "FLANGER", { beats: 4, mix: 0.55, depth: 0.6 })],
    // SWEEP (Rust)
    [slot("filter", "FILTER", { variant: "LPF+", depth: 0.08 }), slot("filter", "FILTER", { variant: "LPF+", depth: 0.08 })],
    // RELEASE: VINYL BRAKE 1/2 (Rust)
    [slot("echo", "ECHO", { beats: 1 / 8, mix: 0.45, depth: 0.5 }), slot("echo", "ECHO", { beats: 1 / 8, mix: 0.45, depth: 0.5 })],
  ],
};

/**
 * Mode 1 = PAD FX 1, mode 5 = PAD FX 2 (controller mode numbering); slot 0..15.
 * The slot shown and sent depends on what the host declares.
 */
export function padPreset(mode = 1, slot = 0, capabilities: readonly string[] = []): PadFxSlot {
  const [preferred, fallback] = BANKS[mode === 5 ? 5 : 1][slot & 15];
  return (preferred.requires ?? []).every((name) => capabilities.includes(name)) ? preferred : fallback;
}

/** Values sent for a chain slot. `beats` and `params` only when the host declares them. */
export function padFxParams(preset: BeatPadSlot, extendedHost: boolean): { mix: number; depth: number; beats?: number; params?: Record<string, number> } {
  return {
    mix: preset.insert ? 1 : preset.mix, depth: preset.depth,
    ...(extendedHost && preset.beats ? { beats: preset.beats } : {}),
    ...(extendedHost && preset.params ? { params: preset.params } : {}),
  };
}

export const BEAT_LOOP_PAGES = [[.125,.25,.5,1,2,4,8,16],[.125,.25,.5,1,8,16,32,64]];
export const BEAT_JUMP_PAGES = [[-1,1,-2,2,-4,4,-8,8],[-8,8,-16,16,-32,32,-64,64]];
export function keyPad(page: number, pad: number): number | null {
  const value = [-8,0][page] + (pad & 7);
  return Number.isFinite(value) && value <= 12 ? value : null;
}
