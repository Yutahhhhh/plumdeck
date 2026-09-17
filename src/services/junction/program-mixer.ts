import type {JunctionSnapshot, ProgramVenueSource} from '../../types/junction.ts';
import {participantName} from './roster-model.ts';

export interface ProgramMixerView {
  venueLabel: string;
  returnLabel: string;
}

const VENUE_LABEL: Record<ProgramVenueSource, string> = {
  'none': '会場出力なし',
  'remote-host': '会場出力はホスト側',
  'direct-stream': '会場：操作中DJの送出音（このミキサーを通りません）',
  'local-mix': '会場：このミキサーのProgram Master',
};

/** Reads the native routing, or derives the same answer from an older runtime's snapshot. */
export function programMixerView(snapshot: JunctionSnapshot): ProgramMixerView {
  const operatorPeerId = snapshot.operatorPeerId ?? snapshot.performerPeerId ?? '';
  const localOperator = Boolean(snapshot.localPeerId) && snapshot.localPeerId === operatorPeerId;
  const host = snapshot.localPeerId === snapshot.hostPeerId;
  const native = snapshot.programMixer;
  const venueSource: ProgramVenueSource = native?.venueSource
    ?? (!host ? 'remote-host' : snapshot.program.state !== 'running' ? 'none' : localOperator ? 'local-mix' : 'direct-stream');
  const returnSource = native?.returnFeed.source ?? 'none';
  const target = native?.returnFeed.targetPeerId
    ? snapshot.participants.find((participant) => participant.peerId === native.returnFeed.targetPeerId)
    : undefined;
  const targetName = target ? participantName(target) : '次のDJ';
  return {
    venueLabel: VENUE_LABEL[venueSource],
    returnLabel: returnSource === 'local-play' ? `${targetName}へ自分のローカル音だけを返送中`
      : returnSource === 'relayed-peer' ? `前のDJの音を${targetName}へ中継中`
        : native?.returnFeed.feedbackBlocked ? 'JUNCTION MASTERが会場ミックスにあるため返送を停止中' : '返送なし',
  };
}
