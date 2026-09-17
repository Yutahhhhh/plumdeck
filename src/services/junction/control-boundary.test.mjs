import {test} from 'node:test';
import assert from 'node:assert/strict';
import {junctionState} from './state.ts';
import {routePerformanceCommand} from '../performance-command-router.ts';
const base = {active:true,sessionId:'room',revision:1,epoch:'1',localPeerId:'self',hostPeerId:'self',performerPeerId:'other',handoffState:'IDLE',participants:[],program:{state:'preparing'},connection:{state:'connected'}};
test('PFL stays local',()=>{
  junctionState.set(base);
  assert.deepEqual(routePerformanceCommand('mixer.channel.pfl',{deck:'A',enabled:true}),{deck:'A',enabled:true});
});
test('while a Lite DJ performs, this computer prepares its own decks',()=>{
  junctionState.set({...base,performerPeerId:'phone'});
  assert.deepEqual(routePerformanceCommand('deck.load',{deck:'B'}),{deck:'B'});
  assert.throws(()=>routePerformanceCommand('unknown.raw',{}));
});
test('fader start: a waiting DJ plays their own decks and only the OUTGOING tail is refused',()=>{
  const turn={signal:'standby',nextPeerId:'self',outgoingPeerId:'',blocker:{code:'',text:''},nextStatus:'',tailDecks:[],repeat:false,outOfQueue:[],incompatiblePeerIds:[],autoFailover:false};
  junctionState.set({...base,turn});
  assert.equal(routePerformanceCommand('deck.play',{deck:'A'}).deck,'A');
  junctionState.set({...base,turn:{...turn,signal:'outgoing',tailDecks:['A']}});
  assert.throws(()=>routePerformanceCommand('deck.play',{deck:'A'}),/ループ以外/);
  assert.equal(routePerformanceCommand('deck.loop.enable',{deck:'A',enabled:false}).deck,'A');
  assert.equal(routePerformanceCommand('deck.load',{deck:'B'}).deck,'B');
  assert.throws(()=>routePerformanceCommand('mixer.crossfader',{position:0}),/マスター系/);
});
test('no authority ticket is attached and unknown raw operations fail closed',()=>{
  junctionState.set({...base,performerPeerId:'self'});
  assert.deepEqual(routePerformanceCommand('mixer.beatfx.set',{enabled:false}),{enabled:false});
  assert.deepEqual(routePerformanceCommand('audio.config.set',{microphone:{enabled:true}}),{microphone:{enabled:true}});
  assert.throws(()=>routePerformanceCommand('unknown.raw',{}));
});

test('remote numeric IDs never fall back to a receiver library row', async()=>{
  const {localTrackId, assetWaveform} = await import('./asset-resolver.ts');
  assert.equal(localTrackId({trackId:'42',assetId:'sha256:remote'}),null);
  assert.equal(localTrackId({trackId:'42',assetId:'sha256:remote',localTrackId:7}),7);
  assert.equal(localTrackId({trackId:'42'}),42);
  assert.equal(assetWaveform({bins_per_second:300,duration_ms:100,amplitude_scale:1,peaks:[1],low:[1],mid:[NaN],high:[0]}),undefined);
});
