import type {JunctionParticipant, JunctionSnapshot, ProgramVenueSource} from '../../types/junction.ts';
import {coordinatorCanSelect, participantName, participantVisualState} from './roster-model.ts';

/**
 * Fixed vocabulary. JUNCTION MASTER is the previous DJ's *current* sound, LOCAL
 * NEXT is what this DJ will play, Program Master is what the venue hears. The
 * received master is never called "next".
 */
export const JUNCTION_MASTER = 'JUNCTION MASTER';
export const LOCAL_NEXT = 'LOCAL NEXT';
export const PROGRAM_MASTER = 'PROGRAM MASTER';

export interface JunctionRoles {
  coordinatorPeerId: string;
  /** Holds the operator role: the only DJ whose mixer controls Program. */
  operatorPeerId: string;
  nextPeerId: string;
  /** Lost the operator role but keeps sending until JUNCTION MASTER is faded out. */
  soundingPeerId: string;
  localCoordinator: boolean;
  localOperator: boolean;
  localNext: boolean;
  localSounding: boolean;
}

export function junctionRoles(snapshot: JunctionSnapshot): JunctionRoles {
  const operatorPeerId = snapshot.operatorPeerId ?? snapshot.performerPeerId ?? '';
  const coordinatorPeerId = snapshot.coordinatorPeerId || snapshot.hostPeerId;
  const nextPeerId = snapshot.nextPeerId ?? '';
  const soundingPeerId = snapshot.junctionInput?.releasingPeerId ?? '';
  const local = snapshot.localPeerId;
  return {
    coordinatorPeerId, operatorPeerId, nextPeerId, soundingPeerId,
    localCoordinator: Boolean(local) && local === coordinatorPeerId,
    localOperator: Boolean(local) && local === operatorPeerId,
    localNext: Boolean(local) && local === nextPeerId,
    localSounding: Boolean(local) && local === soundingPeerId,
  };
}

/**
 * - `waiting`: no previous DJ feeds JUNCTION MASTER.
 * - `cueing`: JUNCTION MASTER arrives; this DJ rehearses on CUE, Program is not theirs.
 * - `mixing`: this DJ operates Program while the previous DJ still sounds on JUNCTION MASTER.
 * - `playing`: this DJ operates Program alone.
 * - `sending`: this DJ handed over and keeps sending until the receiver fades them out.
 */
export type JunctionStage = 'inactive' | 'waiting' | 'cueing' | 'mixing' | 'playing' | 'sending';

export function junctionStage(snapshot: JunctionSnapshot | null | undefined): JunctionStage {
  if (!snapshot?.active) return 'inactive';
  const roles = junctionRoles(snapshot);
  if (roles.localSounding && !roles.localOperator) return 'sending';
  if (roles.localOperator) return roles.soundingPeerId ? 'mixing' : 'playing';
  return snapshot.junctionInput?.peerId ? 'cueing' : 'waiting';
}

export const STAGE_LABEL: Record<JunctionStage, string> = {
  inactive: 'Junction未接続',
  waiting: '前のDJの音声を待機',
  cueing: 'CUEで準備中（会場は前のDJ）',
  mixing: 'ミックス中（前のDJの音が残っています）',
  playing: '演奏中',
  sending: '交代済み・受け手が下げ切るまで送出中',
};

export interface ProgramMixerView {
  venueSource: ProgramVenueSource;
  venueLabel: string;
  /** True while the engine still sends a remote operator's master straight to the venue. */
  bypass: boolean;
  junctionMasterInProgram: boolean;
  localNextInProgram: boolean;
  localNextCueOnly: boolean;
  returnLabel: string;
  feedbackBlocked: boolean;
}

const VENUE_LABEL: Record<ProgramVenueSource, string> = {
  'none': '会場出力なし',
  'remote-host': '会場出力はホスト側',
  'direct-stream': '会場：操作中DJの送出音（このミキサーを通りません）',
  'local-mix': '会場：このミキサーのProgram Master',
};

/** Reads the native routing, or derives the same answer from an older runtime's snapshot. */
export function programMixerView(snapshot: JunctionSnapshot): ProgramMixerView {
  const roles = junctionRoles(snapshot);
  const host = snapshot.localPeerId === snapshot.hostPeerId;
  const native = snapshot.programMixer;
  const venueSource: ProgramVenueSource = native?.venueSource
    ?? (!host ? 'remote-host' : snapshot.program.state !== 'running' ? 'none' : roles.localOperator ? 'local-mix' : 'direct-stream');
  const returnSource = native?.returnFeed.source ?? 'none';
  const target = native?.returnFeed.targetPeerId
    ? snapshot.participants.find((participant) => participant.peerId === native.returnFeed.targetPeerId)
    : undefined;
  const targetName = target ? participantName(target) : '次のDJ';
  return {
    venueSource,
    venueLabel: VENUE_LABEL[venueSource],
    bypass: native?.directStreamBypass ?? venueSource === 'direct-stream',
    junctionMasterInProgram: native?.junctionMaster.inProgram ?? false,
    localNextInProgram: native?.localNext.inProgram ?? roles.localOperator,
    localNextCueOnly: native?.localNext.cueOnly ?? !roles.localOperator,
    returnLabel: returnSource === 'local-play' ? `${targetName}へ自分のローカル音だけを返送中`
      : returnSource === 'relayed-peer' ? `前のDJの音を${targetName}へ中継中`
        : native?.returnFeed.feedbackBlocked ? 'JUNCTION MASTERが会場ミックスにあるため返送を停止中' : '返送なし',
    feedbackBlocked: native?.returnFeed.feedbackBlocked ?? false,
  };
}

export type StripAction =
  | {kind: 'request'; label: string}
  | {kind: 'accept'; label: string; disabled: boolean}
  | {kind: 'release'; label: string}
  | {kind: 'nominate'; label: string; candidates: JunctionParticipant[]};

/** Handoff actions reachable from the player without opening the Junction panel. */
export function stripActions(snapshot: JunctionSnapshot): StripAction[] {
  if (!snapshot.active) return [];
  const roles = junctionRoles(snapshot);
  const actions: StripAction[] = [];
  const self = snapshot.participants.find((participant) => participant.peerId === snapshot.localPeerId);
  const selfState = self ? participantVisualState(self, snapshot) : undefined;
  if (roles.localOperator && roles.soundingPeerId) actions.push({kind: 'release', label: '前のDJを解放'});
  if (roles.localNext) actions.push({kind: 'accept', label: '準備OK・引き継ぐ', disabled: !snapshot.readiness.ready});
  else if (roles.localCoordinator && roles.nextPeerId && snapshot.readiness.ready) actions.push({kind: 'accept', label: '交代を確定', disabled: false});
  if (!roles.localCoordinator && !roles.localOperator && !roles.localNext && (selfState === 'ready' || selfState === 'finished')) {
    actions.push({kind: 'request', label: selfState === 'finished' ? 'もう一度演奏を希望' : '演奏を希望'});
  }
  // Operator switch and sender stop are separate: nobody new is nominated
  // while the previous DJ still sounds on JUNCTION MASTER.
  if (roles.localCoordinator && snapshot.lifecycle === 'live' && !roles.soundingPeerId && !roles.nextPeerId) {
    const candidates = snapshot.participants.filter((participant) => coordinatorCanSelect(participantVisualState(participant, snapshot)));
    if (candidates.length) actions.push({kind: 'nominate', label: '次のDJに指名', candidates});
  }
  return actions;
}
