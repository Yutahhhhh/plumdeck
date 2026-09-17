import { useEffect, useMemo, useState } from 'react';
import {
  closestCenter,
  DndContext,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type { ExchangeActionId } from '@/services/junction/exchange-actions';
import { deriveHostCardGuidance } from '@/services/junction/exchange-actions';
import {
  orderedParticipants,
  connectionAlert,
  participantName,
  participantVisualState,
  qualityPresentation,
  reorderPeerIds,
  rosterPositionLocked,
  rosterTurnActions,
  safeAvatarDataUrl,
  stableThemeColor,
  type RosterVisualState,
  type TurnAction,
} from '@/services/junction/roster-model';
import type { JunctionParticipant, JunctionSnapshot } from '@/types/junction';
import { ExchangeFlow } from './ExchangeFlow';

const STATE_LABEL: Record<RosterVisualState, string> = {
  invited: '招待中',
  response: '返答あり',
  connecting: '接続中',
  ready: '接続済み',
  next: 'STANDBY',
  nextReady: 'READY',
  playing: 'ON AIR',
  outgoing: 'OUTGOING',
  finished: '順番の外',
  reconnecting: '自動再接続中',
  disconnected: '切断',
  problem: '要対応',
};

interface Props {
  snapshot: JunctionSnapshot;
  host: boolean;
  busyKey?: string;
  errors: Record<string, string>;
  copiedPackets: Record<string, string | undefined>;
  /** Peers whose invite/response travel through share-musics. */
  automaticPeers?: Set<string>;
  onExchangeAction: (peerId: string, action: ExchangeActionId) => void;
  onImportText: (peerId: string, text: string) => Promise<void>;
  onTurn: (action: TurnAction) => void;
  onReorder: (peerIds: string[]) => Promise<void>;
}

/** The session's single source of truth: identity, order, connection and action. */
export function JunctionRoster({
  snapshot,
  host,
  busyKey,
  errors,
  copiedPackets,
  automaticPeers,
  onExchangeAction,
  onImportText,
  onTurn,
  onReorder,
}: Props) {
  const source = useMemo(() => orderedParticipants(snapshot.participants), [snapshot.participants]);
  const sourceKey = source.map((participant) => `${participant.peerId}:${participant.orderIndex ?? ''}`).join('|');
  const [peerOrder, setPeerOrder] = useState(() => source.map((participant) => participant.peerId));
  const sensors = useSensors(
    useSensor(PointerSensor, {activationConstraint: {distance: 6}}),
    useSensor(KeyboardSensor, {coordinateGetter: sortableKeyboardCoordinates}),
  );

  useEffect(() => {
    setPeerOrder(source.map((participant) => participant.peerId));
  }, [sourceKey]);

  const byId = new Map(source.map((participant) => [participant.peerId, participant]));
  const participants = [
    ...peerOrder.map((peerId) => byId.get(peerId)).filter((participant): participant is JunctionParticipant => Boolean(participant)),
    ...source.filter((participant) => !peerOrder.includes(participant.peerId)),
  ];
  const connectionProblem = connectionAlert(snapshot);

  const onDragEnd = async ({active, over}: DragEndEvent) => {
    if (!over || active.id === over.id) return;
    const previous = participants.map((participant) => participant.peerId);
    const locked = new Set(participants
      .filter((participant) => rosterPositionLocked(participantVisualState(participant, snapshot)))
      .map((participant) => participant.peerId));
    const next = reorderPeerIds(participants, String(active.id), String(over.id), locked);
    setPeerOrder(next);
    try {
      await onReorder(next);
    } catch {
      setPeerOrder(previous);
    }
  };

  return (
    <section className="junction-roster" aria-labelledby="junction-roster-title">
      <header className="junction-roster-head">
        <div>
          <h3 id="junction-roster-title">DJ一覧</h3>
          <p>{participants.length}人 · 上から演奏順（タイムテーブル） · 次のDJはフェーダーを上げるとON AIRになります</p>
        </div>
        {host && participants.length > 1 && <small>ドラッグで順番を変更</small>}
      </header>
      {connectionProblem && (
        <p className={`junction-roster-alert${['interrupted', 'reconnecting'].includes(snapshot.connection.state) ? ' is-reconnecting' : ''}`} role={['interrupted', 'reconnecting'].includes(snapshot.connection.state) ? 'status' : 'alert'}>
          {['interrupted', 'reconnecting'].includes(snapshot.connection.state) ? '自動再接続中：' : '接続を確認してください：'}{connectionProblem}
        </p>
      )}
      <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={(event) => void onDragEnd(event)}>
        <SortableContext items={participants.map((participant) => participant.peerId)} strategy={verticalListSortingStrategy}>
          <ol className="junction-roster-list">
            {participants.map((participant, index) => (
              <RosterRow
                key={participant.peerId}
                index={index}
                participant={participant}
                snapshot={snapshot}
                host={host}
                busy={busyKey === participant.peerId || busyKey === 'handoff'}
                error={errors[participant.peerId]}
                copiedPacket={copiedPackets[participant.peerId]}
                automatic={Boolean(automaticPeers?.has(participant.peerId))}
                onExchangeAction={onExchangeAction}
                onImportText={onImportText}
                onTurn={onTurn}
              />
            ))}
          </ol>
        </SortableContext>
      </DndContext>
      {(errors.roster || errors.handoff) && (
        <p className="junction-card-error" role="alert">{errors.roster || errors.handoff}</p>
      )}
    </section>
  );
}

interface RowProps {
  index: number;
  participant: JunctionParticipant;
  snapshot: JunctionSnapshot;
  host: boolean;
  busy: boolean;
  error?: string;
  copiedPacket?: string;
  automatic: boolean;
  onExchangeAction: (peerId: string, action: ExchangeActionId) => void;
  onImportText: (peerId: string, text: string) => Promise<void>;
  onTurn: (action: TurnAction) => void;
}

function RosterRow({
  index,
  participant,
  snapshot,
  host,
  busy,
  error,
  copiedPacket,
  automatic,
  onExchangeAction,
  onImportText,
  onTurn,
}: RowProps) {
  const state = participantVisualState(participant, snapshot);
  const isSelf = participant.peerId === snapshot.localPeerId;
  const isHost = participant.peerId === snapshot.hostPeerId || participant.isHost;
  const sortable = host && !rosterPositionLocked(state);
  const incompatible = Boolean(snapshot.turn?.incompatiblePeerIds.includes(participant.peerId));
  const {attributes, listeners, setNodeRef, transform, transition, isDragging} = useSortable({
    id: participant.peerId,
    disabled: !sortable,
  });
  const style = {transform: CSS.Transform.toString(transform), transition};
  const guidance = host && !isSelf && participant.exchange ? deriveHostCardGuidance(participant, Boolean(copiedPacket && copiedPacket === participant.exchange.inviteText), automatic) : undefined;
  const actions = rosterTurnActions(participant, snapshot, host);
  const quality = qualityPresentation(
    participant.connectionQuality ?? (state === 'disconnected' ? {level: 'offline'} : state === 'reconnecting' ? {level: 'poor'} : undefined),
  );
  const nextStatus = (state === 'next' || state === 'nextReady') && !isSelf && snapshot.turn?.nextStatus
    ? {receiving: '受信中', loaded: 'ロード済み', cueing: 'CUE中', ready: 'READY'}[snapshot.turn.nextStatus]
    : undefined;

  return (
    <li
      ref={setNodeRef}
      style={style}
      className={`junction-roster-row junction-roster-${state}${isDragging ? ' is-dragging' : ''}`}
      aria-busy={busy}
    >
      <div className="junction-roster-main">
        <div className="junction-roster-order">
          {sortable ? (
            <button
              type="button"
              className="junction-drag-handle"
              aria-label={`${participantName(participant)}を並べ替え`}
              {...attributes}
              {...listeners}
            >
              <span aria-hidden="true">⠿</span><span>{index + 1}</span>
            </button>
          ) : <span>{index + 1}</span>}
        </div>
        <DjAvatar participant={participant} />
        <div className="junction-roster-identity">
          <strong title={participantName(participant)}>{participantName(participant)}</strong>
          <div className="junction-roster-badges">
            {isSelf && <span className="junction-badge junction-badge-self">あなた</span>}
            {isHost && <span className="junction-badge" title="セッションの管理者。演奏者や操作権とは別です">ホスト</span>}
            {incompatible && <span className="junction-badge" title="相手のアプリを更新してください。更新するまで順番に入れません">要アップデート</span>}
            {nextStatus && <span className="junction-badge" title="次のDJの準備状況">{nextStatus}</span>}
            <span className={`junction-state junction-state-${state}`}>{STATE_LABEL[state]}</span>
          </div>
        </div>
        <ConnectionIndicator presentation={quality} />
      </div>

      {actions.length > 0 && (
        <div className="junction-roster-actions">
          {actions.map((action) => (
            <button key={action.op} type="button" title={action.title}
              className={`junction-btn ${action.primary ? 'junction-btn-primary junction-row-primary' : 'junction-btn-default'}`}
              disabled={busy} onClick={() => onTurn(action)}>
              {action.label}
            </button>
          ))}
        </div>
      )}

      {guidance && <ExchangeFlow
        key={`${participant.peerId}:${participant.exchange?.inviteId ?? ''}`}
        guidance={guidance} host connected={participant.exchange?.state === 'connected'} automatic={automatic} busy={busy} error={error}
        onAction={(action) => onExchangeAction(participant.peerId, action)}
        onImport={(text) => onImportText(participant.peerId, text)}
      />}
      {error && !guidance && <p className="junction-card-error" role="alert">{error}</p>}
    </li>
  );
}

function DjAvatar({participant}: {participant: JunctionParticipant}) {
  const name = participantName(participant);
  const avatar = safeAvatarDataUrl(participant.avatarDataUrl);
  const color = /^#[0-9a-f]{6}$/i.test(participant.themeColor ?? '')
    ? participant.themeColor
    : stableThemeColor(name);
  return (
    <span className="junction-avatar" style={{backgroundColor: color}} aria-hidden="true">
      {avatar ? <img src={avatar} alt="" /> : name.slice(0, 2).toLocaleUpperCase()}
    </span>
  );
}

function ConnectionIndicator({presentation}: {presentation: ReturnType<typeof qualityPresentation>}) {
  const title = presentation.detail ? `${presentation.label} · ${presentation.detail}` : presentation.label;
  return (
    <span className={`junction-quality junction-quality-${presentation.level}`} title={title} aria-label={title}>
      {[1, 2, 3].map((bar) => <i key={bar} className={bar <= presentation.bars ? 'active' : ''} />)}
      <small>{presentation.label.replace('通信', '')}</small>
    </span>
  );
}
