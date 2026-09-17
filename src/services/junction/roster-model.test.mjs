import assert from 'node:assert/strict';
import test from 'node:test';
import {
  orderedParticipants,
  compactReadinessReasons,
  connectionAlert,
  coordinatorCanSelect,
  DJ_NAME_MAX_LENGTH,
  limitDjName,
  profileAvatarValue,
  participantName,
  participantVisualState,
  qualityPresentation,
  reorderPeerIds,
  safeAvatarDataUrl,
  stableThemeColor,
  rosterTurnActions,
  rosterPositionLocked,
} from './roster-model.ts';

const participant = (peerId, overrides = {}) => ({
  peerId,
  displayName: peerId,
  ...overrides,
});

const snapshot = (participants, overrides = {}) => ({
  active: true,
  sessionId: 'session-1',
  sessionName: 'Night session',
  revision: 1,
  epoch: '1',
  localPeerId: 'host',
  hostPeerId: 'host',
  performerPeerId: '',
  handoffState: 'IDLE',
  participants,
  readiness: {ready: false, reasons: []},
  program: {state: 'preparing'},
  connection: {state: 'connected'},
  ...overrides,
});

test('DJ name is the public identity while legacy displayName remains a fallback', () => {
  assert.equal(participantName(participant('a', {djName: '  DJ MIKA  ', displayName: 'Legacy name'})), 'DJ MIKA');
  assert.equal(participantName(participant('b', {displayName: '  Legacy DJ  '})), 'Legacy DJ');
  assert.equal(participantName(participant('c', {displayName: '', djName: '  '})), 'DJ名を確認中');
});

test('the roster follows orderIndex stably without mutating a legacy snapshot', () => {
  const legacy = participant('legacy', {displayName: 'Legacy host'});
  const input = [
    participant('third', {orderIndex: 2}),
    legacy,
    participant('second', {orderIndex: 1}),
  ];

  assert.deepEqual(orderedParticipants(input).map((row) => row.peerId), ['legacy', 'second', 'third']);
  assert.deepEqual(input.map((row) => row.peerId), ['third', 'legacy', 'second']);

  const oldSnapshot = snapshot([legacy], {performerPeerId: 'legacy'});
  assert.equal(participantName(oldSnapshot.participants[0]), 'Legacy host');
  assert.equal(participantVisualState(oldSnapshot.participants[0], oldSnapshot), 'playing');
  assert.deepEqual(qualityPresentation(oldSnapshot.participants[0].connectionQuality), {
    level: 'unknown', label: '通信確認中', bars: 0, detail: undefined,
  });
});

test('reordering returns the complete ordered peer-id set and leaves rows untouched', () => {
  const rows = [
    participant('host', {orderIndex: 0}),
    participant('one', {orderIndex: 1}),
    participant('two', {orderIndex: 2}),
    participant('three', {orderIndex: 3}),
  ];

  assert.deepEqual(reorderPeerIds(rows, 'three', 'one'), ['host', 'three', 'one', 'two']);
  assert.deepEqual(reorderPeerIds(rows, 'missing', 'one'), ['host', 'one', 'two', 'three']);
  assert.deepEqual(rows.map((row) => row.peerId), ['host', 'one', 'two', 'three']);
});

test('an invitation row keeps its identity and position when the real DJ profile arrives', () => {
  const placeholder = participant('peer-slot-1', {
    djName: '招待中のDJ',
    slotId: 'slot-1',
    invitationId: 'invite-1',
    orderIndex: 2,
    approved: false,
    rosterStatus: 'invited',
    exchange: {state: 'invite_ready', waitingFor: 'guest'},
  });
  const connected = {
    ...placeholder,
    djName: 'DJ MIKA',
    displayName: 'DJ MIKA',
    avatarDataUrl: 'data:image/png;base64,YQ==',
    themeColor: '#0369a1',
    approved: true,
    rosterStatus: 'ready',
    exchange: {state: 'connected', waitingFor: 'none'},
  };

  assert.equal(connected.peerId, placeholder.peerId);
  assert.equal(connected.slotId, placeholder.slotId);
  assert.equal(connected.invitationId, placeholder.invitationId);
  assert.equal(connected.orderIndex, placeholder.orderIndex);
  assert.equal(participantName(connected), 'DJ MIKA');
  assert.equal(participantVisualState(placeholder, snapshot([placeholder])), 'invited');
  assert.equal(participantVisualState(connected, snapshot([connected])), 'ready');
});

test('the first performer is derived from performerPeerId, not from host identity or row order', () => {
  const rows = [
    participant('host', {djName: 'Session manager', orderIndex: 1, isHost: true}),
    participant('first-dj', {djName: 'Opening DJ', orderIndex: 0, rosterStatus: 'ready'}),
  ];
  const live = snapshot(rows, {lifecycle: 'live', performerPeerId: 'first-dj'});

  assert.equal(participantVisualState(rows[0], live), 'ready');
  assert.equal(participantVisualState(rows[1], live), 'playing');
  assert.deepEqual(orderedParticipants(rows).map((row) => row.peerId), ['first-dj', 'host']);
});

test('connected DJs are candidates without a request, and played DJs are never locked out', () => {
  assert.equal(coordinatorCanSelect('ready'), true, 'a connected DJ needs no turn request to be chosen');
  assert.equal(coordinatorCanSelect('finished'), true, 'a DJ who played may be chosen again');
  for (const state of ['playing', 'next', 'invited', 'connecting', 'response', 'problem']) assert.equal(coordinatorCanSelect(state), false);
  assert.equal(rosterPositionLocked('playing'), true);
  assert.equal(rosterPositionLocked('finished'), false);
  const lite = participant('phone', {client: 'lite', status: 'connected', rosterStatus: 'waiting'});
  assert.equal(participantVisualState(lite, snapshot([lite], {lifecycle: 'live', performerPeerId: 'host'})), 'ready');
  const replay = participant('phone', {rosterStatus: 'requested'});
  assert.equal(participantVisualState(replay, snapshot([replay], {lifecycle: 'live', performerPeerId: 'host'})), 'requested');
});

test('a temporary transport interruption is shown as automatic reconnect, not a terminal disconnect', () => {
  const unstable = participant('guest', {rosterStatus: 'unstable', exchange: {state: 'interrupted', waitingFor: 'none'}});
  assert.equal(participantVisualState(unstable, snapshot([unstable])), 'reconnecting');
  const legacy = participant('legacy', {status: 'interrupted'});
  assert.equal(participantVisualState(legacy, snapshot([legacy])), 'reconnecting');
});

test('connection quality has an accessible four-level presentation plus unknown fallback', () => {
  assert.deepEqual(qualityPresentation({level: 'good', rttMs: 42, packetLossPct: 0.25, jitterMs: 3}), {
    level: 'good', label: '通信良好', bars: 3, detail: '遅延 42ms / 損失 0.3% / 揺らぎ 3ms',
  });
  assert.deepEqual(qualityPresentation({level: 'fair'}), {level: 'fair', label: '通信注意', bars: 2, detail: undefined});
  assert.deepEqual(qualityPresentation({level: 'poor'}), {level: 'poor', label: '通信不安定', bars: 1, detail: undefined});
  assert.deepEqual(qualityPresentation({level: 'offline'}), {level: 'offline', label: '未接続', bars: 0, detail: undefined});
  assert.deepEqual(qualityPresentation(), {level: 'unknown', label: '通信確認中', bars: 0, detail: undefined});
});

test('avatar and fallback colour helpers are deterministic and reject active image payloads', () => {
  assert.equal(stableThemeColor('DJ MIKA'), stableThemeColor('DJ MIKA'));
  assert.match(stableThemeColor('DJ MIKA'), /^#[0-9a-f]{6}$/i);
  assert.equal(safeAvatarDataUrl('data:image/png;base64,YQ=='), 'data:image/png;base64,YQ==');
  assert.equal(safeAvatarDataUrl('data:image/svg+xml;base64,PHN2Zy8+'), undefined);
  assert.equal(safeAvatarDataUrl('data:text/html;base64,PGgxPkJvb208L2gxPg=='), undefined);
  assert.equal(profileAvatarValue(''), '');
  assert.equal(profileAvatarValue('data:image/png;base64,YQ=='), 'data:image/png;base64,YQ==');
});

test('DJ names follow the native forty-character contract', () => {
  assert.equal(DJ_NAME_MAX_LENGTH, 40);
  assert.equal(limitDjName('x'.repeat(41)), 'x'.repeat(40));
});

test('blocking readiness and serious connection reasons stay concise and visible', () => {
  assert.equal(compactReadinessReasons([' 楽曲を準備中 ', '楽曲を準備中', '音声を確認中', '同期を確認中']), '楽曲を準備中・音声を確認中（ほか1件）');
  assert.equal(compactReadinessReasons([]), undefined);
  assert.equal(connectionAlert(snapshot([], {connection: {state: 'interrupted', detail: '相手との通信が中断しました'}})), '相手との通信が中断しました');
  assert.equal(connectionAlert(snapshot([], {connection: {state: 'connected', detail: 'internal detail'}})), undefined);
});

test('fader start roster: the timetable decides next, the host steps in only when the booth needs it', () => {
  const p = (peerId, overrides = {}) => ({peerId, displayName: peerId, djName: peerId, status: 'connected', ...overrides});
  const turn = (overrides = {}) => ({signal: 'onair', nextPeerId: 'b', outgoingPeerId: '', blocker: {code: '', text: ''}, nextStatus: '', tailDecks: [], repeat: false, outOfQueue: ['d'], incompatiblePeerIds: ['old'], autoFailover: false, ...overrides});
  const snap = (overrides = {}) => ({active: true, sessionId: 's', revision: 1, epoch: '3', lifecycle: 'live', localPeerId: 'a', hostPeerId: 'a', performerPeerId: 'a', nextPeerId: 'b', handoffState: 'playing',
    participants: [p('a', {rosterStatus: 'performing'}), p('b', {rosterStatus: 'next'}), p('c', {rosterStatus: 'waiting'}), p('d', {rosterStatus: 'finished'}), p('old', {rosterStatus: 'waiting'})],
    readiness: {ready: false, reasons: []}, program: {state: 'running'}, connection: {state: 'connected'}, turn: turn(), ...overrides});
  const labels = (snapshot, id, host) => rosterTurnActions(snapshot.participants.find((row) => row.peerId === id), snapshot, host).map((action) => `${action.label}:${action.op}`);
  const s = snap();
  assert.deepEqual(labels(s, 'a', true), [], 'the ON AIR DJ has nothing to press');
  assert.deepEqual(labels(s, 'b', true), ['強制交代:turn.force', 'スキップ:turn.skip']);
  assert.deepEqual(labels(s, 'c', true), ['順番から外す:turn.leave']);
  assert.deepEqual(labels(s, 'd', true), ['順番に入れる:turn.join']);
  assert.deepEqual(labels(s, 'old', true), [], 'an app that cannot fader-start is never queued');
  assert.equal(rosterTurnActions(s.participants[2], s, true)[0].params.peerId, 'c');
  const ready = snap({participants: s.participants.map((row) => row.peerId === 'b' ? {...row, rosterStatus: 'ready'} : row)});
  assert.equal(participantVisualState(ready.participants[1], ready), 'nextReady');
  assert.equal(rosterTurnActions(ready.participants[1], ready, true)[0].primary, true, 'forcing a READY DJ is the obvious step');
  const guest = snap({localPeerId: 'd', turn: turn()});
  assert.deepEqual(labels(guest, 'd', false), ['順番に入る:turn.join']);
  assert.deepEqual(rosterTurnActions(guest.participants[3], guest, false)[0].params, {}, 'a DJ joins as themself');
  assert.deepEqual(labels(guest, 'c', false), [], 'guests never move other DJs');
  const outgoing = snap({performerPeerId: 'b', nextPeerId: '', turn: turn({outgoingPeerId: 'a', nextPeerId: ''})});
  assert.equal(participantVisualState(outgoing.participants[0], outgoing), 'outgoing');
  assert.deepEqual(labels(outgoing, 'a', true), ['強制解放:turn.release']);
  const lobby = snap({lifecycle: 'lobby', performerPeerId: '', nextPeerId: '', participants: [p('a', {rosterStatus: 'waiting'}), p('c', {rosterStatus: 'waiting'})]});
  assert.deepEqual(labels(lobby, 'c', true), ['最初のDJにする:session.start']);
  assert.deepEqual(labels(lobby, 'c', false), []);
});
