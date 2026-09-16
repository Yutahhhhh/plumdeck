import type { JunctionSnapshot } from '@/types/junction';
import { describeExchangeState } from '@/services/junction/exchange-actions';
import { participantName } from '@/services/junction/roster-model';

const CONNECTION_LABEL: Record<string, string> = {
  stable: '安定', connected: '接続済み', connecting: '接続中', unknown: '確認中', interrupted: '中断', reconnecting: '復旧待ち', error: '要確認', pending: '管理者の操作待ち', closing: '終了中', disconnected: '未接続',
};

interface Props {
  snapshot: JunctionSnapshot | null;
  open: boolean;
  onToggle: () => void;
}

/** Compact root control. Identity, order and actions live in the roster panel. */
export function JunctionRootButton({ snapshot, open, onToggle }: Props) {
  const active = Boolean(snapshot?.active);
  const nameOf = (id?: string) => {
    if (!id) return '—';
    const p = snapshot?.participants.find((x) => x.peerId === id);
    return p ? participantName(p) : '確認中';
  };
  const pending = active
    ? snapshot!.participants.filter((p) => p.exchange?.state === 'approval_pending' || ['failed','expired','needs_exchange','interrupted'].includes(p.exchange?.state ?? '')).length
    : 0;
  const status = active
    ? snapshot?.lifecycle === 'lobby'
      ? '準備中'
      : CONNECTION_LABEL[snapshot!.connection.state] ?? describeExchangeState(snapshot!.exchange?.state) ?? '確認中'
    : '停止中';

  return (
    <button
      id="junction-toggle"
      aria-controls="junction-panel"
      type="button"
      className="junction-root"
      aria-expanded={open}
      onClick={onToggle}
      title="Junction パネルを開閉"
    >
      <b>Junction</b>
      {active ? (
        <>
          <span className="junction-root-name" title={snapshot?.sessionName || undefined}>
            {snapshot?.sessionName?.trim() || '無名のセッション'}
          </span>
          {snapshot?.performerPeerId
            ? <span>演奏：{nameOf(snapshot.performerPeerId)}</span>
            : <span>{status}</span>}
          {snapshot?.nextPeerId && <span className="junction-root-dim">次のDJ：{nameOf(snapshot.nextPeerId)}</span>}
          {pending > 0 && <span className="junction-root-badge" aria-label={`未対応 ${pending} 件`}>{pending}</span>}
        </>
      ) : (
        <span className="junction-root-dim">セッションなし</span>
      )}
    </button>
  );
}
