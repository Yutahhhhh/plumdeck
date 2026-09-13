// Manual per-DJ offer/answer exchange contract shared with the native gateway.
// Legacy WSS ("server") exchange is preserved behind the same types.
export type ExchangeState =
  | 'idle'
  | 'collecting'
  | 'invite_ready'
  | 'awaiting_answer'
  | 'approval_pending'
  | 'response_ready'
  | 'awaiting_host'
  | 'connecting'
  | 'connected'
  | 'interrupted'
  | 'needs_exchange'
  | 'failed'
  | 'expired'
  | 'cancelled'
  | 'rejected';
export type ExchangeMode = 'manual' | 'server';
export type ExchangeWaitingFor = 'host' | 'guest' | 'local' | 'none';
export type ExchangeRoute = 'direct' | 'relay' | 'mixed' | 'unknown';

export interface SnapshotExchange {
  mode: ExchangeMode;
  state: ExchangeState;
  detail?: string;
  errorCode?: string;
  responseText?: string;
  inviteId?: string;
  expiresAt?: number;
}
export interface ParticipantExchange {
  state: ExchangeState;
  waitingFor: ExchangeWaitingFor;
  detail?: string;
  errorCode?: string;
  inviteText?: string;
  noticeText?: string;
  expiresAt?: number;
  inviteId?: string;
  attempt?: number;
  route?: ExchangeRoute;
}
export interface JunctionParticipant {
  peerId: string;
  /** User-facing identity. displayName remains for older native runtimes. */
  djName?: string;
  displayName: string;
  avatarDataUrl?: string;
  themeColor?: string;
  slotId?: string;
  invitationId?: string;
  isPlaceholder?: boolean;
  orderIndex?: number;
  rosterStatus?: string;
  /** The DJ asked to be considered next; only the coordinator starts handoff. */
  turnRequested?: boolean;
  connectionQuality?: JunctionConnectionQuality;
  isHost?: boolean;
  /** Session coordinator; independent from the current performer. */
  isCoordinator?: boolean;
  isPerformer?: boolean;
  isNextUp?: boolean;
  status?: string;
  approved?: boolean;
  exchange?: ParticipantExchange;
}
export type JunctionConnectionLevel = 'unknown' | 'good' | 'fair' | 'poor' | 'offline';
export interface JunctionConnectionQuality {
  level: JunctionConnectionLevel;
  rttMs?: number;
  jitterMs?: number;
  packetLossPct?: number;
}
export interface JunctionSnapshot {
  active: boolean;
  sessionId: string | null;
  sessionName?: string;
  revision: number | string;
  localPeerId: string;
  hostPeerId: string;
  /** Alias that describes the role without implying performance order. */
  coordinatorPeerId?: string;
  performerPeerId: string;
  nextPeerId?: string;
  lifecycle?: 'lobby' | 'starting' | 'live';
  epoch: string;
  handoffState: string;
  participants: JunctionParticipant[];
  readiness: { ready: boolean; reasons: string[] };
  program: { state: string; captureActive?: boolean; localMonitor?: 'direct' | 'program-delayed'; meter?: number; outputDevice?: string; recording?: boolean };
  connection: { state: string; detail?: string };
  /** Legacy single server invite. Manual mode uses per-participant exchange.inviteText. */
  invite?: string;
  exchange?: SnapshotExchange;
  /** Local-only: tracks received from the remote performer. Never on the wire. */
  junctionTracks?: JunctionTrack[];
}
export type JunctionTrackState = 'pending' | 'receiving' | 'verifying' | 'ready' | 'failed';
export type JunctionTrackRole = 'current' | 'next';
/** Current/next presentation metadata from the remote performer. */
export interface JunctionTrack {
  role: JunctionTrackRole;
  assetId: string;
  title: string;
  artist: string;
  musicalKey: string;
  durationMs: number;
  bpm: number;
  sizeBytes: number;
  sourcePeerId: string;
  sourceDjName: string;
  /** Deck on the performer's computer, or '' once it left every deck. */
  sourceDeck: '' | 'A' | 'B' | 'C' | 'D';
  onDeck: boolean;
  playing: boolean;
  positionMs: number;
  rate: number;
  audibility: number;
  state: JunctionTrackState;
  ready: boolean;
  order: number;
  progress: number;
  detail?: string;
  /** This computer's verified cache file; present only when ready. */
  path?: string;
}
export type JunctionOp =
  | 'snapshot'
  | 'create'
  | 'join'
  | 'leave'
  | 'end'
  | 'invite.rotate'
  | 'invite.create'
  | 'invite.cancel'
  | 'exchange.inspect'
  | 'exchange.import'
  | 'peer.approve'
  | 'peer.retry'
  | 'profile.update'
  | 'roster.reorder'
  | 'session.start'
  | 'handoff.request'
  | 'handoff.cancel'
  | 'handoff.accept'
  | 'recovery.resume'
  | 'program.configure'
  | 'program.record.start'
  | 'program.record.stop'
  | 'private.load'
  | 'private.play'
  | 'private.pause'
  | 'private.seek'
  | 'private.gain'
  | 'private.unload'
  | 'private.state'
  | 'network.get'
  | 'network.configure'
  | 'network.clear'
  | 'network.test';
export interface JunctionLease { sessionId: string; epoch: string; actorPeerId: string }

/** Sanitized result of exchange.inspect. No secrets, bounded fields. */
export interface ExchangeInspection {
  kind: 'invite' | 'response' | 'notice';
  sessionName?: string;
  hostName?: string;
  hostPeerId?: string;
  peerId?: string;
  inviteId?: string;
  expiresAt?: number;
}

export type TurnMode = 'rest' | 'temporary';
export interface NetworkTurnConfig {
  mode: TurnMode;
  urls: string[];
  secret?: string;
  username?: string;
  credential?: string;
  expiresAt?: number;
}
export interface NetworkConfigureInput {
  stunUrls: string[];
  /** Omit or pass {} to remove TURN. */
  turn?: NetworkTurnConfig | Record<string, never>;
  save: boolean;
}
export interface NetworkSummary {
  stunUrls: string[];
  turn?: { mode: TurnMode; urls: string[]; hasSecret?: boolean; username?: string; expiresAt?: number };
  saved: boolean;
  errors?: string[];
}
export interface NetworkTestResult {
  state: 'checking' | 'success' | 'failure' | 'timeout';
  detail?: string;
  route?: ExchangeRoute;
}
