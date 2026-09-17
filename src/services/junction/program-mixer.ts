import type {JunctionSnapshot, ProgramVenueSource} from '../../types/junction.ts';
import {participantName} from './roster-model.ts';

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
