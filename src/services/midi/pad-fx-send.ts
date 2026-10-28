import type { ChannelState, DeckId, PadEffect } from "../../types/dj-engine";
import { padFxParams, padPreset, PAD_FX_EXTENDED, type PadFxSlot } from "./pad-presets.ts";

/**
 * PAD FX use two engine slots per deck: the deck's effect chain (beat pads)
 * and the channel Color FX (color pads). Pads on different slots sound
 * together; pads on the same slot resolve to "last pressed sounds".
 */
export type PadFxGroup = "chain" | "color";
export const padFxGroup = (slot: PadFxSlot): PadFxGroup => slot.kind === "color" ? "color" : "chain";

type Snapshot = { engine: { capabilities: readonly string[] }; mixer: { channels: Partial<Record<DeckId, ChannelState>> } };
export type PadFxClient = {
  getState(): { snapshot?: Snapshot | null };
  setFx(deck: DeckId, effect: PadEffect, enabled: boolean, mix: number, depth?: number, beats?: number, parameters?: Record<string, number>): Promise<unknown>;
  setColorFx(deck: DeckId, effect: string, amount: number): Promise<unknown>;
  setFilter(deck: DeckId, value: number): Promise<unknown>;
};

export type PadFxSender = {
  send(deck: DeckId, slot: PadFxSlot, enabled: boolean): Promise<unknown>;
  /** Drop this source's held state without sending anything (new engine session). */
  forget(deck: DeckId): void;
  /**
   * The release can no longer reach the pad's own track (load, unload or a
   * Junction lease change). Chain FX were reset with the track, but COLOR FX
   * is channel state: give the knob back if this source still borrows it.
   */
  abandon(deck: DeckId): Promise<unknown>;
};

type ColorValue = { effect: string; amount: number };
/** Per deck: the knob value to give back, and each source's held color pad (insertion order = recency). */
type ColorHold = { knob: ColorValue | null; holders: Map<symbol, ColorValue> };
const colorHolds = new WeakMap<PadFxClient, Map<DeckId, ColorHold>>();

/**
 * Sends one pad press or release. A color pad borrows the channel's COLOR FX
 * while held; the knob's own type and position are captured on the first
 * color press and given back when the last color pad of the deck releases.
 * Mouse, keyboard and controller each create a sender (a source), but the
 * borrowed knob is shared per engine client, so overlapping color pads from
 * two sources never capture each other's pad as the knob value.
 */
export function createPadFxSender(client: PadFxClient): PadFxSender {
  let holds = colorHolds.get(client);
  if (!holds) { holds = new Map(); colorHolds.set(client, holds); }
  const store = holds;
  const source = Symbol("pad-fx-source");
  const apply = (deck: DeckId, value: ColorValue) => client.getState().snapshot?.engine?.capabilities.includes("mixer.colorfx")
    ? client.setColorFx(deck, value.effect, value.amount) : client.setFilter(deck, value.amount);
  const releaseColor = (deck: DeckId): Promise<unknown> => {
    const hold = store.get(deck);
    if (!hold) return apply(deck, { effect: "filter", amount: 0 });
    hold.holders.delete(source);
    // Another source still holds a color pad: it sounds again.
    const remaining = [...hold.holders.values()].pop();
    if (remaining) return apply(deck, remaining);
    const knob = hold.knob ?? { effect: "filter", amount: 0 };
    // Forget the knob value only once it is back, so a retried release restores it too.
    return apply(deck, knob).then((result) => { if (!hold.holders.size && store.get(deck) === hold) store.delete(deck); return result; });
  };
  const send = (deck: DeckId, slot: PadFxSlot, enabled: boolean): Promise<unknown> => {
    const snapshot = client.getState().snapshot;
    const capabilities = snapshot?.engine?.capabilities ?? [];
    if (slot.kind === "beat") {
      const params = padFxParams(slot, capabilities.includes(PAD_FX_EXTENDED));
      return client.setFx(deck, slot.effect, enabled, params.mix, params.depth, params.beats, params.params);
    }
    if (!enabled) return releaseColor(deck);
    let hold = store.get(deck);
    if (!hold) { hold = { knob: null, holders: new Map() }; store.set(deck, hold); }
    if (!hold.knob) {
      const channel = snapshot?.mixer.channels[deck];
      hold.knob = { effect: channel?.colorFx?.effect ?? "filter", amount: channel?.colorFx?.amount ?? channel?.filter ?? 0 };
    }
    hold.holders.delete(source);
    hold.holders.set(source, { effect: slot.effect, amount: slot.amount });
    return apply(deck, { effect: slot.effect, amount: slot.amount });
  };
  return {
    send,
    forget: (deck) => {
      const hold = store.get(deck);
      if (!hold) return;
      hold.holders.delete(source);
      if (!hold.holders.size) store.delete(deck);
    },
    abandon: (deck) => store.get(deck)?.holders.has(source) ? releaseColor(deck) : Promise.resolve(),
  };
}

/** Whether a slot is what the engine is sounding now (controller pad lamps). */
export function padFxSounding(slot: PadFxSlot, channel: ChannelState | undefined, capabilities: readonly string[] = []): boolean {
  if (!channel) return false;
  if (slot.kind === "beat") {
    if (!channel.fx?.enabled || channel.fx.effect !== slot.effect) return false;
    // Pads sharing an effect differ in beat length (and depth): light only the one sounding.
    const sent = padFxParams(slot, capabilities.includes(PAD_FX_EXTENDED));
    if (sent.beats !== undefined && channel.fx.beats !== undefined && Math.abs(channel.fx.beats - sent.beats) > 1e-6) return false;
    return channel.fx.depth === undefined || Math.abs(channel.fx.depth - sent.depth) < 1e-6;
  }
  return channel.colorFx ? channel.colorFx.effect === slot.effect && Math.abs(channel.colorFx.amount - slot.amount) < 0.005 : false;
}

/** Short names for engine effect ids, for an effect that no pad of the bank explains. */
const EFFECT_NAMES: Record<string, string> = {
  sliproll: "SLIP ROLL", roll: "ROLL", pitchshift: "PITCH", moogladder4filter: "LADDER", tremolo: "TRANS", autopan: "PAN",
  bitcrusher: "CRUSH", distortion: "DIST", lowcutecho: "LOW CUT ECHO", mtdelay: "MT DELAY", enigmajet: "ENIGMA JET",
  whitenoise: "NOISE", echo: "ECHO", reverb: "REVERB", flanger: "FLANGER", phaser: "PHASER", filter: "FILTER", spiral: "SPIRAL",
};
const beatText = (beats: number) => {
  if (beats >= 1) return String(Number(beats.toFixed(3)));
  const denominator = Math.round(1 / beats);
  return Math.abs(1 / denominator - beats) < 1e-6 ? `1/${denominator}` : String(Number(beats.toFixed(4)));
};

/**
 * What the PAD FX caption names: the pads of `bank` the engine is sounding
 * (chain and COLOR), in the pads' own short names. Null when nothing sounds.
 */
export function padFxCaption(bank: 1 | 5, channel: ChannelState | undefined, capabilities: readonly string[] = []): string | null {
  if (!channel) return null;
  const extended = capabilities.includes(PAD_FX_EXTENDED);
  const names = new Set<string>();
  for (let index = 0; index < 16; index++) {
    const slot = padPreset(bank, index, capabilities);
    if (!padFxSounding(slot, channel, capabilities)) continue;
    const beats = extended && slot.beats ? ` ${beatText(slot.beats)}` : "";
    names.add(`${slot.label}${slot.variant && !beats ? ` ${slot.variant}` : ""}${beats}`);
  }
  if (channel.fx?.enabled && ![...names].length) names.add(EFFECT_NAMES[channel.fx.effect] ?? channel.fx.effect.toUpperCase());
  return names.size ? [...names].join(" + ") : null;
}
