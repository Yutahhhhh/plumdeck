import type { PadEffect } from "../../types/dj-engine";

/**
 * PAD FX slot definition. Mouse pads and controller pads read the same slot,
 * so both send identical values (no separate DEPTH slider on screen).
 * This is the slot contract the Rust engine is expected to take over.
 */
export type PadFxSlot = {
  kind: "beat" | "color" | "release";
  effect: PadEffect;
  /** Short plumdeck name shown on the pad. */
  label: string;
  /** Qualifier shown when no beat value applies (HPF, LONG …). */
  variant?: string;
  /** Beat length; sent and shown only when the host accepts per-pad beats. */
  beats?: number;
  /** Dry/wet. Insert types (filter, crush …) always use 1.0 so no dry leaks. */
  mix: number;
  /** Effect macro (intensity). */
  depth: number;
  /** Insert types replace the dry signal instead of adding to it. */
  insert: boolean;
  /** Latching pad (click on/off) instead of momentary. */
  hold: boolean;
};
export type PadPreset = PadFxSlot;

/** Effects that process the dry signal in place; mixing dry back in only weakens them. */
export const INSERT_EFFECTS: readonly PadEffect[] = ["filter", "moogladder4filter", "bitcrusher", "distortion", "autopan"];

const slot = (effect: PadEffect, label: string, options: Partial<Omit<PadFxSlot, "effect" | "label">> = {}): PadFxSlot => {
  const insert = INSERT_EFFECTS.includes(effect);
  return { kind: "beat", effect, label, mix: insert ? 1 : options.mix ?? 0.5, depth: options.depth ?? 0.5, insert, hold: false, ...options, ...(insert ? { mix: 1 } : {}) };
};

// plumdeck's own default banks (the official software publishes no default
// assignment). Built from the effects the current host can load; ROLL, TRANS,
// SPIRAL and the RELEASE pads follow once the host exposes those processors.
// The Mixxx filter macro is centred at 0.5 (bypass): below sweeps LPF, above HPF.
const BANKS: Record<1 | 5, PadFxSlot[]> = {
  1: [
    slot("echo", "ECHO", { beats: 1 / 2, mix: 0.5, depth: 0.5 }),
    slot("echo", "ECHO", { beats: 3 / 4, mix: 0.55, depth: 0.6 }),
    slot("reverb", "REVERB", { variant: "SHORT", mix: 0.45, depth: 0.45 }),
    slot("flanger", "FLANGER", { beats: 16, mix: 0.6, depth: 0.7 }),
    slot("phaser", "PHASER", { beats: 1 / 4, mix: 0.6, depth: 0.7 }),
    slot("filter", "FILTER", { variant: "HPF", depth: 0.82 }),
    slot("bitcrusher", "CRUSH", { depth: 0.6 }),
    slot("autopan", "PAN", { beats: 1 / 2, depth: 0.8 }),
    slot("echo", "ECHO", { beats: 1, mix: 0.6, depth: 0.65 }),
    slot("reverb", "REVERB", { variant: "60%", mix: 0.6, depth: 0.7 }),
    slot("flanger", "FLANGER", { beats: 8, mix: 0.7, depth: 0.85 }),
    slot("phaser", "PHASER", { beats: 1, mix: 0.7, depth: 0.85 }),
    slot("filter", "FILTER", { variant: "LPF", depth: 0.18 }),
    slot("distortion", "DIST", { depth: 0.55 }),
    slot("moogladder4filter", "LADDER", { variant: "LPF", depth: 0.2 }),
    slot("autopan", "PAN", { beats: 1 / 4, depth: 0.9 }),
  ],
  5: [
    slot("echo", "ECHO", { beats: 1 / 4, mix: 0.5, depth: 0.55 }),
    slot("reverb", "REVERB", { variant: "LONG", mix: 0.75, depth: 0.9 }),
    slot("moogladder4filter", "LADDER", { variant: "LPF", depth: 0.25 }),
    slot("bitcrusher", "CRUSH", { variant: "HARD", depth: 0.85 }),
    slot("phaser", "PHASER", { beats: 4, mix: 0.65, depth: 0.75 }),
    slot("flanger", "FLANGER", { beats: 16, mix: 0.75, depth: 0.9 }),
    slot("filter", "FILTER", { variant: "HPF+", depth: 0.92 }),
    slot("distortion", "DIST", { variant: "HARD", depth: 0.85 }),
    slot("echo", "ECHO", { beats: 2, mix: 0.7, depth: 0.75 }),
    slot("reverb", "REVERB", { variant: "WASH", mix: 0.9, depth: 1 }),
    slot("moogladder4filter", "LADDER", { variant: "HPF", depth: 0.78 }),
    slot("autopan", "PAN", { beats: 1, depth: 1 }),
    slot("phaser", "PHASER", { beats: 1 / 2, mix: 0.6, depth: 0.6 }),
    slot("flanger", "FLANGER", { beats: 4, mix: 0.55, depth: 0.6 }),
    slot("filter", "FILTER", { variant: "LPF+", depth: 0.08 }),
    slot("echo", "ECHO", { beats: 1 / 8, mix: 0.45, depth: 0.5 }),
  ],
};

/** Mode 1 = PAD FX 1, mode 5 = PAD FX 2 (controller mode numbering); slot 0..15. */
export function padPreset(mode = 1, slot = 0): PadFxSlot {
  return BANKS[mode === 5 ? 5 : 1][slot & 15];
}

/** Values sent for a slot. `beats` only when the host declares per-pad beats. */
export function padFxParams(preset: PadFxSlot, beatsSupported: boolean): { mix: number; depth: number; beats?: number } {
  return { mix: preset.insert ? 1 : preset.mix, depth: preset.depth, ...(beatsSupported && preset.beats ? { beats: preset.beats } : {}) };
}

export const BEAT_LOOP_PAGES = [[.125,.25,.5,1,2,4,8,16],[.125,.25,.5,1,8,16,32,64]];
export const BEAT_JUMP_PAGES = [[-1,1,-2,2,-4,4,-8,8],[-8,8,-16,16,-32,32,-64,64]];
export function keyPad(page: number, pad: number): number | null {
  const value = [-8,0][page] + (pad & 7);
  return Number.isFinite(value) && value <= 12 ? value : null;
}
