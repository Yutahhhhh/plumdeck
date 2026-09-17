import type {JunctionDeckName, JunctionSnapshot, JunctionTurnBlocker, TurnCueKind, TurnSignal} from '../../types/junction.ts';

/**
 * Fader-start turn model, mirrored from native `junction::turn`. The native
 * runtime publishes `snapshot.turn`; this module derives the same answer from
 * the roles so the UI, the MCP bridge and tests read one vocabulary.
 */
export const FADER_START_CAPABILITY = 'junction-fader-start-v1';

export interface TurnRoles {
  active: boolean;
  local: string;
  owner: string;
  next: string;
  outgoing: string;
}

export function deriveSignal(roles: TurnRoles, ready: boolean): TurnSignal {
  if (!roles.active || !roles.local) return 'off';
  if (roles.local === roles.owner) return 'onair';
  if (roles.local === roles.outgoing) return 'outgoing';
  if (roles.local === roles.next) return ready ? 'ready' : 'standby';
  return 'off';
}

/** Fix-first order; only the first applicable blocker is shown. Same order as native `readyBlocker`. */
export const BLOCKER_ORDER: JunctionTurnBlocker[] = [
  'update_required', 'program_closed', 'waiting_junction', 'junction_unstable', 'latency_unknown',
  'latency_budget', 'host_waiting_stream', 'junction_not_unity', 'already_audible',
];

export const BLOCKER_TEXT: Record<JunctionTurnBlocker, string> = {
  update_required: '相手のアプリを更新してください',
  program_closed: '会場への音声出力がまだ開いていません',
  waiting_junction: '前のDJの音を受信するまでお待ちください',
  junction_unstable: '前のDJの音が途切れています。回線が安定するまでお待ちください',
  latency_unknown: '回線の遅延を測定しています',
  latency_budget: '回線の遅延が大きく、会場の音に間に合いません',
  host_waiting_stream: 'ホストがあなたの音を受信するまでお待ちください',
  junction_not_unity: 'JUNCTION MASTERを等倍・EQフラット・THRUに戻してください',
  already_audible: '一度フェーダーを下げてください',
};

export interface ReadyInputs {
  compatible: boolean;
  programOpen: boolean;
  junctionRequired: boolean;
  junctionReceiving: boolean;
  junctionStable: boolean;
  latencyMeasured: boolean;
  pathLatencyMs: number;
  budgetMs: number;
  standbyStreamRequired: boolean;
  standbyStreamReceived: boolean;
  junctionUnity: boolean;
  localSilent: boolean;
}

export function readyBlocker(input: ReadyInputs): JunctionTurnBlocker | '' {
  if (!input.compatible) return 'update_required';
  if (!input.programOpen) return 'program_closed';
  if (input.junctionRequired) {
    if (!input.junctionReceiving) return 'waiting_junction';
    if (!input.junctionStable) return 'junction_unstable';
  }
  if (!input.latencyMeasured) return 'latency_unknown';
  if (input.pathLatencyMs > input.budgetMs) return 'latency_budget';
  if (input.standbyStreamRequired && !input.standbyStreamReceived) return 'host_waiting_stream';
  if (!input.junctionUnity) return 'junction_not_unity';
  if (!input.localSilent) return 'already_audible';
  return '';
}

/** Silence-to-sound onset, identical constants to native `FaderStart`. */
export class FaderStart {
  static readonly threshold = 0.00316;
  static readonly onsetMs = 20;
  static readonly armMs = 300;
  private soundSince: number | null = null;
  private silentSince: number | null = null;
  armed = false;
  reset(): void { this.soundSince = null; this.silentSince = null; this.armed = false; }
  observe(level: number, junctionMoved: boolean, nowMs: number): boolean {
    const sound = junctionMoved || level >= FaderStart.threshold;
    if (!sound) {
      this.soundSince = null;
      if (this.silentSince === null) this.silentSince = nowMs;
      if (nowMs - this.silentSince >= FaderStart.armMs) this.armed = true;
      return false;
    }
    this.silentSince = null;
    if (!this.armed) return false;
    if (this.soundSince === null) this.soundSince = nowMs;
    if (nowMs - this.soundSince < FaderStart.onsetMs) return false;
    this.armed = false; this.soundSince = null; return true;
  }
}

const DECKS: JunctionDeckName[] = ['A', 'B', 'C', 'D'];
/** Same table as native `TailLock::check`; the engine enforces, the UI explains. */
export function tailLockReason(op: string, params: {deck?: string; leader?: string}, tailDecks: readonly string[]): string {
  if (op === 'mixer.crossfader' || op === 'mixer.master.gain' || op === 'mixer.beatfx.set') return '残りの曲を送出中はマスター系を操作できません';
  const tail = new Set(tailDecks.map((deck) => deck.toUpperCase()));
  if (op === 'deck.sync.set' && params.leader && tail.has(params.leader.toUpperCase())) return '残りの曲をSYNCのリーダーにはできません';
  const deckOp = op.startsWith('deck.') || op.startsWith('mixer.channel.') || ['mixer.eq.set', 'mixer.filter.set', 'mixer.trim.set', 'mixer.fx.set', 'mixer.colorfx.set'].includes(op);
  const deck = (params.deck ?? '').toUpperCase();
  if (!deckOp || !DECKS.includes(deck as JunctionDeckName) || !tail.has(deck)) return '';
  if (op.startsWith('deck.loop.') || op === 'mixer.channel.pfl' || op === 'deck.timing.trace') return '';
  return 'このデッキは残りの曲として送出中です。ループ以外は操作できません';
}

export const SIGNAL_LABEL: Record<TurnSignal, string> = {
  off: 'OFF',
  standby: 'STANDBY',
  ready: 'READY',
  onair: 'ON AIR',
  outgoing: 'OUTGOING',
};

export const CUE_LABEL: Record<TurnCueKind, string> = {
  one_more: 'あと1曲',
  go_ahead: '次どうぞ',
  hold: '少し待って',
  ok: 'OK',
};

function nameOf(snapshot: JunctionSnapshot, peerId: string): string {
  const participant = snapshot.participants.find((row) => row.peerId === peerId);
  return participant?.djName || participant?.displayName || 'DJ';
}

/** The local turn signal, from native `turn` when present, otherwise from the roles. */
export function turnSignal(snapshot: JunctionSnapshot | null | undefined): TurnSignal {
  if (!snapshot?.active) return 'off';
  if (snapshot.turn) return snapshot.turn.signal;
  return deriveSignal({
    active: snapshot.lifecycle !== 'lobby',
    local: snapshot.localPeerId,
    owner: snapshot.operatorPeerId ?? snapshot.performerPeerId ?? '',
    next: snapshot.nextPeerId ?? '',
    outgoing: snapshot.junctionInput?.releasingPeerId ?? '',
  }, false);
}

/** One line of booth guidance, in the DJ's words. */
export function turnGuidance(snapshot: JunctionSnapshot | null | undefined): string {
  if (!snapshot?.active) return '';
  const turn = snapshot.turn;
  const signal = turnSignal(snapshot);
  const owner = snapshot.operatorPeerId ?? snapshot.performerPeerId ?? '';
  const outgoing = turn?.outgoingPeerId ?? '';
  switch (signal) {
    case 'standby': {
      if (turn?.blocker?.code === 'already_audible') return `STANDBY — ${BLOCKER_TEXT.already_audible}`;
      if (!owner) return 'STANDBY — 最初のDJです。曲を用意してください';
      if (turn?.blocker?.code) return `STANDBY — ${BLOCKER_TEXT[turn.blocker.code]}`;
      return `STANDBY — ${nameOf(snapshot, owner)}の音を受信中。曲を用意してCUEで合わせてください`;
    }
    case 'ready':
      return 'READY — フェーダーを上げるとON AIR';
    case 'onair':
      if (outgoing) return `ON AIR — ${nameOf(snapshot, outgoing)}の曲が残っています。Jを下げると交代完了`;
      if (turn?.nextPeerId) return `ON AIR — 次は${nameOf(snapshot, turn.nextPeerId)}です`;
      return 'ON AIR';
    case 'outgoing':
      return `OUTGOING — 残りの曲を${nameOf(snapshot, owner)}が下げるまで送っています（ループのみ操作可）`;
    case 'off':
    default: {
      if (snapshot.lifecycle === 'lobby') return 'セッション開始を待っています';
      const next = turn?.nextPeerId;
      return next ? `次は${nameOf(snapshot, next)}です` : '';
    }
  }
}

/** Why a fader move did not put this DJ on air. */
export function notOnAirReason(snapshot: JunctionSnapshot | null | undefined): string {
  const code = snapshot?.turn?.blocker?.code;
  if (turnSignal(snapshot) !== 'standby' || !code) return '';
  return `まだ本番に出ていません：${BLOCKER_TEXT[code]}`;
}
