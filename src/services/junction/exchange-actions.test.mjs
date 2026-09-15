import {test} from 'node:test';
import assert from 'node:assert/strict';
import {deriveGuestGuidance, deriveHostCardGuidance, describeExchangeState, exchangeConnected, formatExpiry, primaryExchangeAction} from './exchange-actions.ts';
const peer = (state, extra = {}) => ({peerId:'guest', displayName:'DJ MIKA', exchange:{state, ...extra}});
const guest = (state) => ({hostPeerId:'host', localPeerId:'guest', participants:[{peerId:'host', exchange:{state}}], exchange:{mode:'manual',state}});
const states = ['idle','collecting','invite_ready','awaiting_answer','approval_pending','response_ready','awaiting_host','connecting','connected','interrupted','needs_exchange','failed','expired','cancelled','rejected'];
test('every exchange state has at most one primary action, no file or ineffective retry action', () => {
  for(const state of states) for(const g of [deriveHostCardGuidance(peer(state)), deriveGuestGuidance(guest(state))]) {
    assert(g.actions.filter(a => a.intent === 'primary').length <= 1, state);
    assert(g.actions.every(a => !/file|retry/.test(a.id)), state);
    assert([1,2,3].includes(g.step));
  }
});
test('host moves from invitation copy to response entry without claiming delivery', () => {
  const initial=deriveHostCardGuidance(peer('invite_ready'));
  const copied=deriveHostCardGuidance(peer('invite_ready'),true);
  assert.equal(primaryExchangeAction(initial).id,'copy_invite'); assert.equal(initial.step,1);
  assert.equal(primaryExchangeAction(copied).id,'paste_answer'); assert.equal(copied.step,2);
  assert.match(copied.headline,/チャットで送り/);
});
test('guest moves from reply copy to waiting and can still enter a renewed invitation', () => {
  assert.equal(primaryExchangeAction(deriveGuestGuidance(guest('response_ready'))).id,'copy_answer');
  const g=deriveGuestGuidance(guest('response_ready'),true);
  assert.equal(g.step,3); assert.equal(g.waiting,true); assert.equal(primaryExchangeAction(g),undefined);
  assert.match(g.headline,/チャットで管理DJへ送って/); assert(g.actions.some(a=>a.id==='paste_invite'));
});
test('host approval stays explicit and identifies the DJ before applying the answer', () => {
  const g=deriveHostCardGuidance(peer('approval_pending'));
  assert.equal(g.step,3); assert.match(g.headline,/DJ MIKA/);
  assert.equal(primaryExchangeAction(g).id,'approve'); assert(g.actions.some(a=>a.id==='reject'));
});
test('a temporary outage waits; persistent failure has one role-specific recovery path', () => {
  const recovering=deriveHostCardGuidance(peer('interrupted'));
  assert.equal(primaryExchangeAction(recovering),undefined);
  assert.match(recovering.headline,/自動で再接続/);
  assert.match(deriveGuestGuidance(guest('interrupted')).headline,/自動で再接続/);
  for(const state of ['needs_exchange','failed','expired','cancelled','rejected']) {
    assert.equal(primaryExchangeAction(deriveHostCardGuidance(peer(state))).id,'reexchange');
    assert.equal(primaryExchangeAction(deriveGuestGuidance(guest(state))).id,'paste_invite');
  }
});
test('transport errors remain visible and cancellations offer a signed notice', () => {
  assert.equal(deriveHostCardGuidance(peer('failed',{detail:'相手が応答しません'})).error,'相手が応答しません');
  assert(deriveHostCardGuidance(peer('cancelled')).actions.some(a=>a.id==='copy_notice'));
});
test('guest can derive progress from the session snapshot without a host row', () => {
  assert.equal(deriveGuestGuidance({participants:[],exchange:{mode:'manual',state:'connecting'}}).step,3);
});
test('connection is distinct from playback readiness; expiry is based on actual time', () => {
  assert.equal(describeExchangeState('connected'),'接続済み'); assert(exchangeConnected('connected')); assert(!exchangeConnected('connecting'));
  assert.equal(formatExpiry(undefined,1000),undefined); assert.equal(formatExpiry(999,1000),'有効期限切れ');
  assert.equal(formatExpiry(31000,1000),'有効期限まで約30秒'); assert.equal(formatExpiry(601000,1000),'有効期限まで約10分');
});
test('share-musics delivery replaces clipboard steps without hiding the manual fallback', () => {
  for (const state of ['invite_ready', 'awaiting_answer']) {
    const g = deriveHostCardGuidance(peer(state), false, true);
    assert.equal(g.step, 2); assert.equal(g.waiting, true); assert.equal(primaryExchangeAction(g), undefined);
    assert.match(g.headline, /PlumDeck Lite経由/);
    assert(g.actions.some(a => a.id === 'copy_invite') && g.actions.some(a => a.id === 'paste_answer'));
  }
  assert.equal(primaryExchangeAction(deriveHostCardGuidance(peer('approval_pending'), false, true)).id, 'approve');
  assert.equal(primaryExchangeAction(deriveHostCardGuidance(peer('failed'), false, true)).id, 'reexchange');
  assert.equal(deriveHostCardGuidance(peer('connected'), false, true).headline, deriveHostCardGuidance(peer('connected')).headline);
  const reply = deriveGuestGuidance(guest('response_ready'), false, true);
  assert.equal(reply.step, 3); assert.equal(primaryExchangeAction(reply), undefined); assert.match(reply.headline, /PlumDeck Lite経由/);
  const lost = deriveGuestGuidance(guest('needs_exchange'), false, true);
  assert.equal(primaryExchangeAction(lost), undefined); assert(lost.actions.some(a => a.id === 'paste_invite'));
  for (const state of states) for (const g of [deriveHostCardGuidance(peer(state), false, true), deriveGuestGuidance(guest(state), false, true)]) {
    assert(g.actions.filter(a => a.intent === 'primary').length <= 1, state);
  }
});
