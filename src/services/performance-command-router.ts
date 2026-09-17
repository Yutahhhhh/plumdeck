import { junctionState } from './junction/state.ts';
import { tailLockReason } from './junction/turn-state.ts';
const local = new Set(['performance.endpoint','engine.clock.probe','waveform.ensure','waveform.manifest','waveform.requestRange','waveform.cancelRequest','waveform.invalidate','engine.ping','state.snapshot','audio.devices.list','audio.config.get','meters.subscribe','mixer.channel.pfl','mixer.headphone.gain','mixer.headphone.mix','sampler.state','sampler.pfl','recording.directory.set','recording.format.set']);
const shared = new Set(['deck.key.shift','deck.load','deck.unload','deck.play','deck.pause','deck.seek','deck.scratch','deck.pitchbend','deck.tempo.set','deck.beatgrid.set','deck.keylock.set','deck.sync.set','deck.hotcue.set','deck.hotcue.jump','deck.hotcue.clear','deck.loop.set','deck.loop.enable','deck.beatjump','deck.loop.beats','deck.quantize.set','deck.slipReverse.set','mixer.filter.set','mixer.trim.set','mixer.fx.set','mixer.channel.gain','mixer.channel.eq','mixer.channel.orientation','mixer.crossfader','mixer.master.gain','mixer.colorfx.set','mixer.beatfx.set','sampler.eject','sampler.load','sampler.unload','sampler.play','sampler.stop','sampler.gain','sampler.bank','sampler.trigger','sampler.set']);
/** Early UX guard only. Native enforces the OUTGOING tail lock again. */
export function routePerformanceCommand(op: string, params: Record<string, unknown>): Record<string, unknown> {
  if (!junctionState.active()) return params;
  if (local.has(op) || op === 'audio.config.set') return params;
  if (!shared.has(op)) throw new Error('Junction中はこの操作を利用できません。セッション設定を開いてください。');
  // Fader start: every DJ operates their own mixer; only the OUTGOING tail is locked.
  const turn = junctionState.get()?.turn;
  const reason = turn?.signal === 'outgoing' ? tailLockReason(op, params as {deck?: string; leader?: string}, turn.tailDecks) : '';
  if (reason) throw new Error(reason);
  return params;
}
