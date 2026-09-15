import type { ExchangeState, JunctionParticipant, JunctionSnapshot } from '../../types/junction';

export type ExchangeActionId = 'copy_invite' | 'paste_answer' | 'approve' | 'reject' | 'paste_invite' | 'copy_answer' | 'reexchange' | 'cancel' | 'copy_notice' | 'open_relay';
export interface ExchangeAction { id: ExchangeActionId; label: string; intent: 'primary' | 'default' | 'danger' }
export interface ExchangeGuidance {
  headline: string; hint?: string; waiting: boolean; error?: string;
  step: 1 | 2 | 3; actions: ExchangeAction[];
}
export function primaryExchangeAction(guidance: ExchangeGuidance): ExchangeAction | undefined {
  return guidance.actions.find((action) => action.intent === 'primary');
}
const A: Record<ExchangeActionId, ExchangeAction> = {
  copy_invite: {id: 'copy_invite', label: '招待をコピー', intent: 'primary'},
  paste_answer: {id: 'paste_answer', label: '返答を入力', intent: 'primary'},
  approve: {id: 'approve', label: 'このDJの接続を許可', intent: 'primary'},
  reject: {id: 'reject', label: '参加を許可しない', intent: 'danger'},
  paste_invite: {id: 'paste_invite', label: '新しい招待を入力', intent: 'primary'},
  copy_answer: {id: 'copy_answer', label: '返答をコピー', intent: 'primary'},
  reexchange: {id: 'reexchange', label: '再接続の招待を作る', intent: 'primary'},
  cancel: {id: 'cancel', label: 'この接続操作を中止', intent: 'danger'},
  copy_notice: {id: 'copy_notice', label: '中止・拒否の通知をコピー', intent: 'default'},
  open_relay: {id: 'open_relay', label: '中継設定を開く', intent: 'default'},
};
const secondary = (id: ExchangeActionId): ExchangeAction => ({...A[id], intent: 'default'});
const STATE_LABEL: Record<ExchangeState, string> = {
  idle: '待機中', collecting: '接続を準備中', invite_ready: '招待を渡す', awaiting_answer: '返答を待っています',
  approval_pending: '接続を許可してください', response_ready: '返答を渡す', awaiting_host: '管理DJの接続許可待ち',
  connecting: '接続中', connected: '接続済み', interrupted: '接続の復旧を待っています', needs_exchange: '再接続が必要',
  failed: '接続できませんでした', expired: '期限切れ', cancelled: '中止', rejected: '参加が許可されませんでした',
};
export function describeExchangeState(state: ExchangeState | undefined): string { return state ? STATE_LABEL[state] ?? state : '未接続'; }
export function exchangeConnected(state: ExchangeState | undefined): boolean { return state === 'connected'; }
export function formatExpiry(expiresAt: number | undefined, nowMs: number): string | undefined {
  if (!expiresAt) return undefined;
  const remain = Math.round((expiresAt - nowMs) / 1000);
  if (remain <= 0) return '有効期限切れ';
  return remain < 60 ? `有効期限まで約${remain}秒` : `有効期限まで約${Math.round(remain / 60)}分`;
}
// Copy acknowledgement belongs to this exact packet only. It never means delivered.
// `automatic`: this DJ's packets travel through share-musics; the clipboard stays a fallback.
export function deriveHostCardGuidance(participant: JunctionParticipant, copied = false, automatic = false): ExchangeGuidance {
  const x = participant.exchange;
  if (automatic) {
    const automaticGuidance = deriveAutomaticHostGuidance(participant);
    if (automaticGuidance) return automaticGuidance;
  }
  switch (x?.state) {
    case 'idle': case 'collecting':
      return {step: 1, headline: '招待を準備しています。', waiting: true, actions: [A.cancel]};
    case 'invite_ready': case 'awaiting_answer': {
      const next = copied || x.state === 'awaiting_answer';
      return {step: next ? 2 : 1, headline: next ? '招待をチャットで送り、相手から届いた返答を入力してください。' : '招待をコピーして、相手のDJへチャットで送ってください。',
        waiting: false, actions: next ? [A.paste_answer, secondary('copy_invite'), A.cancel] : [A.copy_invite, secondary('paste_answer'), A.cancel]};
    }
    case 'approval_pending':
      return {step: 3, headline: `${participant.djName || participant.displayName || '相手のDJ'}から返答が届きました。送り主を確認して接続を許可してください。`, waiting: false, actions: [A.approve, A.reject]};
    case 'connecting':
      return {step: 3, headline: '相手のDJに接続しています。', hint: '通常は45秒以内に接続結果が表示されます。', waiting: true, actions: [A.cancel]};
    case 'connected':
      return {step: 3, headline: '接続済みです。', waiting: false, actions: [secondary('reexchange')]};
    case 'interrupted':
      return {step: 3, headline: '通信が一時的に途切れています。同じ接続へ自動で再接続しています。', hint: 'セッションとDJの順番は維持されます。復旧不能と表示された場合だけ招待を作り直してください。', waiting: true, actions: [secondary('reexchange')]};
    case 'cancelled': case 'rejected':
      return {step: 1, headline: 'この接続操作は終了しました。相手にもチャットで伝えてください。', waiting: false, actions: [A.reexchange, A.copy_notice]};
    default:
      return {step: 1, headline: x?.state === 'expired' ? '招待の期限が切れました。新しい招待を送ってください。' : '新しい招待を送り、相手の新しい返答で再接続してください。',
        hint: '同じDJの枠を使います。セッションの作り直しは不要です。', error: x?.state === 'failed' ? x.detail || x.errorCode : undefined,
        waiting: false, actions: [A.reexchange, A.open_relay]};
  }
}
function deriveAutomaticHostGuidance(participant: JunctionParticipant): ExchangeGuidance | undefined {
  const x = participant.exchange;
  switch (x?.state) {
    case 'idle': case 'collecting':
      return {step: 1, headline: '招待を準備しています。できあがるとPlumDeck Lite経由で相手に届きます。', waiting: true, actions: [A.cancel]};
    case 'invite_ready': case 'awaiting_answer':
      return {step: 2, headline: 'PlumDeck Lite経由で招待を届けています。相手の返答を待っています。', hint: '相手のplumdeckが自動で返答します。コピーでの受け渡しは不要です。',
        waiting: true, actions: [secondary('copy_invite'), secondary('paste_answer'), A.cancel]};
    case 'approval_pending':
      return {step: 3, headline: `${participant.djName || participant.displayName || '相手のDJ'}から返答が届きました。参加申請を許可済みのため、自動で接続を許可しています。`,
        waiting: true, actions: [A.approve, A.reject]};
    case 'cancelled': case 'rejected':
      return {step: 1, headline: 'この接続操作は終了しました。相手にはPlumDeck Lite経由で通知しています。', waiting: false, actions: [A.reexchange]};
    case 'needs_exchange': case 'failed': case 'expired':
      return {step: 1, headline: '再接続の招待を作ると、PlumDeck Lite経由で相手に届き、自動で再接続します。',
        hint: '同じDJの枠を使います。セッションの作り直しは不要です。', error: x.state === 'failed' ? x.detail || x.errorCode : undefined,
        waiting: false, actions: [A.reexchange, A.open_relay]};
    default:
      return undefined;
  }
}
/** The guest's own connection, displayed at session level, never as a host-row action. */
export function deriveGuestGuidance(snapshot: JunctionSnapshot, copied = false, automatic = false): ExchangeGuidance {
  const x = snapshot.participants.find((p) => p.peerId === snapshot.hostPeerId)?.exchange ?? snapshot.exchange;
  if (automatic) {
    switch (x?.state) {
      case 'idle': case 'collecting':
        return {step: 2, headline: 'PlumDeck Lite経由で招待を受け取りました。返答を準備しています。', waiting: true, actions: [A.cancel]};
      case 'response_ready': case 'awaiting_host':
        return {step: 3, headline: 'PlumDeck Lite経由で返答を届けています。管理DJ側で自動的に接続されます。', waiting: true,
          actions: [secondary('copy_answer'), secondary('paste_invite'), A.cancel]};
      case 'needs_exchange': case 'failed': case 'expired': case 'cancelled': case 'rejected':
        return {step: 1, headline: '管理DJが再接続の招待を作ると、PlumDeck Lite経由で届いて自動で再接続します。',
          error: x.state === 'failed' ? x.detail || x.errorCode : undefined, waiting: true, actions: [secondary('paste_invite')]};
    }
  }
  switch (x?.state) {
    case 'idle': case 'collecting':
      return {step: 2, headline: '招待を受け取りました。あなたの返答を準備しています。', waiting: true, actions: [A.cancel]};
    case 'response_ready': case 'awaiting_host': {
      const next = copied || x.state === 'awaiting_host';
      return {step: next ? 3 : 2, headline: next ? 'コピーした返答をチャットで管理DJへ送ってください。相手が接続を許可すると自動でつながります。' : '返答をコピーして、招待をくれた管理DJへチャットで送ってください。',
        waiting: next, actions: next ? [secondary('copy_answer'), secondary('paste_invite'), A.cancel] : [A.copy_answer, secondary('paste_invite'), A.cancel]};
    }
    case 'connecting':
      return {step: 3, headline: 'セッションに接続しています。', waiting: true, actions: [A.cancel]};
    case 'connected':
      return {step: 3, headline: 'セッションに接続済みです。', waiting: false, actions: [secondary('paste_invite')]};
    case 'interrupted':
      return {step: 3, headline: '通信が一時的に途切れています。同じ接続へ自動で再接続しています。', hint: 'セッションとDJの順番は維持されます。復旧不能と表示された場合だけ管理DJから新しい招待を受け取ってください。', waiting: true, actions: [secondary('paste_invite')]};
    default:
      return {step: 1, headline: '管理DJに再接続の招待をお願いし、届いた新しい招待を入力してください。',
        hint: '取り込むと新しい返答が作られます。その返答を管理DJへ送り返してください。',
        error: x?.state === 'failed' ? x.detail || x.errorCode : undefined, waiting: false, actions: [A.paste_invite]};
  }
}
