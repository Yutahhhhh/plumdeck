// Decides the next automatic step of a Junction session shared through
// share-musics. Phones (PlumDeck Lite) and desktops use the same session list,
// members and signal queue; each pair uses what both sides support:
//   desktop <-> desktop  native signed invite/response/notice packets
//   anything with Lite   Lite WebRTC offer/answer (audio relay)
// share-musics only carries packets; every desktop packet is still produced,
// signed and verified by the native engine exactly as in the manual flow.
// Pure functions so the protocol can be tested without the engine or network.
import type { ExchangeState, JunctionSnapshot } from '../../../types/junction.ts';

export type JunctionClient = 'lite' | 'desktop';
export type JunctionMemberStatus = 'pending' | 'approved' | 'rejected' | 'left';

export interface SharedMember {
  peerId: string;
  displayName: string;
  role: 'host' | 'guest';
  status: JunctionMemberStatus;
  owner: boolean;
  client: JunctionClient;
}

export interface SharedSessionDetail {
  id: string;
  name: string;
  hostPeerId: string;
  ownerPeerId: string;
  status: 'open' | 'closed';
  members: SharedMember[];
}

export interface SharedSignal {
  id: number;
  senderPeerId: string;
  kind: 'offer' | 'answer' | 'invite' | 'response' | 'notice';
  payload: {type?: string; sdp?: string; text?: string};
}

export interface SharedSessionView {
  session: SharedSessionDetail;
  me: {peerId: string; role: 'host' | 'guest'; status: JunctionMemberStatus; client: JunctionClient};
  signals: SharedSignal[];
}

export interface SharedSessionSummary {
  id: string;
  name: string;
  hostName: string;
  hostClient: JunctionClient;
  participantCount: number;
  createdAt: string;
  mine: boolean;
}

export interface ReceivedPacket { signalId: number; text: string }

/** What the host has delivered or consumed; keyed so a restarted poll never repeats a step. */
export interface HostMemory {
  afterSignalId: number;
  /** share-musics member peer id -> native peer id of the roster slot created for it. */
  peerByMember: Record<string, string>;
  /** native peer id -> invite id already delivered. */
  uploadedInvite: Record<string, string>;
  /** native peer id -> fingerprint of the notice already delivered. */
  uploadedNotice: Record<string, string>;
  /** member -> latest response received for the current invitation. */
  responses: Record<string, ReceivedPacket>;
  /** member -> signal id of the last response fetched for import (successful or not; never retried). */
  importedResponse: Record<string, number>;
  /** member -> invite id whose response was imported successfully; only that one is auto-approved. */
  approvableInvite: Record<string, string>;
  /** member -> invite id whose native approval was already attempted. */
  approvedInvite: Record<string, string>;
  /** member -> reason the roster slot could not be created; not retried automatically. */
  failed: Record<string, string>;
  /** Lite member -> offer fingerprint already uploaded. */
  liteOffer: Record<string, string>;
  /** Lite member -> latest answer from the shared signal queue. */
  liteAnswers: Record<string, ReceivedPacket>;
  /** Lite member -> answer signal already imported by the native bridge. */
  liteImportedAnswer: Record<string, number>;
  litePeers: Record<string, boolean>;
  /** Last Worker owner observed, used to restore ownership after restart. */
  sharedOwnerPeerId: string;
}

export interface GuestMemory {
  afterSignalId: number;
  invite?: ReceivedPacket;
  notice?: ReceivedPacket;
  handledInvite: number;
  handledNotice: number;
  respondedInvite: number;
  uploadedResponse: string;
  liteOffer?: ReceivedPacket;
  liteHandledOffer: number;
  liteUploadedAnswer: string;
}

export type HostAction =
  | {kind: 'create_invite'; memberId: string; djName: string}
  | {kind: 'upload_invite'; memberId: string; peerId: string; inviteId: string; text: string}
  | {kind: 'import_response'; memberId: string; peerId: string; inviteId: string; packet: ReceivedPacket}
  | {kind: 'approve'; memberId: string; peerId: string; inviteId: string}
  | {kind: 'upload_notice'; memberId: string; peerId: string; text: string};

export type GuestAction =
  | {kind: 'import_notice'; packet: ReceivedPacket}
  | {kind: 'import_invite'; packet: ReceivedPacket}
  | {kind: 'upload_response'; inviteSignal: number; text: string};

export const emptyHostMemory = (): HostMemory => ({afterSignalId: 0, peerByMember: {}, uploadedInvite: {}, uploadedNotice: {}, responses: {}, importedResponse: {}, approvableInvite: {}, approvedInvite: {}, failed: {}, liteOffer: {}, liteAnswers: {}, liteImportedAnswer: {}, litePeers: {}, sharedOwnerPeerId: ''});
export const emptyGuestMemory = (): GuestMemory => ({afterSignalId: 0, handledInvite: 0, handledNotice: 0, respondedInvite: 0, uploadedResponse: '', liteHandledOffer: 0, liteUploadedAnswer: ''});

const AWAITING_ANSWER: ExchangeState[] = ['invite_ready', 'awaiting_answer'];
const RESPONSE_READY: ExchangeState[] = ['response_ready', 'awaiting_host'];

/** Short stable fingerprint, so memory never has to compare whole packets. */
export function packetFingerprint(text: string): string {
  let hash = 2166136261;
  for (let index = 0; index < text.length; index += 1) hash = Math.imul(hash ^ text.charCodeAt(index), 16777619);
  return `${(hash >>> 0).toString(16)}-${text.length}`;
}

/** Guests whose connection this desktop host drives with native packets. */
export function desktopGuests(view: SharedSessionView): SharedMember[] {
  return view.session.members.filter((member) => member.role === 'guest' && member.status === 'approved' && member.client === 'desktop');
}

/** Native peers whose packets travel through share-musics instead of the clipboard. */
export function automaticHostPeers(view: SharedSessionView | null, memory: HostMemory): string[] {
  if (!view) return [];
  return [
    ...desktopGuests(view).map((member) => memory.peerByMember[member.peerId]).filter((peerId): peerId is string => Boolean(peerId)),
    ...view.session.members.filter((member) => member.role === 'guest' && member.status === 'approved' && member.client === 'lite').map((member) => member.peerId),
  ];
}

/** Files arriving packets: the latest response per guest (host) or the latest invite/notice (guest). */
export function receiveSignals(view: SharedSessionView, host: HostMemory | null, guest: GuestMemory | null): void {
  for (const signal of view.signals) {
    const text = signal.payload.text;
    if (host) {
      host.afterSignalId = Math.max(host.afterSignalId, signal.id);
      if (signal.kind === 'response' && text) host.responses[signal.senderPeerId] = {signalId: signal.id, text};
      if (signal.kind === 'answer' && signal.payload.sdp) host.liteAnswers[signal.senderPeerId] = {signalId: signal.id, text: signal.payload.sdp};
    }
    if (guest) {
      guest.afterSignalId = Math.max(guest.afterSignalId, signal.id);
      if (signal.senderPeerId !== view.session.hostPeerId) continue;
      if (signal.kind === 'invite' && text) guest.invite = {signalId: signal.id, text};
      if (signal.kind === 'notice' && text) guest.notice = {signalId: signal.id, text};
      if (signal.kind === 'offer' && signal.payload.sdp) guest.liteOffer = {signalId: signal.id, text: signal.payload.sdp};
    }
  }
}

/**
 * Forgets progress for guests that are no longer approved. A guest who left
 * and asks again starts over, so nothing from the previous attempt is reused.
 */
export function settleHostMemory(view: SharedSessionView, memory: HostMemory): void {
  const approved = new Set(desktopGuests(view).map((member) => member.peerId));
  for (const table of [memory.peerByMember, memory.responses, memory.importedResponse, memory.approvableInvite, memory.approvedInvite, memory.failed]) {
    for (const memberId of Object.keys(table)) if (!approved.has(memberId)) delete table[memberId];
  }
  const approvedLite = new Set(view.session.members.filter((member) => member.role === 'guest' && member.status === 'approved' && member.client === 'lite').map((member) => member.peerId));
  for (const table of [memory.liteOffer, memory.liteAnswers, memory.liteImportedAnswer]) {
    for (const memberId of Object.keys(table)) if (!approvedLite.has(memberId)) delete table[memberId];
  }
}

export function planHost(view: SharedSessionView, snapshot: JunctionSnapshot, memory: HostMemory): HostAction | null {
  for (const member of desktopGuests(view)) {
    if (memory.failed[member.peerId]) continue;
    const peerId = memory.peerByMember[member.peerId];
    const participant = peerId ? snapshot.participants.find((item) => item.peerId === peerId) : undefined;
    if (!peerId || !participant) return {kind: 'create_invite', memberId: member.peerId, djName: member.displayName};
    const exchange = participant.exchange;
    if (!exchange) continue;
    if (exchange.noticeText && (exchange.state === 'cancelled' || exchange.state === 'rejected')
      && memory.uploadedNotice[peerId] !== packetFingerprint(exchange.noticeText)) {
      return {kind: 'upload_notice', memberId: member.peerId, peerId, text: exchange.noticeText};
    }
    if (exchange.inviteText && exchange.inviteId && AWAITING_ANSWER.includes(exchange.state)
      && memory.uploadedInvite[peerId] !== exchange.inviteId) {
      return {kind: 'upload_invite', memberId: member.peerId, peerId, inviteId: exchange.inviteId, text: exchange.inviteText};
    }
    // Only a response that arrived after this poll delivered the current invitation, and only once.
    const response = memory.responses[member.peerId];
    const delivered = Boolean(exchange.inviteId) && memory.uploadedInvite[peerId] === exchange.inviteId;
    if (delivered && response && AWAITING_ANSWER.includes(exchange.state) && response.signalId > (memory.importedResponse[member.peerId] ?? 0)) {
      return {kind: 'import_response', memberId: member.peerId, peerId, inviteId: exchange.inviteId!, packet: response};
    }
    // The host approved this member in share-musics; the response came from that
    // Google-verified account, so the native approval follows. Only for the
    // invitation whose response this poll imported itself, never a pasted one.
    if (exchange.state === 'approval_pending' && exchange.inviteId
      && memory.approvableInvite[member.peerId] === exchange.inviteId && memory.approvedInvite[member.peerId] !== exchange.inviteId) {
      return {kind: 'approve', memberId: member.peerId, peerId, inviteId: exchange.inviteId};
    }
  }
  return null;
}

/** True while the guest's native session belongs to this shared session. */
export function guestSessionMatches(snapshot: JunctionSnapshot | null, nativeSessionId: string | undefined): boolean {
  return Boolean(snapshot?.active && nativeSessionId && snapshot.sessionId === nativeSessionId);
}

export function planGuest(view: SharedSessionView, snapshot: JunctionSnapshot | null, memory: GuestMemory, joined: boolean): GuestAction | null {
  if (memory.notice && memory.notice.signalId > memory.handledNotice) return {kind: 'import_notice', packet: memory.notice};
  if (view.me.status !== 'approved') return null;
  if (memory.invite && memory.invite.signalId > memory.handledInvite) return {kind: 'import_invite', packet: memory.invite};
  const exchange = joined && snapshot?.active ? snapshot.exchange : undefined;
  if (exchange?.responseText && RESPONSE_READY.includes(exchange.state) && memory.handledInvite > memory.respondedInvite
    && packetFingerprint(exchange.responseText) !== memory.uploadedResponse) {
    return {kind: 'upload_response', inviteSignal: memory.handledInvite, text: exchange.responseText};
  }
  return null;
}

/** Faster polling only while an admission step is in flight. */
export function hostPollDelay(view: SharedSessionView, snapshot: JunctionSnapshot, memory: HostMemory): number {
  const settling = view.session.members.some((member) => {
    if (member.role !== 'guest') return false;
    if (member.status === 'pending') return true;
    if (member.status !== 'approved' || member.client !== 'desktop') return false;
    const state = snapshot.participants.find((item) => item.peerId === memory.peerByMember[member.peerId])?.exchange?.state;
    return state !== 'connected' && state !== 'interrupted';
  });
  return settling ? 1500 : 5000;
}

export function guestPollDelay(view: SharedSessionView, snapshot: JunctionSnapshot | null, joined: boolean): number {
  if (view.me.status === 'pending') return 3000;
  const state = joined && snapshot?.active ? snapshot.exchange?.state : undefined;
  return state === 'connected' || state === 'interrupted' ? 8000 : 1500;
}
