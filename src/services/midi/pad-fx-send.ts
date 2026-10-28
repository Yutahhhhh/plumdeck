import type { ChannelState, DeckId, PadEffect } from "../../types/dj-engine";
import { padFxParams, PAD_FX_EXTENDED, type PadFxSlot } from "./pad-presets.ts";

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
  /** Drop a captured knob value when its release can no longer be sent (new track or session). */
  forget(deck: DeckId): void;
};
/**
 * Sends one pad press or release. A color pad borrows the channel's COLOR FX
 * while held; the knob's own type and position are captured on the first
 * color press and given back when the last color pad of the deck releases.
 */
export function createPadFxSender(client: PadFxClient): PadFxSender {
  const restore = new Map<DeckId, { effect: string; amount: number }>();
  const send = (deck: DeckId, slot: PadFxSlot, enabled: boolean): Promise<unknown> => {
    const snapshot = client.getState().snapshot;
    const capabilities = snapshot?.engine.capabilities ?? [];
    if (slot.kind === "beat") {
      const params = padFxParams(slot, capabilities.includes(PAD_FX_EXTENDED));
      return client.setFx(deck, slot.effect, enabled, params.mix, params.depth, params.beats, params.params);
    }
    const selectable = capabilities.includes("mixer.colorfx");
    const apply = (effect: string, amount: number) => selectable ? client.setColorFx(deck, effect, amount) : client.setFilter(deck, amount);
    if (enabled) {
      if (!restore.has(deck)) {
        const channel = snapshot?.mixer.channels[deck];
        restore.set(deck, { effect: channel?.colorFx?.effect ?? "filter", amount: channel?.colorFx?.amount ?? channel?.filter ?? 0 });
      }
      return apply(slot.effect, slot.amount);
    }
    const previous = restore.get(deck) ?? { effect: "filter", amount: 0 };
    // Forget the knob value only once it is back, so a retried release restores it too.
    return apply(previous.effect, previous.amount).then((result) => { restore.delete(deck); return result; });
  };
  return { send, forget: (deck) => { restore.delete(deck); } };
}

/** Whether a slot is what the engine is sounding now (controller pad lamps). */
export function padFxSounding(slot: PadFxSlot, channel: ChannelState | undefined): boolean {
  if (!channel) return false;
  if (slot.kind === "beat") return Boolean(channel.fx?.enabled && channel.fx.effect === slot.effect);
  return channel.colorFx ? channel.colorFx.effect === slot.effect && Math.abs(channel.colorFx.amount - slot.amount) < 0.005 : false;
}
