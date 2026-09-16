import type {
  ExchangeState,
  JunctionConnectionQuality,
  JunctionParticipant,
  JunctionSnapshot,
} from '../../types/junction.ts';

export const DJ_NAME_MAX_LENGTH = 40;

export type RosterVisualState =
  | 'invited'
  | 'response'
  | 'connecting'
  | 'ready'
  | 'requested'
  | 'next'
  | 'playing'
  | 'finished'
  | 'reconnecting'
  | 'disconnected'
  | 'problem';

export interface QualityPresentation {
  level: JunctionConnectionQuality['level'];
  label: string;
  bars: 0 | 1 | 2 | 3;
  detail?: string;
}

/**
 * Every connected DJ is a candidate: a turn request only joins the queue, and a
 * DJ who already played may be chosen again.
 */
export function coordinatorCanSelect(state: RosterVisualState): boolean {
  return state === 'ready' || state === 'requested' || state === 'finished';
}

/** Only the performer keeps a fixed position; played DJs can be queued again. */
export function rosterPositionLocked(state: RosterVisualState): boolean {
  return state === 'playing';
}

/** Only the coordinator withdraws a pending (not yet committed) turn. */
export function handoffCancelAvailable(state: RosterVisualState, coordinator: boolean): boolean {
  return coordinator && state === 'next';
}

export function turnRequestAvailable(state: RosterVisualState, coordinator: boolean, self: boolean): boolean {
  return !coordinator && self && (state === 'ready' || state === 'finished');
}

const EXCHANGE_PROBLEM = new Set<ExchangeState>([
  'failed',
  'expired',
  'needs_exchange',
  'rejected',
  'cancelled',
]);

/** Keep the user-facing DJ identity independent from legacy runtime wording. */
export function participantName(participant: JunctionParticipant): string {
  return participant.djName?.trim() || participant.displayName?.trim() || 'DJ名を確認中';
}

export function limitDjName(value: string): string {
  return value.slice(0, DJ_NAME_MAX_LENGTH);
}

export function compactReadinessReasons(reasons: string[], limit = 2): string | undefined {
  const unique = [...new Set(reasons.map((reason) => reason.trim()).filter(Boolean))];
  if (!unique.length) return undefined;
  const shown = unique.slice(0, limit).join('・');
  return unique.length > limit ? `${shown}（ほか${unique.length - limit}件）` : shown;
}

export function connectionAlert(snapshot: JunctionSnapshot): string | undefined {
  const serious = new Set(['error', 'interrupted', 'disconnected', 'reconnecting', 'failed']);
  if (!serious.has(snapshot.connection.state)) return undefined;
  return snapshot.connection.detail?.trim() || 'セッションの接続を確認してください。';
}

export function orderedParticipants(participants: JunctionParticipant[]): JunctionParticipant[] {
  return participants
    .map((participant, originalIndex) => ({participant, originalIndex}))
    .sort((a, b) => {
      const ai = Number.isFinite(a.participant.orderIndex) ? a.participant.orderIndex! : a.originalIndex;
      const bi = Number.isFinite(b.participant.orderIndex) ? b.participant.orderIndex! : b.originalIndex;
      return ai - bi || a.originalIndex - b.originalIndex;
    })
    .map(({participant}) => participant);
}

/**
 * Converts transport/runtime detail into the small set of states shown in the
 * roster. Raw WebRTC terminology deliberately stays out of the normal UI.
 */
export function participantVisualState(
  participant: JunctionParticipant,
  snapshot: JunctionSnapshot,
): RosterVisualState {
  if (participant.peerId === snapshot.performerPeerId || participant.isPerformer) return 'playing';
  if (participant.peerId === snapshot.nextPeerId || participant.isNextUp) return 'next';

  const explicit = participant.rosterStatus?.toLowerCase();
  if (explicit === 'performing' || explicit === 'playing') return 'playing';
  if (explicit === 'next') return 'next';
  if (explicit === 'finished') return 'finished';
  if (explicit === 'requested') return 'requested';
  if (explicit === 'ready' || explicit === 'waiting') return 'ready';
  if (explicit === 'unstable' || explicit === 'reconnecting') return 'reconnecting';
  if (explicit === 'disconnected' || explicit === 'offline') return 'disconnected';
  if (explicit === 'response' || explicit === 'response_pending' || explicit === 'approval_pending') return 'response';
  if (explicit === 'invited' || explicit === 'invite_ready' || explicit === 'awaiting_answer') return 'invited';
  if (explicit === 'connecting' || explicit === 'pending' || explicit === 'collecting') return 'connecting';

  const exchange = participant.exchange?.state;
  if (exchange && EXCHANGE_PROBLEM.has(exchange)) return 'problem';
  if (exchange === 'approval_pending') return 'response';
  if (exchange === 'collecting' || exchange === 'connecting') return 'connecting';
  if (exchange === 'invite_ready' || exchange === 'awaiting_answer') return 'invited';
  if (exchange === 'interrupted') return 'reconnecting';
  if (exchange === 'connected' || participant.status === 'connected') return 'ready';
  if (participant.status === 'reconnecting' || participant.status === 'interrupted') return 'reconnecting';
  if (participant.status === 'disconnected' || participant.status === 'offline') return 'disconnected';
  if (participant.approved === false) return 'invited';
  return 'ready';
}

export function qualityPresentation(quality?: JunctionConnectionQuality): QualityPresentation {
  const level = quality?.level ?? 'unknown';
  const values: string[] = [];
  if (typeof quality?.rttMs === 'number') values.push(`遅延 ${Math.round(quality.rttMs)}ms`);
  if (typeof quality?.packetLossPct === 'number') values.push(`損失 ${quality.packetLossPct.toFixed(1)}%`);
  if (typeof quality?.jitterMs === 'number') values.push(`揺らぎ ${Math.round(quality.jitterMs)}ms`);
  const detail = values.length ? values.join(' / ') : undefined;
  switch (level) {
    case 'good': return {level, label: '通信良好', bars: 3, detail};
    case 'fair': return {level, label: '通信注意', bars: 2, detail};
    case 'poor': return {level, label: '通信不安定', bars: 1, detail};
    case 'offline': return {level, label: '未接続', bars: 0, detail};
    default: return {level: 'unknown', label: '通信確認中', bars: 0, detail};
  }
}

export function stableThemeColor(name: string): string {
  const palette = ['#0f766e', '#0369a1', '#6d28d9', '#be185d', '#b45309', '#4d7c0f', '#4338ca', '#a21caf'];
  let hash = 0;
  for (const char of name.trim() || 'DJ') hash = (hash * 31 + char.codePointAt(0)!) >>> 0;
  return palette[hash % palette.length];
}

/** Only raster image data URLs are rendered; SVG/HTML payloads are rejected. */
export function safeAvatarDataUrl(value?: string): string | undefined {
  if (!value || value.length > 4_096) return undefined;
  return /^data:image\/(?:png|jpeg|webp);base64,[a-z0-9+/=]+$/i.test(value) ? value : undefined;
}

/** Empty is meaningful: it tells the native profile update to remove an avatar. */
export function profileAvatarValue(value?: string): string | undefined {
  return value === '' ? '' : safeAvatarDataUrl(value);
}

export function reorderPeerIds(
  participants: JunctionParticipant[],
  activePeerId: string,
  overPeerId: string,
  lockedPeerIds: ReadonlySet<string> = new Set(),
): string[] {
  const ids = orderedParticipants(participants).map((participant) => participant.peerId);
  if (lockedPeerIds.has(activePeerId)) return ids;
  const movable = ids.filter((peerId) => !lockedPeerIds.has(peerId));
  const from = movable.indexOf(activePeerId);
  let to = movable.indexOf(overPeerId);
  if (from < 0) return ids;
  if (to < 0) {
    const overIndex = ids.indexOf(overPeerId);
    if (overIndex < 0) return ids;
    const nextMovable = ids.slice(overIndex + 1).find((peerId) => !lockedPeerIds.has(peerId));
    const previousMovable = [...ids.slice(0, overIndex)].reverse().find((peerId) => !lockedPeerIds.has(peerId));
    const target = ids.indexOf(activePeerId) > overIndex ? nextMovable : previousMovable;
    to = target ? movable.indexOf(target) : from;
  }
  if (from !== to) {
    const [moved] = movable.splice(from, 1);
    movable.splice(to, 0, moved);
  }
  let cursor = 0;
  return ids.map((peerId) => lockedPeerIds.has(peerId) ? peerId : movable[cursor++]);
}
