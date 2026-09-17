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
  orderIndex?: number;
  rosterStatus?: string;
  connectionQuality?: JunctionConnectionQuality;
  /** Set for PlumDeck Lite (phone) participants. */
  client?: 'lite';
  isHost?: boolean;
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
  performerPeerId: string;
  nextPeerId?: string;
  lifecycle?: 'lobby' | 'live';
  epoch: string;
  handoffState: string;
  participants: JunctionParticipant[];
  program: { state: string; meter?: number; outputDevice?: string; recording?: boolean };
  connection: { state: string; detail?: string };
  /** Legacy single server invite. Manual mode uses per-participant exchange.inviteText. */
  invite?: string;
  exchange?: SnapshotExchange;
  /** JUNCTION MASTER: the previous DJ's master as a local virtual input channel. */
  junctionInput?: JunctionInputState;
  /** Who holds the operator role (controls Program). Equal to performer today. */
  operatorPeerId?: string;
  /** Local-only routing of Program Master, JUNCTION MASTER, LOCAL NEXT and the return feed. */
  programMixer?: JunctionProgramMixer;
  /** Fader-start turn state for this DJ. */
  turn?: JunctionTurnState;
}
export type TurnSignal = 'off' | 'standby' | 'ready' | 'onair' | 'outgoing';
export type JunctionTurnBlocker =
  | 'update_required' | 'program_closed' | 'waiting_junction' | 'junction_unstable' | 'latency_unknown'
  | 'latency_budget' | 'host_waiting_stream' | 'junction_not_unity' | 'already_audible';
export type TurnCueKind = 'one_more' | 'go_ahead' | 'hold' | 'ok';
/** Next DJ's preparation as the ON AIR DJ sees it. */
export type TurnNextStatus = '' | 'receiving' | 'loaded' | 'cueing' | 'ready';
export interface JunctionTurnState {
  signal: TurnSignal;
  nextPeerId: string;
  outgoingPeerId: string;
  blocker: { code: JunctionTurnBlocker | ''; text: string };
  nextStatus: TurnNextStatus;
  /** Decks this DJ still sends as the tail while OUTGOING. */
  tailDecks: JunctionDeckName[];
  /** B2B: DJs who finish rejoin the end of the timetable. */
  repeat: boolean;
  /** Out of the queue until they join again. */
  outOfQueue: string[];
  /** Peers that cannot negotiate fader start. */
  incompatiblePeerIds: string[];
  cue?: { kind: TurnCueKind; fromPeerId: string; at: number };
  latency?: { pathMs: number; budgetMs: number };
  /** This DJ's own sound (or a J move) is up right now. */
  localAudible?: boolean;
  /** A READY DJ replaces a disconnected ON AIR DJ without asking the host. */
  autoFailover: boolean;
}
/**
 * - `local-mix`: Program Master = this mixer (JUNCTION MASTER + LOCAL NEXT).
 * - `direct-stream`: the remote operator's master reaches the venue without
 *   this mixer (engine path kept until Program Master can carry it).
 * - `remote-host`: a guest; the host owns the venue output.
 */
export type ProgramVenueSource = 'none' | 'remote-host' | 'direct-stream' | 'local-mix';
export interface JunctionProgramMixer {
  venueSource: ProgramVenueSource;
  /** Only local play is ever returned; `relayed-peer` is the host forwarding one Lite DJ to the next. */
  returnFeed: {
    source: 'none' | 'local-play' | 'relayed-peer';
    targetPeerId: string;
    feedbackBlocked: boolean;
  };
}
export type JunctionDeckName = 'A' | 'B' | 'C' | 'D';
/** What the remote Lite DJ reports for one of its decks (shown inside JUNCTION MASTER). */
export interface JunctionInputDeck {
  deck: JunctionDeckName;
  /** The remote DJ's own deck role; never this computer's LOCAL NEXT. */
  role: 'current' | 'next';
  title: string;
  artist: string;
  bpm: number;
  positionMs: number;
  durationMs: number;
  rate: number;
  /** Channel fader x crossfader gain on the remote mixer, 0..1. */
  audibility: number;
  playing: boolean;
  /** Beat grid anchor; absent from Lite builds that do not send it. */
  firstBeatMs?: number;
  beatsPerBar: number;
}
export interface JunctionInputChannel {
  available: boolean;
  volume?: number;
  /** Crossfader assign: 0 left, 1 thru, 2 right. */
  orientation?: 0 | 1 | 2;
  pfl?: boolean;
  pflAvailable?: boolean;
  eqLow?: number;
  eqMid?: number;
  eqHigh?: number;
  vu?: number;
  /** False once the fader or crossfader has removed it from the mix. */
  audible?: boolean;
}
export interface JunctionInputState {
  /** Source peer; empty when nobody feeds the JUNCTION deck. */
  peerId: string;
  /** Previous performer still sounding here until faded out. */
  releasingPeerId: string;
  receiving: boolean;
  latencyMs: number;
  underruns: string;
  channel: JunctionInputChannel;
  djName?: string;
  decks?: JunctionInputDeck[];
  audibleDeck?: JunctionDeckName | '';
  /** Recent peaks, oldest first, with the remote deck that was loudest then. */
  lane: { bucketMs: number; peaks: number[]; decks: (JunctionDeckName | '')[] };
}
export interface JunctionInputSettings {
  volume?: number;
  orientation?: 0 | 1 | 2;
  pfl?: boolean;
  eqLow?: number;
  eqMid?: number;
  eqHigh?: number;
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
  | 'recovery.resume'
  | 'turn.join'
  | 'turn.leave'
  | 'turn.repeat'
  | 'turn.failover'
  | 'turn.onair'
  | 'turn.force'
  | 'turn.skip'
  | 'turn.release'
  | 'turn.cue'
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
  | 'network.test'
  | 'lite.join'
  | 'lite.exchange'
  | 'lite.guest.offer'
  | 'lite.peer.ensure'
  | 'lite.peer.answer'
  | 'lite.peer.remove'
  | 'lite.owner.set'
  | 'lite.roster.set'
  | 'input.set'
  | 'input.release';

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
