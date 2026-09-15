import {test} from 'node:test';
import assert from 'node:assert/strict';
import {
  automaticHostPeers, desktopGuests, emptyGuestMemory, emptyHostMemory, guestPollDelay, guestSessionMatches, hostPollDelay,
  packetFingerprint, planGuest, planHost, receiveSignals, settleHostMemory,
} from './automation.ts';

const member = (peerId, extra = {}) => ({peerId, displayName: `DJ ${peerId}`, role: 'guest', status: 'approved', owner: false, client: 'desktop', ...extra});
const view = (members, signals = [], me = {peerId: 'host', role: 'host', status: 'approved', client: 'desktop'}) => ({
  session: {id: 's', name: 'Friday', hostPeerId: 'host', ownerPeerId: 'host', status: 'open', members: [member('host', {role: 'host'}), ...members]},
  me, signals,
});
const hostSnapshot = (...participants) => ({active: true, sessionId: 'n1', localPeerId: 'native-host', hostPeerId: 'native-host', participants: [{peerId: 'native-host', displayName: 'DJ HOST'}, ...participants]});
const peer = (state, extra = {}) => ({peerId: 'p1', displayName: 'DJ m1', exchange: {state, ...extra}});
const response = (id, text = `R${id}`) => ({id, senderPeerId: 'm1', kind: 'response', payload: {text}});

test('only approved desktop guests are driven with native packets; phones use the Lite path', () => {
  const v = view([member('m1'), member('m2', {status: 'pending'}), member('m3', {client: 'lite'}), member('m4', {status: 'left'})]);
  assert.deepEqual(desktopGuests(v).map((m) => m.peerId), ['m1']);
  assert.deepEqual(planHost(v, hostSnapshot(), emptyHostMemory()), {kind: 'create_invite', memberId: 'm1', djName: 'DJ m1'});
});

test('host walks an approved desktop guest from roster slot to native approval exactly once', () => {
  const memory = emptyHostMemory();
  memory.peerByMember.m1 = 'p1';
  assert.equal(planHost(view([member('m1')]), hostSnapshot(peer('collecting')), memory), null);
  const ready = hostSnapshot(peer('invite_ready', {inviteId: 'i1', inviteText: 'PLUMDECK-JUNCTION-1.invite'}));
  assert.deepEqual(planHost(view([member('m1')]), ready, memory), {kind: 'upload_invite', memberId: 'm1', peerId: 'p1', inviteId: 'i1', text: 'PLUMDECK-JUNCTION-1.invite'});
  memory.uploadedInvite.p1 = 'i1';
  assert.equal(planHost(view([member('m1')]), ready, memory), null, 'waits for the response');
  receiveSignals(view([member('m1')], [response(7)]), memory, null);
  assert.equal(memory.afterSignalId, 7);
  assert.deepEqual(planHost(view([member('m1')]), ready, memory), {kind: 'import_response', memberId: 'm1', peerId: 'p1', inviteId: 'i1', packet: {signalId: 7, text: 'R7'}});
  memory.importedResponse.m1 = 7;
  memory.approvableInvite.m1 = 'i1';
  assert.equal(planHost(view([member('m1')]), ready, memory), null, 'a response is imported once');
  const pending = hostSnapshot(peer('approval_pending', {inviteId: 'i1'}));
  assert.deepEqual(planHost(view([member('m1')]), pending, memory), {kind: 'approve', memberId: 'm1', peerId: 'p1', inviteId: 'i1'});
  memory.approvedInvite.m1 = 'i1';
  assert.equal(planHost(view([member('m1')]), pending, memory), null, 'a failed approval falls back to the manual button');
});

test('a response entered by hand, or imported for another invitation, is never approved automatically', () => {
  const memory = {...emptyHostMemory(), peerByMember: {m1: 'p1'}, uploadedInvite: {p1: 'i1'}, importedResponse: {m1: 3}};
  const pending = hostSnapshot(peer('approval_pending', {inviteId: 'i1'}));
  assert.equal(planHost(view([member('m1')]), pending, memory), null, 'automatic import failed');
  memory.approvableInvite.m1 = 'i0';
  assert.equal(planHost(view([member('m1')]), pending, memory), null);
});

test('re-exchange: a fresh invitation is delivered, and only a response after it is imported', () => {
  const memory = {...emptyHostMemory(), peerByMember: {m1: 'p1'}, uploadedInvite: {p1: 'i1'}, importedResponse: {m1: 3}};
  const renewed = hostSnapshot(peer('invite_ready', {inviteId: 'i2', inviteText: 'PLUMDECK-JUNCTION-1.renewed'}));
  assert.equal(planHost(view([member('m1')]), renewed, memory).kind, 'upload_invite');
  memory.uploadedInvite.p1 = 'i2';
  memory.responses.m1 = {signalId: 3, text: 'old'};
  assert.equal(planHost(view([member('m1')]), renewed, memory), null, 'already fetched');
  receiveSignals(view([member('m1')], [response(9)]), memory, null);
  assert.equal(planHost(view([member('m1')]), renewed, memory).kind, 'import_response');
  // A response for an invitation this poll never delivered is ignored.
  const foreign = {...emptyHostMemory(), peerByMember: {m1: 'p1'}, responses: {m1: {signalId: 1, text: 'x'}}};
  assert.equal(planHost(view([member('m1')]), hostSnapshot(peer('awaiting_answer', {inviteId: 'i9'})), foreign), null);
});

test('host delivers a cancellation notice once and never retries a failed roster slot', () => {
  const memory = {...emptyHostMemory(), peerByMember: {m1: 'p1'}};
  const cancelled = hostSnapshot(peer('cancelled', {noticeText: 'PLUMDECK-JUNCTION-1.notice'}));
  assert.deepEqual(planHost(view([member('m1')]), cancelled, memory), {kind: 'upload_notice', memberId: 'm1', peerId: 'p1', text: 'PLUMDECK-JUNCTION-1.notice'});
  memory.uploadedNotice.p1 = packetFingerprint('PLUMDECK-JUNCTION-1.notice');
  assert.equal(planHost(view([member('m1')]), cancelled, memory), null);
  assert.equal(planHost(view([member('m2')]), hostSnapshot(), {...emptyHostMemory(), failed: {m2: '8人まで'}}), null);
});

test('a guest who left starts over when they come back', () => {
  const memory = {...emptyHostMemory(), peerByMember: {m1: 'p1', m2: 'p2'}, importedResponse: {m1: 1, m2: 1}, approvableInvite: {m1: 'i1'}, approvedInvite: {m1: 'i1'}, failed: {m1: 'x'}, responses: {m1: {signalId: 1, text: 'r'}}};
  settleHostMemory(view([member('m1', {status: 'left'}), member('m2')]), memory);
  assert.deepEqual([memory.peerByMember, memory.importedResponse, memory.approvableInvite, memory.approvedInvite, memory.failed, memory.responses], [{m2: 'p2'}, {m2: 1}, {}, {}, {}, {}]);
  assert.deepEqual(automaticHostPeers(view([member('m1', {status: 'left'}), member('m2')]), memory), ['p2']);
  assert.deepEqual(automaticHostPeers(null, memory), []);
});

const guestView = (status = 'approved', signals = []) => view([], signals, {peerId: 'me', role: 'guest', status, client: 'desktop'});
const guestSnapshot = (state, responseText) => ({active: true, sessionId: 'n1', localPeerId: 'me', hostPeerId: 'native-host', participants: [], exchange: {mode: 'manual', state, responseText}});
const fromHost = (id, kind, text = `${kind}${id}`) => ({id, senderPeerId: 'host', kind, payload: {text}});

test('guest imports each invitation once and returns the response for it', () => {
  const memory = emptyGuestMemory();
  assert.equal(planGuest(guestView('pending'), null, memory, false), null);
  receiveSignals(guestView('approved', [fromHost(4, 'invite'), {id: 5, senderPeerId: 'other', kind: 'invite', payload: {text: 'forged'}}]), null, memory);
  assert.equal(memory.afterSignalId, 5); assert.equal(memory.invite.text, 'invite4', 'only packets from the host count');
  assert.deepEqual(planGuest(guestView(), null, memory, false), {kind: 'import_invite', packet: {signalId: 4, text: 'invite4'}});
  memory.handledInvite = 4;
  assert.equal(planGuest(guestView(), guestSnapshot('collecting'), memory, true), null);
  assert.equal(planGuest(guestView(), guestSnapshot('response_ready', 'R1'), memory, false), null, 'not before the native join');
  assert.deepEqual(planGuest(guestView(), guestSnapshot('response_ready', 'R1'), memory, true), {kind: 'upload_response', inviteSignal: 4, text: 'R1'});
  Object.assign(memory, {respondedInvite: 4, uploadedResponse: packetFingerprint('R1')});
  assert.equal(planGuest(guestView(), guestSnapshot('awaiting_host', 'R1'), memory, true), null);
  receiveSignals(guestView('approved', [fromHost(8, 'invite')]), null, memory);
  assert.equal(planGuest(guestView(), guestSnapshot('connected', 'R1'), memory, true).kind, 'import_invite');
  memory.handledInvite = 8;
  assert.equal(planGuest(guestView(), guestSnapshot('response_ready', 'R1'), memory, true), null, 'the previous response is never resent');
  assert.equal(planGuest(guestView(), guestSnapshot('response_ready', 'R2'), memory, true).inviteSignal, 8);
});

test('guest handles notices first, even after the request was refused', () => {
  const memory = emptyGuestMemory();
  receiveSignals(guestView('rejected', [fromHost(3, 'notice'), fromHost(2, 'invite')]), null, memory);
  assert.deepEqual(planGuest(guestView('rejected'), null, memory, true), {kind: 'import_notice', packet: {signalId: 3, text: 'notice3'}});
  memory.handledNotice = 3;
  assert.equal(planGuest(guestView('rejected'), null, memory, true), null);
});

test('session matching and polling cadence', () => {
  assert.equal(guestSessionMatches(guestSnapshot('connected'), 'n1'), true);
  assert.equal(guestSessionMatches(guestSnapshot('connected'), 'n2'), false);
  assert.equal(guestSessionMatches({...guestSnapshot('connected'), active: false}, 'n1'), false);
  assert.equal(guestSessionMatches(null, 'n1'), false);
  const memory = {...emptyHostMemory(), peerByMember: {m1: 'p1'}};
  assert.equal(hostPollDelay(view([member('m9', {status: 'pending'})]), hostSnapshot(), memory), 1500);
  assert.equal(hostPollDelay(view([member('m1')]), hostSnapshot(peer('connected')), memory), 5000);
  assert.equal(guestPollDelay(guestView('pending'), null, false), 3000);
  assert.equal(guestPollDelay(guestView(), guestSnapshot('connected'), true), 8000);
  assert.equal(guestPollDelay(guestView(), guestSnapshot('response_ready'), true), 1500);
});
