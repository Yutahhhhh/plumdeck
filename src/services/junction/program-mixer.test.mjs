import assert from 'node:assert/strict';
import test from 'node:test';
import {programMixerView} from './program-mixer.ts';

const participant = (peerId, overrides = {}) => ({peerId, displayName: peerId, status: 'connected', ...overrides});
const base = (overrides = {}) => ({
  active: true, sessionId: 's', revision: 1, epoch: '2', lifecycle: 'live',
  localPeerId: 'mac', hostPeerId: 'mac', performerPeerId: 'phone', handoffState: 'playing',
  participants: [participant('mac', {isHost: true}), participant('phone', {client: 'lite', rosterStatus: 'performing'}), participant('guest', {rosterStatus: 'waiting'})],
  program: {state: 'running'}, connection: {state: 'connected'},
  ...overrides,
});
const mixer = (overrides = {}) => ({
  venueSource: 'direct-stream',
  returnFeed: {source: 'none', targetPeerId: '', feedbackBlocked: false},
  ...overrides,
});

test('the venue label follows the native routing, including the engine bypass', () => {
  assert.match(programMixerView(base({programMixer: mixer()})).venueLabel, /このミキサーを通りません/);
  assert.match(programMixerView(base({performerPeerId: 'mac', programMixer: mixer({venueSource: 'local-mix'})})).venueLabel, /Program Master/);
});

test('the return feed is local play only; feedback refusal is surfaced', () => {
  const returning = programMixerView(base({programMixer: mixer({returnFeed: {source: 'local-play', targetPeerId: 'phone', feedbackBlocked: false}})}));
  assert.match(returning.returnLabel, /phoneへ自分のローカル音だけを返送中/);
  assert.doesNotMatch(returning.returnLabel, /Program/);
  const blocked = programMixerView(base({programMixer: mixer({returnFeed: {source: 'none', targetPeerId: '', feedbackBlocked: true}})}));
  assert.match(blocked.returnLabel, /返送を停止中/);
});

test('older runtimes without programMixer derive the same venue answer', () => {
  const label = (source) => programMixerView(base({programMixer: mixer({venueSource: source})})).venueLabel;
  assert.equal(programMixerView(base()).venueLabel, label('direct-stream'));
  assert.equal(programMixerView(base({performerPeerId: 'mac'})).venueLabel, label('local-mix'));
  assert.equal(programMixerView(base({operatorPeerId: 'mac'})).venueLabel, label('local-mix'));
  assert.equal(programMixerView(base({localPeerId: 'guest'})).venueLabel, label('remote-host'));
  assert.equal(programMixerView(base({program: {state: 'idle'}})).venueLabel, label('none'));
});
