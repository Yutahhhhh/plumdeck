import assert from 'node:assert/strict';
import test from 'node:test';
import {
  JUNCTION_MASTER,
  LOCAL_NEXT,
  PROGRAM_MASTER,
  STAGE_LABEL,
  junctionRoles,
  junctionStage,
  programMixerView,
  stripActions,
} from './program-mixer.ts';

const participant = (peerId, overrides = {}) => ({peerId, displayName: peerId, status: 'connected', ...overrides});
const base = (overrides = {}) => ({
  active: true, sessionId: 's', revision: 1, epoch: '2', lifecycle: 'live',
  localPeerId: 'mac', hostPeerId: 'mac', performerPeerId: 'phone', handoffState: 'playing',
  participants: [participant('mac', {isHost: true}), participant('phone', {client: 'lite', rosterStatus: 'performing'}), participant('guest', {rosterStatus: 'waiting'})],
  readiness: {ready: false, reasons: []}, program: {state: 'running'}, connection: {state: 'connected'},
  ...overrides,
});
const input = (overrides = {}) => ({peerId: 'phone', releasingPeerId: '', receiving: true, latencyMs: 120, underruns: '0', channel: {available: true}, lane: {bucketMs: 40, peaks: [], decks: []}, ...overrides});
const mixer = (overrides = {}) => ({
  venueSource: 'direct-stream', directStreamBypass: true,
  junctionMaster: {peerId: 'phone', inProgram: false, releasingPeerId: ''},
  localNext: {inProgram: false, cueOnly: true},
  returnFeed: {source: 'none', targetPeerId: '', feedbackBlocked: false},
  ...overrides,
});

test('the received master is JUNCTION MASTER and never called next', () => {
  assert.equal(JUNCTION_MASTER, 'JUNCTION MASTER');
  assert.equal(LOCAL_NEXT, 'LOCAL NEXT');
  assert.equal(PROGRAM_MASTER, 'PROGRAM MASTER');
  for (const label of Object.values(STAGE_LABEL)) assert.doesNotMatch(label, /次の曲/);
});

test('coordinator, operator, next and the still-sounding DJ are separate roles', () => {
  const snapshot = base({operatorPeerId: 'mac', performerPeerId: 'mac', nextPeerId: 'guest', junctionInput: input({releasingPeerId: 'phone'})});
  const roles = junctionRoles(snapshot);
  assert.equal(roles.coordinatorPeerId, 'mac');
  assert.equal(roles.operatorPeerId, 'mac');
  assert.equal(roles.nextPeerId, 'guest');
  assert.equal(roles.soundingPeerId, 'phone');
  assert.equal(roles.localCoordinator && roles.localOperator, true);
  assert.equal(roles.localSounding, false);
});

test('receiver stages: cueing before takeover, mixing while the previous DJ sounds, then playing', () => {
  assert.equal(junctionStage(null), 'inactive');
  assert.equal(junctionStage(base()), 'waiting');
  assert.equal(junctionStage(base({junctionInput: input()})), 'cueing');
  assert.equal(junctionStage(base({performerPeerId: 'mac', junctionInput: input({releasingPeerId: 'phone'})})), 'mixing');
  assert.equal(junctionStage(base({performerPeerId: 'mac', junctionInput: input({peerId: ''})})), 'playing');
  assert.equal(junctionStage(base({localPeerId: 'guest', performerPeerId: 'phone', junctionInput: input({peerId: '', releasingPeerId: 'guest'})})), 'sending');
});

test('before takeover LOCAL NEXT is CUE only and the engine bypass is reported, not hidden', () => {
  const view = programMixerView(base({junctionInput: input(), programMixer: mixer()}));
  assert.equal(view.venueSource, 'direct-stream');
  assert.equal(view.bypass, true);
  assert.equal(view.localNextCueOnly, true);
  assert.equal(view.localNextInProgram, false);
  assert.equal(view.junctionMasterInProgram, false);
});

test('after takeover Program Master carries JUNCTION MASTER and LOCAL NEXT through the mixer', () => {
  const view = programMixerView(base({performerPeerId: 'mac', programMixer: mixer({
    venueSource: 'local-mix', directStreamBypass: false,
    junctionMaster: {peerId: 'phone', inProgram: true, releasingPeerId: 'phone'},
    localNext: {inProgram: true, cueOnly: false},
  })}));
  assert.equal(view.bypass, false);
  assert.equal(view.junctionMasterInProgram, true);
  assert.equal(view.localNextInProgram, true);
});

test('the return feed is local play only; feedback refusal is surfaced', () => {
  const returning = programMixerView(base({programMixer: mixer({returnFeed: {source: 'local-play', targetPeerId: 'phone', feedbackBlocked: false}})}));
  assert.match(returning.returnLabel, /phoneへ自分のローカル音だけを返送中/);
  assert.doesNotMatch(returning.returnLabel, /Program/);
  const blocked = programMixerView(base({programMixer: mixer({returnFeed: {source: 'none', targetPeerId: '', feedbackBlocked: true}})}));
  assert.equal(blocked.feedbackBlocked, true);
});

test('older runtimes without programMixer derive the same venue answer', () => {
  assert.equal(programMixerView(base()).venueSource, 'direct-stream');
  assert.equal(programMixerView(base({performerPeerId: 'mac'})).venueSource, 'local-mix');
  assert.equal(programMixerView(base({localPeerId: 'guest'})).venueSource, 'remote-host');
  assert.equal(programMixerView(base({program: {state: 'idle'}})).venueSource, 'none');
});

test('the coordinator can nominate any connected or played DJ without a request', () => {
  const snapshot = base({participants: [
    participant('mac', {isHost: true, rosterStatus: 'waiting'}),
    participant('phone', {client: 'lite', rosterStatus: 'performing'}),
    participant('guest', {rosterStatus: 'waiting'}),
    participant('played', {rosterStatus: 'finished'}),
    participant('invitee', {rosterStatus: 'invited'}),
  ]});
  const nominate = stripActions(snapshot).find((action) => action.kind === 'nominate');
  assert.deepEqual(nominate.candidates.map((row) => row.peerId), ['mac', 'guest', 'played']);
});

test('no new nomination while the previous DJ still sounds; the operator gets release instead', () => {
  const actions = stripActions(base({performerPeerId: 'mac', junctionInput: input({releasingPeerId: 'phone'})}));
  assert.deepEqual(actions.map((action) => action.kind), ['release']);
});

test('guests request a turn, including again after playing, and the next DJ accepts', () => {
  const guest = (status, overrides = {}) => base({localPeerId: 'guest', participants: [participant('mac', {isHost: true}), participant('guest', {rosterStatus: status})], ...overrides});
  assert.deepEqual(stripActions(guest('waiting')).map((action) => action.label), ['演奏を希望']);
  assert.deepEqual(stripActions(guest('finished')).map((action) => action.label), ['もう一度演奏を希望']);
  assert.deepEqual(stripActions(guest('requested')), []);
  const next = stripActions(guest('next', {nextPeerId: 'guest'}));
  assert.equal(next[0].kind, 'accept');
  assert.equal(next[0].disabled, true);
});
