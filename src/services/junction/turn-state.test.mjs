import assert from 'node:assert/strict';
import test from 'node:test';
import {readFile} from 'node:fs/promises';
import path from 'node:path';
import {
  BLOCKER_ORDER, BLOCKER_TEXT, CUE_LABEL, FADER_START_CAPABILITY, FaderStart, SIGNAL_LABEL,
  deriveSignal, notOnAirReason, readyBlocker, tailLockReason, turnGuidance, turnSignal,
} from './turn-state.ts';

const nativeSource = () => readFile(path.resolve(import.meta.dirname, '../../../native/mixxx-engine-host/src/junction/turn_state.h'), 'utf8');
const roles = (local) => ({active: true, local, owner: 'a', next: 'b', outgoing: 'z'});
const allReady = () => ({compatible: true, programOpen: true, junctionRequired: true, junctionReceiving: true, junctionStable: true, latencyMeasured: true, pathLatencyMs: 80, budgetMs: 250, standbyStreamRequired: true, standbyStreamReceived: true, junctionUnity: true, localSilent: true});
const participant = (peerId, djName) => ({peerId, displayName: djName, djName});
const snapshot = (overrides = {}) => ({
  active: true, sessionId: 's', revision: 1, epoch: '2', lifecycle: 'live', localPeerId: 'b', hostPeerId: 'a', performerPeerId: 'a', operatorPeerId: 'a',
  handoffState: 'playing', participants: [participant('a', 'DJ A'), participant('b', 'DJ B'), participant('c', 'DJ C')],
  readiness: {ready: false, reasons: []}, program: {state: 'running'}, connection: {state: 'connected'}, ...overrides,
});
const turn = (overrides = {}) => ({signal: 'standby', nextPeerId: 'b', outgoingPeerId: '', blocker: {code: '', text: ''}, nextStatus: '', tailDecks: [], repeat: false, outOfQueue: [], incompatiblePeerIds: [], autoFailover: false, ...overrides});

test('signal light follows the roles exactly like native derive', () => {
  assert.equal(deriveSignal(roles('a'), false), 'onair');
  assert.equal(deriveSignal(roles('b'), false), 'standby');
  assert.equal(deriveSignal(roles('b'), true), 'ready');
  assert.equal(deriveSignal(roles('z'), true), 'outgoing');
  assert.equal(deriveSignal(roles('c'), true), 'off');
  assert.equal(deriveSignal({...roles('a'), active: false}, true), 'off');
  assert.deepEqual(Object.values(SIGNAL_LABEL), ['OFF', 'STANDBY', 'READY', 'ON AIR', 'OUTGOING']);
});

test('ready shows exactly one blocker in fix-first order', () => {
  assert.equal(readyBlocker(allReady()), '');
  assert.equal(readyBlocker({...allReady(), localSilent: false, junctionUnity: false}), 'junction_not_unity');
  assert.equal(readyBlocker({...allReady(), localSilent: false}), 'already_audible');
  assert.equal(readyBlocker({...allReady(), junctionReceiving: false, compatible: false}), 'update_required');
  assert.equal(readyBlocker({...allReady(), junctionReceiving: false}), 'waiting_junction');
  assert.equal(readyBlocker({...allReady(), junctionReceiving: false, junctionRequired: false}), '');
  assert.equal(readyBlocker({...allReady(), pathLatencyMs: 251}), 'latency_budget');
  assert.equal(readyBlocker({...allReady(), standbyStreamReceived: false}), 'host_waiting_stream');
  assert.equal(readyBlocker({...allReady(), standbyStreamReceived: false, standbyStreamRequired: false}), '');
});

test('native and TypeScript share the capability, blocker vocabulary, order and fader-start constants', async () => {
  const source = await nativeSource();
  assert.match(source, new RegExp(`kCapability="${FADER_START_CAPABILITY}"`));
  const codes = [...source.matchAll(/case Blocker::(\w+):return QStringLiteral\("([a-z_]+)"\);/g)].map((m) => m[2]);
  assert.deepEqual(codes, BLOCKER_ORDER, 'codes are listed in the same fix-first order');
  for (const code of BLOCKER_ORDER) assert(source.includes(`QStringLiteral("${BLOCKER_TEXT[code]}")`), `native text for ${code}`);
  const order = source.slice(source.indexOf('inline Blocker readyBlocker('), source.indexOf('inline QString blockerCode('));
  const returned = [...order.matchAll(/return Blocker::(\w+);/g)].map((m) => m[1]).filter((name) => name !== 'None');
  const enumOrder = [...source.matchAll(/case Blocker::(\w+):return QStringLiteral\("[a-z_]+"\);/g)].map((m) => m[1]);
  assert.deepEqual(returned, enumOrder, 'native readyBlocker checks in the published order');
  assert.match(source, /kThreshold=\.00316f/); assert.equal(FaderStart.threshold, 0.00316);
  assert.match(source, /kOnsetNanos=20000000LL/); assert.equal(FaderStart.onsetMs, 20);
  assert.match(source, /kArmNanos=300000000LL/); assert.equal(FaderStart.armMs, 300);
  assert.match(source, /残りの曲を送出中はマスター系を操作できません/); assert.match(source, /このデッキは残りの曲として送出中です。ループ以外は操作できません/);
});

test('fader start fires once on a silence to sound edge and never for a fader already up', () => {
  const fader = new FaderStart();
  for (let t = 0; t <= 1000; t += 5) assert.equal(fader.observe(0.5, false, t), false);
  assert.equal(fader.armed, false);
  for (let t = 1005; t < 1300; t += 5) assert.equal(fader.observe(0, false, t), false);
  assert.equal(fader.observe(0, false, 1305), false); assert.equal(fader.armed, true);
  assert.equal(fader.observe(0.01, false, 1310), false); assert.equal(fader.observe(0.01, false, 1325), false);
  assert.equal(fader.observe(0.01, false, 1330), true); assert.equal(fader.observe(0.01, false, 1335), false);
  const j = new FaderStart(); j.observe(0, false, 0); j.observe(0, false, 300);
  assert.equal(j.observe(0, true, 301), false); assert.equal(j.observe(0, true, 321), true);
});

test('tail lock matches native: loops only on tail decks, everything free elsewhere', () => {
  for (const op of ['deck.loop.set', 'deck.loop.enable', 'mixer.channel.pfl']) assert.equal(tailLockReason(op, {deck: 'A'}, ['A']), '');
  for (const op of ['deck.play', 'deck.seek', 'deck.hotcue.jump', 'deck.tempo.set', 'deck.sync.set', 'deck.load', 'mixer.channel.gain', 'mixer.filter.set']) assert.notEqual(tailLockReason(op, {deck: 'A'}, ['A']), '');
  for (const op of ['deck.play', 'deck.load', 'mixer.channel.gain']) assert.equal(tailLockReason(op, {deck: 'B'}, ['A']), '');
  assert.notEqual(tailLockReason('mixer.crossfader', {}, ['A']), '');
  assert.notEqual(tailLockReason('deck.sync.set', {deck: 'B', leader: 'A'}, ['A']), '');
});

test('guidance is one DJ-facing line without system vocabulary', () => {
  const standby = snapshot({turn: turn()});
  assert.equal(turnSignal(standby), 'standby');
  assert.equal(turnGuidance(standby), 'STANDBY — DJ Aの音を受信中。曲を用意してCUEで合わせてください');
  assert.equal(turnGuidance(snapshot({turn: turn({signal: 'ready'})})), 'READY — フェーダーを上げるとON AIR');
  assert.equal(turnGuidance(snapshot({localPeerId: 'a', turn: turn({signal: 'onair', outgoingPeerId: 'c'})})), 'ON AIR — DJ Cの曲が残っています。Jを下げると交代完了');
  assert.equal(turnGuidance(snapshot({localPeerId: 'c', operatorPeerId: 'b', performerPeerId: 'b', turn: turn({signal: 'outgoing', outgoingPeerId: 'c'})})), 'OUTGOING — 残りの曲をDJ Bが下げるまで送っています（ループのみ操作可）');
  assert.equal(turnGuidance(snapshot({turn: turn({blocker: {code: 'already_audible', text: ''}})})), 'STANDBY — 一度フェーダーを下げてください');
  assert.equal(turnGuidance(snapshot({operatorPeerId: '', performerPeerId: '', turn: turn()})), 'STANDBY — 最初のDJです。曲を用意してください');
  assert.equal(notOnAirReason(snapshot({turn: turn({blocker: {code: 'latency_budget', text: ''}})})), 'まだ本番に出ていません：回線の遅延が大きく、会場の音に間に合いません');
  assert.equal(notOnAirReason(snapshot({turn: turn({signal: 'ready'})})), '');
  for (const line of [turnGuidance(standby), ...Object.values(BLOCKER_TEXT), ...Object.values(CUE_LABEL)]) {
    assert.doesNotMatch(line, /操作権|送出中|direct-stream|feedbackBlocked|epoch/);
  }
});
