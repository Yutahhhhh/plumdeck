import { invoke, isTauri } from '@tauri-apps/api/core';
import type { SharedSessionSummary, SharedSessionView, SharedSignal } from './automation';

/** Signed-in state. Tokens stay in the Tauri process; the webview never sees them. */
export interface ShareMusicsAccount {
  /** False when this build has no PlumDeck Lite service configured. */
  available: boolean;
  signedIn: boolean;
  email?: string | null;
  serviceUrl: string;
}

export class ShareMusicsError extends Error {
  constructor(message: string, readonly status: number) {
    super(message);
    this.name = 'ShareMusicsError';
  }
}

/** `text` is null when the packet was withdrawn (cancelled invite, guest left). */
export type SharedSignalKind = SharedSignal['kind'];

function toError(cause: unknown): ShareMusicsError {
  if (cause && typeof cause === 'object' && 'status' in cause && 'message' in cause) {
    return new ShareMusicsError(String(cause.message), Number(cause.status));
  }
  return new ShareMusicsError(cause instanceof Error ? cause.message : String(cause), 0);
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) throw new ShareMusicsError('Junctionはデスクトップアプリで利用できます。', 0);
  try {
    return await invoke<T>(command, args);
  } catch (cause) {
    throw toError(cause);
  }
}

export const shareMusicsAccount = {
  status: () => call<ShareMusicsAccount>('share_musics_status'),
  /** Opens the system browser and resolves after the loopback callback and whitelist check. */
  login: () => call<ShareMusicsAccount>('share_musics_login'),
  cancelLogin: () => call<void>('share_musics_cancel_login'),
  logout: () => call<void>('share_musics_logout'),
};

const DEVICE_KEY = 'plumdeck.junction.shareMusicsDevice';
let fallbackDevice = '';

/**
 * Random per-install ID. It only tells two PCs signed in with the same Google
 * account apart (host vs guest); authorization is always the account itself.
 */
function deviceId(): string {
  const valid = (value: string | null): value is string => /^[0-9a-f]{32}$/.test(value ?? '');
  try {
    const saved = localStorage.getItem(DEVICE_KEY);
    if (valid(saved)) return saved;
  } catch { /* fall through to a launch-scoped ID */ }
  if (!valid(fallbackDevice)) {
    fallbackDevice = Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) => byte.toString(16).padStart(2, '0')).join('');
  }
  try { localStorage.setItem(DEVICE_KEY, fallbackDevice); } catch { /* launch-scoped */ }
  return fallbackDevice;
}

function request<T>(action: string, query: Record<string, string> = {}, body: Record<string, unknown> = {}): Promise<T> {
  const identity = {device: deviceId(), client: 'desktop'};
  return call<T>('share_musics_request', {action, query: {...query, ...identity}, body: {...body, ...identity}});
}

/** share-musics' Junction API, shared with PlumDeck Lite. */
export const shareMusicsApi = {
  listSessions: () => request<SharedSessionSummary[]>('listJunctionSessions'),
  getSession: (sessionId: string, afterSignalId: number) => request<SharedSessionView>('getJunctionSession', {sessionId, afterSignalId: String(afterSignalId)}),
  createSession: (name: string, displayName: string) => request<{sessionId: string; peerId: string}>('createJunctionSession', undefined, {name, displayName}),
  joinSession: (sessionId: string, displayName: string) => request<{sessionId: string; peerId: string; role: 'host' | 'guest'}>('joinJunctionSession', undefined, {sessionId, displayName}),
  approve: (sessionId: string, peerId: string, approved: boolean) => request<{ok: true}>('approveJunctionMember', undefined, {sessionId, peerId, approved}),
  signal: (sessionId: string, recipientPeerId: string, kind: SharedSignalKind, payload: Record<string, unknown>) =>
    request<{signalId: number}>('postJunctionSignal', undefined, {sessionId, recipientPeerId, kind, payload}),
  setOwner: (sessionId: string, ownerPeerId: string) => request<{ok: true}>('setJunctionOwner', undefined, {sessionId, ownerPeerId}),
  leave: (sessionId: string) => request<{ok: true}>('leaveJunctionSession', undefined, {sessionId}),
};
