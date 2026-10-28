/**
 * Held PAD FX pads. The Mixxx host has one effect slot per deck, so several
 * held pads resolve to "last pressed sounds"; releasing it brings back the
 * pad still held underneath instead of cutting the effect.
 * Pure module: runs under `node --test`.
 */
export type PadFxStack = { held: readonly number[] };
export type PadFxCommand = { slot: number; enabled: boolean };
export const emptyPadFxStack: PadFxStack = { held: [] };

const top = (held: readonly number[]) => held.length ? held[held.length - 1] : null;

export function pressPad(stack: PadFxStack, slot: number): { stack: PadFxStack; commands: PadFxCommand[] } {
  if (stack.held.includes(slot)) return { stack, commands: [] };
  return { stack: { held: [...stack.held, slot] }, commands: [{ slot, enabled: true }] };
}

export function releasePad(stack: PadFxStack, slot: number): { stack: PadFxStack; commands: PadFxCommand[] } {
  if (!stack.held.includes(slot)) return { stack, commands: [] };
  const sounding = top(stack.held);
  const held = stack.held.filter((item) => item !== slot);
  if (sounding !== slot) return { stack: { held }, commands: [] };
  const next = top(held);
  return { stack: { held }, commands: [next === null ? { slot, enabled: false } : { slot: next, enabled: true }] };
}

/** Release everything (mode change, focus loss). */
export function releaseAll(stack: PadFxStack): { stack: PadFxStack; commands: PadFxCommand[] } {
  const sounding = top(stack.held);
  return { stack: emptyPadFxStack, commands: sounding === null ? [] : [{ slot: sounding, enabled: false }] };
}
