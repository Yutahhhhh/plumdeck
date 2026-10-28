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

/** Acknowledged engine state is separate from physically held pads. A failed
 * switch can leave the old effect sounding even after its pad was released. */
export type PadFxActiveState<T> = { pads: T[]; sounding: T | null };

export async function pressFx<T>(state: PadFxActiveState<T>, pad: T, same: (a: T, b: T) => boolean, send: (pad: T, enabled: boolean) => Promise<unknown>) {
  if (state.pads.some(item => same(item, pad))) return;
  try {
    await send(pad, true);
  } catch (error) {
    // A failed ON may have reached the host; the client attempts to turn it
    // back off. If both pads use the same effect, that also stops the previous
    // pad, so restore it while it remains held.
    const previous = state.sounding;
    if (previous && state.pads.some(item => same(item, previous))) {
      try { await send(previous, true); } catch { /* Preserve the original error and retry cleanup on release. */ }
    }
    throw error;
  }
  state.pads.push(pad);
  state.sounding = pad;
}

/** Retry one failed OFF immediately; retain the last acknowledged effect if
 * both attempts fail so a later release or cleanup can try again. */
async function stopSoundingFx<T>(state: PadFxActiveState<T>, send: (pad: T, enabled: boolean) => Promise<unknown>) {
  const sounding = state.sounding;
  if (!sounding) return;
  try {
    await send(sounding, false);
  } catch {
    try { await send(sounding, false); }
    catch (error) { throw Object.assign(new Error("PAD FX の解除に失敗しました。もう一度解除してください。"), { cause: error }); }
  }
  state.sounding = null;
}

export async function releaseFx<T>(state: PadFxActiveState<T>, pad: T, same: (a: T, b: T) => boolean, send: (pad: T, enabled: boolean) => Promise<unknown>) {
  const index = state.pads.findIndex(item => same(item, pad));
  if (index < 0) {
    if (!state.pads.length) await stopSoundingFx(state, send);
    return;
  }
  const wasTop = index === state.pads.length - 1;
  state.pads.splice(index, 1);
  // If a switch failed, the released pad can still be the actual effect even
  // when it is no longer at the top of the held stack.
  if (!wasTop && !(state.sounding && same(state.sounding, pad))) return;
  const next = state.pads[state.pads.length - 1];
  if (next) {
    await send(next, true);
    state.sounding = next;
  } else {
    await stopSoundingFx(state, send);
  }
}

export async function releaseAllFx<T>(state: PadFxActiveState<T>, send: (pad: T, enabled: boolean) => Promise<unknown>) {
  state.pads.length = 0;
  await stopSoundingFx(state, send);
}

/** Serializes momentary UI gestures even when the engine reply is delayed. */
export type PadFxQueue = { tail: Promise<unknown> };
export function enqueuePadFx(queue: PadFxQueue, task: () => Promise<void>, onError?: (error: unknown) => void) {
  queue.tail = queue.tail.then(task).catch(error => { onError?.(error); });
  return queue.tail;
}
