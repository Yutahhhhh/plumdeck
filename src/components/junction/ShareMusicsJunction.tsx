import { useEffect, useState } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import type { JunctionSnapshot } from '@/types/junction';
import { describeExchangeState } from '@/services/junction/exchange-actions';
import type { HostMemory, SharedMember } from '@/services/junction/share-musics/automation';
import {
  cancelSignIn,
  decideRequest,
  dismissNotice,
  endLink,
  LITE_PEERS_READY,
  publishSession,
  refreshAccount,
  refreshSessions,
  requestJoin,
  retryRequest,
  signIn,
  signOut,
  type ShareMusicsProfile,
} from '@/services/junction/share-musics/coordinator';
import { useShareMusics } from '@/hooks/useShareMusics';

const SESSION_REFRESH_MS = 5000;

const errorText = (cause: unknown) => cause instanceof Error ? cause.message : String(cause);

/** Sign-in state. Everything else in Junction works the same without it. */
export function ShareMusicsAccountCard({open}: {open: boolean}) {
  const s = useShareMusics();
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (open && s.phase === 'unknown' && isTauri()) void refreshAccount();
  }, [open, s.phase]);
  if (!isTauri() || s.phase === 'unavailable') return null;
  return (
    <section className="junction-share-musics" aria-labelledby="junction-share-musics-account">
      <div className="junction-share-musics-head">
        <h3 id="junction-share-musics-account">PlumDeck Liteアカウント</h3>
        {s.phase === 'signed_in' && <span className="junction-state junction-state-ready">ログイン中</span>}
      </div>
      {s.phase === 'signed_in' ? (
        <div className="junction-share-musics-row">
          <p className="junction-card-note">{s.email ?? 'ログイン済み'} · 登録メンバー同士は招待・返答を自動で受け渡します。</p>
          <button type="button" className="junction-btn junction-btn-quiet" disabled={busy} onClick={() => { setBusy(true); void signOut().finally(() => setBusy(false)); }}>ログアウト</button>
        </div>
      ) : s.phase === 'signing_in' ? (
        <div className="junction-share-musics-row">
          <p className="junction-card-note" role="status">ブラウザでGoogleログインを続けてください。完了するとここに反映されます。</p>
          <button type="button" className="junction-btn junction-btn-default" onClick={cancelSignIn}>ログインを中止</button>
        </div>
      ) : (
        <>
          <p className="junction-card-note">PlumDeck Liteに登録済みのGoogleアカウントでログインすると、登録メンバー同士は招待・返答をコピーせずに接続できます。登録していない相手とは、これまでどおり招待・返答で接続します。</p>
          <button type="button" className="junction-btn junction-btn-default" disabled={s.phase === 'unknown' && open} onClick={() => void signIn()}>Googleでログイン</button>
        </>
      )}
      {s.accountError && <p className="junction-card-error" role="alert">{s.accountError}</p>}
    </section>
  );
}

interface LobbyProps {
  profile: ShareMusicsProfile;
  disabled: boolean;
}

/** Guest: sessions on share-musics (phones and desktops alike), and this DJ's pending request. */
export function ShareMusicsLobby({profile, disabled}: LobbyProps) {
  const s = useShareMusics();
  const [busySession, setBusySession] = useState('');
  const [error, setError] = useState('');
  const waiting = s.link?.role === 'guest' && !s.link.joined ? s.link : null;
  const signedIn = s.phase === 'signed_in';

  useEffect(() => {
    if (!signedIn || waiting) return;
    void refreshSessions();
    const timer = setInterval(() => void refreshSessions(), SESSION_REFRESH_MS);
    return () => clearInterval(timer);
  }, [signedIn, waiting]);

  if (!signedIn) return null;

  if (waiting) {
    const status = s.view?.me.status;
    return (
      <section className="junction-share-musics junction-share-musics-waiting" aria-live="polite">
        <strong>{waiting.sessionName}</strong>
        <small>管理DJ：{waiting.hostName}{waiting.hostClient === 'lite' ? '（スマホ）' : ''}</small>
        <p className="junction-exchange-next">
          {status === 'approved' ? '参加が許可されました。自動で接続しています。' : '参加を申請しました。管理DJの許可を待っています。'}
        </p>
        {(s.error || s.pollError) && <p className="junction-card-error" role="alert">{s.error || s.pollError}</p>}
        <button type="button" className="junction-btn junction-btn-default" onClick={() => void endLink()}>申請を取り消す</button>
      </section>
    );
  }

  const sessions = s.sessions.filter((session) => !session.mine);
  return (
    <section className="junction-share-musics" aria-labelledby="junction-share-musics-sessions">
      <div className="junction-share-musics-head">
        <h3 id="junction-share-musics-sessions">メンバーのセッション</h3>
      </div>
      {s.notice && (
        <p className="junction-card-note" role="status">
          {s.notice} <button type="button" className="junction-btn junction-btn-quiet" onClick={dismissNotice}>閉じる</button>
        </p>
      )}
      {sessions.length === 0 ? (
        <p className="junction-card-note">参加できるセッションはありません。PlumDeck Liteのメンバーがスマホかデスクトップで作成すると表示されます。</p>
      ) : (
        <ul className="junction-share-musics-list">
          {sessions.map((session) => {
            // A phone-hosted session speaks the Lite protocol, which this desktop joins once supported.
            const unsupported = session.hostClient === 'lite' && !LITE_PEERS_READY;
            return (
              <li key={session.id}>
                <div>
                  <strong>{session.name}</strong>
                  <small>管理DJ：{session.hostName}（{session.hostClient === 'lite' ? 'スマホ' : 'デスクトップ'}） · {session.participantCount}人</small>
                  {unsupported && <small>スマホのセッションへのデスクトップからの参加は準備中です</small>}
                </div>
                <button
                  type="button"
                  className="junction-btn junction-btn-primary"
                  disabled={disabled || unsupported || Boolean(busySession) || !profile.djName.trim()}
                  onClick={() => {
                    setBusySession(session.id);
                    setError('');
                    void requestJoin(session, profile).catch((cause) => setError(errorText(cause))).finally(() => setBusySession(''));
                  }}
                >
                  参加を申請
                </button>
              </li>
            );
          })}
        </ul>
      )}
      {!profile.djName.trim() && sessions.length > 0 && <p className="junction-card-note">申請する前にDJ名を入力してください。</p>}
      {(error || s.sessionsError) && <p className="junction-card-error" role="alert">{error || s.sessionsError}</p>}
    </section>
  );
}

interface HostProps {
  snapshot: JunctionSnapshot;
  djName: string;
}

/** Host: publish the session and decide who may join; connection then follows automatically. */
export function ShareMusicsHostSection({snapshot, djName}: HostProps) {
  const s = useShareMusics();
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  if (s.phase !== 'signed_in') return null;
  const link = s.link?.role === 'host' && s.link.nativeSessionId === snapshot.sessionId ? s.link : null;
  const run = (key: string, fn: () => Promise<unknown>) => {
    setBusy(key);
    setError('');
    void fn().catch((cause) => setError(errorText(cause))).finally(() => setBusy(''));
  };

  if (!link) {
    return (
      <section className="junction-share-musics">
        <div className="junction-share-musics-head"><h3>PlumDeck Liteのメンバーに公開</h3></div>
        <p className="junction-card-note">公開するとPlumDeck Liteのセッション一覧（スマホ・デスクトップ共通）に表示され、参加申請を許可するだけで接続します。</p>
        <button type="button" className="junction-btn junction-btn-default" disabled={Boolean(busy)}
          onClick={() => run('publish', () => publishSession(snapshot.sessionName?.trim() || `${djName} のセッション`, djName))}>
          公開する
        </button>
        {(error || s.error) && <p className="junction-card-error" role="alert">{error || s.error}</p>}
      </section>
    );
  }

  const members = (s.view?.session.members ?? []).filter((member) => member.role === 'guest' && (member.status === 'pending' || member.status === 'approved'));
  return (
    <section className="junction-share-musics" aria-labelledby="junction-share-musics-requests">
      <div className="junction-share-musics-head">
        <h3 id="junction-share-musics-requests">PlumDeck Liteの参加申請</h3>
        <span className="junction-state junction-state-ready">公開中</span>
      </div>
      {members.length === 0 ? (
        <p className="junction-card-note">メンバーからの申請を待っています。許可した相手とは自動で接続します。</p>
      ) : (
        <ul className="junction-share-musics-list">
          {members.map((member) => {
            const liteWaiting = member.client === 'lite' && !LITE_PEERS_READY;
            return (
              <li key={member.peerId}>
                <div>
                  <strong>{member.displayName}</strong>
                  <small>{member.client === 'lite' ? 'スマホ（PlumDeck Lite）' : 'デスクトップ'}</small>
                  {member.status === 'approved' && <small>{memberProgress(member, snapshot, link.host)}</small>}
                  {liteWaiting && member.status === 'pending' && <small>スマホとの接続は準備中のため、まだ許可できません</small>}
                </div>
                {member.status === 'pending' ? (
                  <div className="junction-card-actions">
                    <button type="button" className="junction-btn junction-btn-primary" disabled={Boolean(busy) || liteWaiting} onClick={() => run(member.peerId, () => decideRequest(member.peerId, true))}>参加を許可</button>
                    <button type="button" className="junction-btn junction-btn-danger" disabled={Boolean(busy)} onClick={() => run(member.peerId, () => decideRequest(member.peerId, false))}>許可しない</button>
                  </div>
                ) : link.host.failed[member.peerId] ? (
                  <button type="button" className="junction-btn junction-btn-default" onClick={() => retryRequest(member.peerId)}>もう一度招待する</button>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
      {(error || s.error || s.pollError) && <p className="junction-card-error" role="alert">{error || s.error || s.pollError}</p>}
      <button type="button" className="junction-btn junction-btn-quiet" disabled={Boolean(busy)} onClick={() => run('unpublish', () => endLink())}>
        公開を終了（接続中のDJはそのまま）
      </button>
    </section>
  );
}

function memberProgress(member: SharedMember, snapshot: JunctionSnapshot, memory: HostMemory): string {
  if (member.client === 'lite') {
    const participant = snapshot.participants.find((item) => item.peerId === member.peerId);
    return participant?.status === 'connected' ? 'スマホと接続しました' : memory.liteOffer[member.peerId] ? 'スマホからの返答を待っています' : '接続情報を準備しています';
  }
  if (memory.failed[member.peerId]) return `招待を作れませんでした：${memory.failed[member.peerId]}`;
  const peerId = memory.peerByMember[member.peerId];
  const state = snapshot.participants.find((participant) => participant.peerId === peerId)?.exchange?.state;
  if (state === 'invite_ready' || state === 'awaiting_answer') return '招待を届けました · 返答を待っています';
  if (state === 'approval_pending') return '返答が届きました · 接続を許可しています';
  return state ? describeExchangeState(state) : '招待を準備しています';
}
