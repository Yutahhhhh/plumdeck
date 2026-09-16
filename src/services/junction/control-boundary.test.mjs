import {test} from 'node:test';
import assert from 'node:assert/strict';
import {junctionState, captureJunctionLease} from './state.ts';
import {routePerformanceCommand} from '../performance-command-router.ts';
const base = {active:true,sessionId:'room',revision:1,epoch:'1',localPeerId:'self',hostPeerId:'self',performerPeerId:'other',handoffState:'IDLE',participants:[],readiness:{ready:false,reasons:[]},program:{state:'preparing'},connection:{state:'connected'}};
test('host authority does not grant performer controls; PFL stays local',()=>{
  junctionState.set(base);
  assert.throws(()=>routePerformanceCommand('deck.play',{deck:'A'}));
  assert.deepEqual(routePerformanceCommand('mixer.channel.pfl',{deck:'A',enabled:true}),{deck:'A',enabled:true});
  assert.throws(()=>routePerformanceCommand('sampler.play',{slot:0}));
});
test('while a Lite DJ performs, this computer prepares its own decks with a lease',()=>{
  junctionState.set({...base,performerPeerId:'phone',localPrep:true});
  const routed = routePerformanceCommand('deck.load',{deck:'B'});
  assert.equal(routed.deck,'B');
  assert.equal(routed._junction.actorPeerId,'self');
  assert.throws(()=>routePerformanceCommand('unknown.raw',{}));
});
test('queued lease is not promoted after handoff and unknown raw operations fail closed',()=>{
  junctionState.set({...base,performerPeerId:'self'});
  const lease = captureJunctionLease();
  junctionState.set({...base,performerPeerId:'self',epoch:'2'});
  assert.deepEqual(routePerformanceCommand('mixer.beatfx.set',{enabled:false},lease)._junction,lease);
  assert.throws(()=>routePerformanceCommand('unknown.raw',{}));
});

test('remote numeric IDs never fall back to a receiver library row', async()=>{
  const {localTrackId, assetWaveform} = await import('./asset-resolver.ts');
  assert.equal(localTrackId({trackId:'42',assetId:'sha256:remote'}),null);
  assert.equal(localTrackId({trackId:'42',assetId:'sha256:remote',localTrackId:7}),7);
  assert.equal(localTrackId({trackId:'42'}),42);
  assert.equal(assetWaveform({bins_per_second:300,duration_ms:100,amplitude_scale:1,peaks:[1],low:[1],mid:[NaN],high:[0]}),undefined);
});
