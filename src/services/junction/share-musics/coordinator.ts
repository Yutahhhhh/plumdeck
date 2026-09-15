// Runs a Junction session shared through share-musics in the background: the
// panel may be closed while invitations, responses and notices keep flowing.
// Phones (PlumDeck Lite) and desktops see the same sessions. Only the transport
// of connection packets is automated; creating the session, choosing who may
// join and every performance decision stay with the DJs.
import type { JunctionSnapshot } from '../../../types/junction';
import { junctionCommand } from '../client';
import { junctionState } from '../state';
import { stableThemeColor } from '../roster-model';
import { shareMusicsAccount, shareMusicsApi, ShareMusicsError, type ShareMusicsAccount } from './client';
import {
  automaticHostPeers,
  emptyGuestMemory,
  emptyHostMemory,
  guestPollDelay,
  guestSessionMatches,
  hostPollDelay,
  packetFingerprint,
  planGuest,
  planHost,
  receiveSignals,
  settleHostMemory,
  type GuestAction,
  type GuestMemory,
  type HostAction,
  type HostMemory,
  type JunctionClient,
  type SharedSessionSummary,
  type SharedSessionView,
} from './automation';

export interface ShareMusicsProfile { djName: string; avatarDataUrl?: string; themeColor?: string }

export interface ShareMusicsLink {
  sessionId: string;
  /** This desktop's member id in the shared session. */
  memberId: string;
  role: 'host' | 'guest';
  sessionName: string;
  hostName: string;
  hostClient: JunctionClient;
  /** The native session this shared session belongs to; set for a guest once joined. */
  nativeSessionId?: string;
  joined: boolean;
  profile?: ShareMusicsProfile;
  host: HostMemory;
  guest: GuestMemory;
}

export type ShareMusicsPhase = 'unknown' | 'unavailable' | 'signed_out' | 'signing_in' | 'signed_in';

export interface ShareMusicsState {
  phase: ShareMusicsPhase;
  email?: string;
  accountError?: string;
  sessions: SharedSessionSummary[];
  sessionsError?: string;
  link: ShareMusicsLink | null;
  view: SharedSessionView | null;
  /** Last automatic step that failed and is not retried; cleared when a later step succeeds. */
  error?: string;
  /** share-musics could not be reached; cleared by the next successful poll. */
  pollError?: string;
  /** Why the link ended (session closed, request refused). */
  notice?: string;
}

const LINK_KEY = 'plumdeck.junction.shareMusicsSession';
const MAX_STEPS_PER_POLL = 4;
/** Lite-protocol connections (phone <-> desktop) are enabled once the native side supports them. */
export const LITE_PEERS_READY = true;

interface LiteExchange { state: 'gathering' | 'ready' | 'connected'; sdp?: string; type?: string }

let state: ShareMusicsState = {phase: 'unknown', sessions: [], link: readLink(), view: null};
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setTimeout> | undefined;
let polling = false;
let started = false;

function update(patch: Partial<ShareMusicsState>) {
  state = {...state, ...patch};
  listeners.forEach((listener) => listener());
}

function readLink(): ShareMusicsLink | null {
  try {
    const value = JSON.parse(localStorage.getItem(LINK_KEY) ?? 'null') as ShareMusicsLink | null;
    if (!value || typeof value.sessionId !== 'string' || typeof value.memberId !== 'string' || (value.role !== 'host' && value.role !== 'guest')) return null;
    return {...value, host: {...emptyHostMemory(), ...value.host}, guest: {...emptyGuestMemory(), ...value.guest}};
  } catch {
    return null;
  }
}

function setLink(link: ShareMusicsLink | null) {
  try {
    if (link) localStorage.setItem(LINK_KEY, JSON.stringify(link));
    else localStorage.removeItem(LINK_KEY);
  } catch { /* the link still works for this launch */ }
  update({link: link ? {...link} : null});
}

/** Saves progress made by a poll, unless the link was ended while it ran. */
function persist(link: ShareMusicsLink) {
  if (state.link?.sessionId === link.sessionId && state.link.role === link.role) setLink(link);
}

const current = (link: ShareMusicsLink) => state.link?.sessionId === link.sessionId;
const message = (cause: unknown) => cause instanceof Error ? cause.message : String(cause);

export const shareMusicsStore = {
  get: () => state,
  subscribe: (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; },
};

/** Native peers whose packets travel through share-musics instead of the clipboard. */
export function automaticPeerIds(snapshotState: ShareMusicsState, snapshot: JunctionSnapshot | null): Set<string> {
  const link = snapshotState.link;
  if (!link || !snapshot?.active || snapshot.sessionId !== link.nativeSessionId) return new Set();
  if (link.role === 'guest') return new Set(link.joined ? [snapshot.hostPeerId] : []);
  return new Set(automaticHostPeers(snapshotState.view, link.host));
}

// --- Account ----------------------------------------------------------------

function applyAccount(account: ShareMusicsAccount) {
  update({phase: !account.available ? 'unavailable' : account.signedIn ? 'signed_in' : 'signed_out', email: account.email ?? undefined, accountError: undefined});
  schedule(0);
}

function signedOut(error?: string) {
  update({phase: 'signed_out', email: undefined, sessions: [], accountError: error});
  schedule(0);
}

/** Starts the background link after an app restart; reads the credential store only when a link exists. */
export function startShareMusics() {
  if (started) return;
  started = true;
  if (state.link) void refreshAccount();
}

export async function refreshAccount() {
  try {
    applyAccount(await shareMusicsAccount.status());
  } catch (cause) {
    update({phase: state.phase === 'unknown' ? 'signed_out' : state.phase, accountError: message(cause)});
  }
}

export async function signIn() {
  update({phase: 'signing_in', accountError: undefined});
  try {
    applyAccount(await shareMusicsAccount.login());
    await refreshSessions();
  } catch (cause) {
    const cancelled = cause instanceof ShareMusicsError && cause.status === 499;
    signedOut(cancelled ? undefined : message(cause));
  }
}

export function cancelSignIn() {
  void shareMusicsAccount.cancelLogin().catch(() => {});
}

export async function signOut() {
  if (state.link) await endLink();
  await shareMusicsAccount.logout().catch(() => {});
  signedOut();
}

// --- Lobby ------------------------------------------------------------------

export async function refreshSessions() {
  if (state.phase !== 'signed_in') return;
  try {
    update({sessions: await shareMusicsApi.listSessions(), sessionsError: undefined});
  } catch (cause) {
    if (cause instanceof ShareMusicsError && cause.status === 401) signedOut(cause.message);
    else update({sessionsError: message(cause)});
  }
}

/** Host: make the active native session visible to share-musics members, on phones and desktops. */
export async function publishSession(sessionName: string, hostName: string) {
  const snapshot = junctionState.get();
  if (!snapshot?.active || !snapshot.sessionId || snapshot.hostPeerId !== snapshot.localPeerId) {
    throw new Error('管理DJとしてセッションを作成してから公開してください');
  }
  const {sessionId, peerId} = await shareMusicsApi.createSession(sessionName, hostName);
  setLink({sessionId, memberId: peerId, role: 'host', sessionName, hostName, hostClient: 'desktop', nativeSessionId: snapshot.sessionId, joined: true, host: emptyHostMemory(), guest: emptyGuestMemory()});
  update({notice: undefined, error: undefined, view: null});
  schedule(0);
}

/** Guest: ask to join. The connection follows once the host approves. */
export async function requestJoin(summary: SharedSessionSummary, profile: ShareMusicsProfile) {
  if (junctionState.active()) throw new Error('参加中のセッションを終了してから申請してください');
  const {sessionId, peerId, role} = await shareMusicsApi.joinSession(summary.id, profile.djName);
  if (role !== 'guest') throw new Error('このPCが管理しているセッションです');
  setLink({sessionId, memberId: peerId, role: 'guest', sessionName: summary.name, hostName: summary.hostName, hostClient: summary.hostClient, joined: false, profile, host: emptyHostMemory(), guest: emptyGuestMemory()});
  update({notice: undefined, error: undefined, view: null});
  schedule(0);
}

export async function decideRequest(memberId: string, approve: boolean) {
  const link = state.link;
  if (!link || link.role !== 'host') return;
  await shareMusicsApi.approve(link.sessionId, memberId, approve);
  schedule(0);
}

/** Host: try again after a roster slot could not be created for an approved member. */
export function retryRequest(memberId: string) {
  const link = state.link;
  if (!link || link.role !== 'host') return;
  delete link.host.failed[memberId];
  setLink(link);
  schedule(0);
}

/** Ends the share-musics link. The native session and its connections are untouched. */
export async function endLink(notice?: string) {
  const link = state.link;
  setLink(null);
  update({view: null, error: undefined, pollError: undefined, notice});
  if (link?.role === 'guest' && link.hostClient === 'lite' && link.joined) await junctionCommand('leave').catch(() => {});
  if (link) await shareMusicsApi.leave(link.sessionId).catch(() => {});
}

export function dismissNotice() {
  update({notice: undefined});
}

/** Shows a problem from a step the panel ran on the DJ's behalf (e.g. publishing right after creating). */
export function reportShareMusicsError(problem: string) {
  update({error: problem});
}

// --- Background poll --------------------------------------------------------

function schedule(delay: number) {
  if (timer) clearTimeout(timer);
  timer = state.link && state.phase === 'signed_in' ? setTimeout(() => void poll(), delay) : undefined;
}

async function poll() {
  if (polling) return;
  polling = true;
  let delay = 5000;
  try {
    const link = state.link;
    if (!link) return;
    delay = link.role === 'host' ? await hostPoll(link) : await guestPoll(link);
    if (state.pollError) update({pollError: undefined});
  } catch (cause) {
    if (cause instanceof ShareMusicsError && cause.status === 401) signedOut(cause.message);
    else update({pollError: message(cause)});
  } finally {
    polling = false;
    schedule(delay);
  }
}

/** The shared session itself; null once it is gone, which ends the link. */
async function loadSession(link: ShareMusicsLink, afterSignalId: number) {
  try {
    const view = await shareMusicsApi.getSession(link.sessionId, afterSignalId);
    if (view.session.status === 'closed') {
      if (current(link)) await endLink(link.role === 'guest' ? `${link.hostName}がセッションを終了しました` : undefined);
      return null;
    }
    return view;
  } catch (cause) {
    if (!(cause instanceof ShareMusicsError && cause.status === 404)) throw cause;
    if (current(link)) await endLink(link.role === 'guest' ? 'PlumDeck Liteのセッションは終了しました' : undefined);
    return null;
  }
}

function reportStep(problem: string | undefined) {
  if (problem || state.error) update({error: problem});
}

async function hostPoll(link: ShareMusicsLink): Promise<number> {
  const snapshot = junctionState.get();
  // Unknown until the engine reports; never keep a session alive we cannot see.
  if (!snapshot) return 2000;
  if (!snapshot.active || snapshot.sessionId !== link.nativeSessionId) {
    await endLink();
    return 0;
  }
  const view = await loadSession(link, link.host.afterSignalId);
  if (!view || !current(link)) return 0;
  receiveSignals(view, link.host, null);
  settleHostMemory(view, link.host);
  persist(link);
  update({view});
  await syncLiteHost(link, view, snapshot);
  for (let step = 0; step < MAX_STEPS_PER_POLL && current(link); step += 1) {
    const live = junctionState.get();
    const action = live?.active ? planHost(view, live, link.host) : null;
    if (!action) break;
    reportStep(await runHostAction(link, action));
    persist(link);
  }
  return hostPollDelay(view, junctionState.get() ?? snapshot, link.host);
}

function workerOwnerForNative(view: SharedSessionView, link: ShareMusicsLink, snapshot: JunctionSnapshot): string | undefined {
  if (snapshot.performerPeerId === snapshot.localPeerId) return view.session.hostPeerId;
  const lite = view.session.members.find((member) => member.client === 'lite' && member.peerId === snapshot.performerPeerId);
  if (lite) return lite.peerId;
  return Object.entries(link.host.peerByMember).find(([, nativePeerId]) => nativePeerId === snapshot.performerPeerId)?.[0];
}

function nativeOwnerForWorker(view: SharedSessionView, link: ShareMusicsLink, snapshot: JunctionSnapshot): string | undefined {
  if (view.session.ownerPeerId === view.session.hostPeerId) return snapshot.localPeerId;
  const member = view.session.members.find((item) => item.peerId === view.session.ownerPeerId && item.status === 'approved');
  if (!member) return undefined;
  return member.client === 'lite' ? member.peerId : link.host.peerByMember[member.peerId];
}

async function syncLiteHost(link: ShareMusicsLink, view: SharedSessionView, initialSnapshot: JunctionSnapshot): Promise<void> {
  const approved = view.session.members.filter((member) => member.role === 'guest' && member.status === 'approved' && member.client === 'lite');
  const approvedIds = new Set(approved.map((member) => member.peerId));
  for (const peerId of Object.keys(link.host.litePeers)) {
    if (approvedIds.has(peerId)) continue;
    await junctionCommand('lite.peer.remove', {peerId});
    delete link.host.litePeers[peerId];
  }
  for (const member of approved) {
    const exchange = await junctionCommand('lite.peer.ensure', {peerId: member.peerId, djName: member.displayName}) as LiteExchange;
    link.host.litePeers[member.peerId] = true;
    if (exchange.sdp) {
      const fingerprint = packetFingerprint(exchange.sdp);
      if (link.host.liteOffer[member.peerId] !== fingerprint) {
        await shareMusicsApi.signal(link.sessionId, member.peerId, 'offer', {type: 'offer', sdp: exchange.sdp});
        link.host.liteOffer[member.peerId] = fingerprint;
      }
    }
    const answer = link.host.liteAnswers[member.peerId];
    if (answer && link.host.liteImportedAnswer[member.peerId] !== answer.signalId) {
      await junctionCommand('lite.peer.answer', {peerId: member.peerId, sdp: answer.text});
      link.host.liteImportedAnswer[member.peerId] = answer.signalId;
    }
  }
  let snapshot = junctionState.get() ?? initialSnapshot;
  // On the first poll after an app restart, restore a phone owner recorded by
  // the shared service before treating the native host as authoritative.
  if (!link.host.sharedOwnerPeerId) {
    const restored = nativeOwnerForWorker(view, link, snapshot);
    if (restored && restored !== snapshot.performerPeerId) {
      await junctionCommand('lite.owner.set', {ownerPeerId: restored});
      snapshot = junctionState.get() ?? snapshot;
    }
  }
  link.host.sharedOwnerPeerId = view.session.ownerPeerId;
  const sharedOwner = workerOwnerForNative(view, link, snapshot);
  if (sharedOwner && sharedOwner !== view.session.ownerPeerId) {
    await shareMusicsApi.setOwner(link.sessionId, sharedOwner);
    link.host.sharedOwnerPeerId = sharedOwner;
  }
  persist(link);
}

/** Returns a user-facing problem for steps that are deliberately not retried. */
async function runHostAction(link: ShareMusicsLink, action: HostAction): Promise<string | undefined> {
  const name = state.view?.session.members.find((member) => member.peerId === action.memberId)?.displayName ?? 'DJ';
  switch (action.kind) {
    case 'create_invite': {
      const before = new Set(junctionState.get()?.participants.map((participant) => participant.peerId) ?? []);
      try {
        const reply = await junctionCommand('invite.create', {djName: action.djName, themeColor: stableThemeColor(action.djName)}) as JunctionSnapshot | undefined;
        const participants = reply?.participants ?? junctionState.get()?.participants ?? [];
        const created = participants.find((participant) => !before.has(participant.peerId) && participant.peerId !== reply?.localPeerId);
        if (!created) throw new Error('招待の枠を作成できませんでした');
        link.host.peerByMember[action.memberId] = created.peerId;
        return undefined;
      } catch (cause) {
        link.host.failed[action.memberId] = message(cause);
        return `${action.djName}：${message(cause)}`;
      }
    }
    case 'upload_invite':
      await shareMusicsApi.signal(link.sessionId, action.memberId, 'invite', {text: action.text});
      link.host.uploadedInvite[action.peerId] = action.inviteId;
      // A response still waiting here answered an older invitation.
      delete link.host.responses[action.memberId];
      return undefined;
    case 'import_response':
      link.host.importedResponse[action.memberId] = action.packet.signalId;
      try {
        await junctionCommand('exchange.import', {text: action.packet.text, peerId: action.peerId});
        link.host.approvableInvite[action.memberId] = action.inviteId;
        return undefined;
      } catch (cause) {
        return `${name}の返答を取り込めません：${message(cause)}`;
      }
    case 'approve':
      link.host.approvedInvite[action.memberId] = action.inviteId;
      try {
        await junctionCommand('peer.approve', {peerId: action.peerId, accept: true});
        return undefined;
      } catch (cause) {
        return `${name}の接続を許可できません：${message(cause)}`;
      }
    case 'upload_notice':
      await shareMusicsApi.signal(link.sessionId, action.memberId, 'notice', {text: action.text});
      link.host.uploadedNotice[action.peerId] = packetFingerprint(action.text);
      return undefined;
  }
}

async function guestPoll(link: ShareMusicsLink): Promise<number> {
  const view = await loadSession(link, link.guest.afterSignalId);
  if (!view || !current(link)) return 0;
  receiveSignals(view, null, link.guest);
  persist(link);
  update({view});
  if (view.me.status === 'rejected' && !link.joined) {
    await endLink(`${link.hostName}が参加を許可しませんでした`);
    return 0;
  }
  if (view.me.status === 'left') {
    await endLink();
    return 0;
  }
  if (link.hostClient === 'lite') {
    await syncLiteGuest(link, view);
    return view.me.status === 'pending' ? 1000 : link.joined ? 3000 : 1000;
  }
  const snapshot = junctionState.get();
  if (link.joined && snapshot && !guestSessionMatches(snapshot, link.nativeSessionId)) {
    await endLink();
    return 0;
  }
  for (let step = 0; step < MAX_STEPS_PER_POLL && current(link); step += 1) {
    const action = planGuest(view, junctionState.get(), link.guest, link.joined);
    if (!action) break;
    reportStep(await runGuestAction(link, action));
    persist(link);
  }
  return guestPollDelay(view, junctionState.get(), link.joined);
}

async function syncLiteGuest(link: ShareMusicsLink, view: SharedSessionView): Promise<void> {
  if (view.me.status !== 'approved') return;
  const offer = link.guest.liteOffer;
  if (offer && link.guest.liteHandledOffer !== offer.signalId) {
    if (junctionState.active() && !guestSessionMatches(junctionState.get(), link.nativeSessionId)) throw new Error('別のJunctionセッションに参加中です。退出してから参加してください');
    if (!link.joined) {
      const profile = link.profile ?? {djName: 'DJ'};
      await junctionCommand('lite.join', {
        sessionId: link.sessionId,
        localPeerId: link.memberId,
        hostPeerId: view.session.hostPeerId,
        ownerPeerId: view.session.ownerPeerId,
        sessionName: link.sessionName,
        hostName: link.hostName,
        djName: profile.djName,
        sdp: offer.text,
      });
      link.joined = true;
      link.nativeSessionId = link.sessionId;
    } else {
      await junctionCommand('lite.guest.offer', {sdp: offer.text});
    }
    link.guest.liteHandledOffer = offer.signalId;
  }
  if (!link.joined) return;
  await junctionCommand('lite.roster.set', {members: view.session.members});
  await junctionCommand('lite.owner.set', {ownerPeerId: view.session.ownerPeerId});
  const exchange = await junctionCommand('lite.exchange', {peerId: view.session.hostPeerId}) as LiteExchange;
  if (exchange.sdp) {
    const fingerprint = packetFingerprint(exchange.sdp);
    if (fingerprint !== link.guest.liteUploadedAnswer) {
      await shareMusicsApi.signal(link.sessionId, view.session.hostPeerId, 'answer', {type: 'answer', sdp: exchange.sdp});
      link.guest.liteUploadedAnswer = fingerprint;
    }
  }
  persist(link);
}

async function runGuestAction(link: ShareMusicsLink, action: GuestAction): Promise<string | undefined> {
  switch (action.kind) {
    case 'import_notice':
      link.guest.handledNotice = action.packet.signalId;
      if (!link.joined || !guestSessionMatches(junctionState.get(), link.nativeSessionId)) return undefined;
      try {
        await junctionCommand('exchange.import', {text: action.packet.text});
        return undefined;
      } catch (cause) {
        return `管理DJからの通知を取り込めません：${message(cause)}`;
      }
    case 'import_invite':
      // A failed import is not retried; the host can send a fresh invitation.
      link.guest.handledInvite = action.packet.signalId;
      try {
        if (!link.joined) {
          if (junctionState.active()) throw new Error('別のJunctionセッションに参加中です。退出してから参加してください');
          const profile = link.profile ?? {djName: 'DJ'};
          await junctionCommand('join', {
            displayName: profile.djName,
            djName: profile.djName,
            avatarDataUrl: profile.avatarDataUrl,
            themeColor: profile.themeColor,
            text: action.packet.text,
          });
          link.joined = true;
          link.nativeSessionId = junctionState.get()?.sessionId ?? undefined;
        } else {
          await junctionCommand('exchange.import', {text: action.packet.text});
        }
        return undefined;
      } catch (cause) {
        return `招待を取り込めません：${message(cause)}`;
      }
    case 'upload_response':
      try {
        await shareMusicsApi.signal(link.sessionId, state.view?.session.hostPeerId ?? '', 'response', {text: action.text});
      } catch (cause) {
        // 403: the host no longer accepts this member; the next poll ends the link.
        if (!(cause instanceof ShareMusicsError && cause.status === 403)) throw cause;
      }
      link.guest.respondedInvite = action.inviteSignal;
      link.guest.uploadedResponse = packetFingerprint(action.text);
      return undefined;
  }
}
