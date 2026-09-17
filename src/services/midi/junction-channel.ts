import type { DeckId } from '../../types/dj-engine.ts';
import type { JunctionInputChannel, JunctionInputSettings, JunctionSnapshot } from '../../types/junction.ts';
import { tailLockReason } from '../junction/turn-state.ts';

/** Physical mixer channel that carries JUNCTION MASTER instead of its deck ('' = none). */
export type JunctionChannelAssign = '' | 'C' | 'D';
export const JUNCTION_CHANNEL_KEY = 'plumdeck.ddj1000.junctionChannel';

export function readJunctionChannel(value: string | null): JunctionChannelAssign {
  return value === 'C' || value === 'D' ? value : '';
}

/** Same knob curve as the deck EQ: centre is unity, full right is 4x. */
const eqGain = (value: number) => (value <= 0.5 ? value * 2 : 1 + (value - 0.5) * 6);
const eqPosition = (gain: number) => (gain <= 1 ? gain / 2 : 0.5 + (gain - 1) / 6);

/**
 * The J channel strip on the controller: LEVEL, EQ, crossfader assign and CUE.
 * Returns null for any control that is not part of that strip (the deck
 * section of the same side keeps controlling its deck).
 */
export function junctionChannelSettings(control: string, value: number, pressed: boolean | undefined, channel: JunctionInputChannel | undefined): JunctionInputSettings | null {
  if (control === 'gain') return {volume: Math.max(0, Math.min(1, value))};
  if (control === 'eqLow') return {eqLow: eqGain(value)};
  if (control === 'eqMid') return {eqMid: eqGain(value)};
  if (control === 'eqHigh') return {eqHigh: eqGain(value)};
  if (control.startsWith('assign')) return pressed ? {orientation: control === 'assignLeft' ? 0 : control === 'assignRight' ? 2 : 1} : null;
  if (control === 'pfl') return pressed ? {pfl: !channel?.pfl} : null;
  return null;
}

/** Soft-takeover target for a J strip control, in controller units (0..1). */
export function junctionChannelTarget(control: string, channel: JunctionInputChannel | undefined): number | undefined {
  if (!channel?.available) return undefined;
  if (control === 'gain') return channel.volume ?? 1;
  if (control === 'eqLow') return eqPosition(channel.eqLow ?? 1);
  if (control === 'eqMid') return eqPosition(channel.eqMid ?? 1);
  if (control === 'eqHigh') return eqPosition(channel.eqHigh ?? 1);
  return undefined;
}

/** The J strip is live only while this computer actually has J. */
export function junctionChannelActive(assign: JunctionChannelAssign, deck: DeckId | undefined, snapshot: JunctionSnapshot | null): boolean {
  return Boolean(assign && deck === assign && snapshot?.active && snapshot.junctionInput?.peerId);
}

const CONTROL_OP: Record<string, string> = {
  gain: 'mixer.channel.gain', trim: 'mixer.trim.set', eqLow: 'mixer.channel.eq', eqMid: 'mixer.channel.eq', eqHigh: 'mixer.channel.eq',
  assignLeft: 'mixer.channel.orientation', assignRight: 'mixer.channel.orientation', assignThru: 'mixer.channel.orientation',
  pfl: 'mixer.channel.pfl', crossfader: 'mixer.crossfader', padFx: 'mixer.fx.set',
  play: 'deck.play', cue: 'deck.play', faderStart: 'deck.play', faderStop: 'deck.play', start: 'deck.seek',
  jog: 'deck.scratch', platter: 'deck.scratch', touch: 'deck.scratch', nudge: 'deck.pitchbend', searchJog: 'deck.seek',
  searchBack: 'deck.seek', searchForward: 'deck.seek', cuePrevious: 'deck.seek', cueNext: 'deck.seek',
  tempo: 'deck.tempo.set', keylock: 'deck.keylock.set', keyShift: 'deck.key.shift', keyReset: 'deck.key.shift', keySync: 'deck.key.shift',
  sync: 'deck.sync.set', master: 'deck.sync.set', quantize: 'deck.quantize.set', slip: 'deck.slipReverse.set', reverse: 'deck.slipReverse.set', slipReverse: 'deck.slipReverse.set',
  hotcue: 'deck.hotcue.jump', load: 'deck.load', beatJumpPad: 'deck.beatjump',
  beatLoopPad: 'deck.loop.set', loop: 'deck.loop.set', loopIn: 'deck.loop.set', loopOut: 'deck.loop.set', loopHalf: 'deck.loop.set', loopDouble: 'deck.loop.set', reloop: 'deck.loop.enable',
};

/**
 * OUTGOING tail lock for a controller gesture. A locked gesture is dropped
 * silently (the controller keeps moving; the engine does not), so the caller
 * must also forget the pickup for that control.
 */
export function controllerTailLock(control: string, deck: DeckId | undefined, snapshot: JunctionSnapshot | null): string {
  const turn = snapshot?.active ? snapshot.turn : undefined;
  if (!turn || turn.signal !== 'outgoing') return '';
  const filterDeck = /^filter[A-D]$/.test(control) ? control.slice(-1) : undefined;
  const op = filterDeck ? 'mixer.filter.set' : CONTROL_OP[control];
  if (!op) return '';
  return tailLockReason(op, {deck: filterDeck ?? deck}, turn.tailDecks);
}
