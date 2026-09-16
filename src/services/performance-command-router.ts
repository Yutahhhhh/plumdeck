import { captureJunctionLease, junctionState } from './junction/state.ts';
import type { JunctionLease } from '../types/junction.ts';
const local = new Set(['performance.endpoint','engine.clock.probe','waveform.ensure','waveform.manifest','waveform.requestRange','waveform.cancelRequest','waveform.invalidate','engine.ping','state.snapshot','audio.devices.list','audio.config.get','meters.subscribe','mixer.channel.pfl','mixer.headphone.gain','mixer.headphone.mix','sampler.state','sampler.pfl','recording.directory.set','recording.format.set']);
const shared = new Set(['deck.key.shift','deck.load','deck.unload','deck.play','deck.pause','deck.seek','deck.scratch','deck.pitchbend','deck.tempo.set','deck.beatgrid.set','deck.keylock.set','deck.sync.set','deck.hotcue.set','deck.hotcue.jump','deck.hotcue.clear','deck.loop.set','deck.loop.enable','deck.beatjump','deck.loop.beats','deck.quantize.set','deck.slipReverse.set','mixer.filter.set','mixer.trim.set','mixer.fx.set','mixer.channel.gain','mixer.channel.eq','mixer.channel.orientation','mixer.crossfader','mixer.master.gain','mixer.colorfx.set','mixer.beatfx.set','sampler.eject','sampler.load','sampler.unload','sampler.play','sampler.stop','sampler.gain','sampler.bank','sampler.trigger','sampler.set']);
/** Early UX guard only. Native validates every lease and operation again. */
export function routePerformanceCommand(op: string, params: Record<string, unknown>, lease: JunctionLease | null = captureJunctionLease()): Record<string, unknown> {
  if (!lease) return params;
  if (local.has(op)) return params;
  const state = junctionState.get();
  if (op === 'audio.config.set') {
    const microphone = params.microphone as {enabled?: boolean} | undefined;
    if (!microphone?.enabled) return {...params, _junction: lease};
  } else if (!shared.has(op)) throw new Error('Junction中はこの操作を利用できません。セッション設定を開いてください。');
  if (state?.localPeerId !== state?.performerPeerId && !state?.localPrep) throw new Error('別のDJがプレイ中です。手元の試聴または引き継ぎをご利用ください。');
  return {...params, _junction: lease};
}
