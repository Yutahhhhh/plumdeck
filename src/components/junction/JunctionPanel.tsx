import { useEffect, useMemo, useRef, useState } from 'react';
import { open as openAudioDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
import type { AudioDevice } from '@/types/dj-engine';
import type { ExchangeInspection, JunctionOp, JunctionSnapshot } from '@/types/junction';
import { deriveGuestGuidance, type ExchangeActionId } from '@/services/junction/exchange-actions';
import { ExchangeFlow } from './ExchangeFlow';
import { useJunction } from '@/hooks/useJunction';
import { djEngineClient } from '@/services/dj-engine/client';
import {
  copyExchangeText,
  junctionCommand,
  junctionInspectExchange,
  readExchangeClipboard,
} from '@/services/junction/client';
import { DJ_NAME_MAX_LENGTH, limitDjName, participantName, profileAvatarValue, stableThemeColor } from '@/services/junction/roster-model';
import { DjProfileEditor, type DjProfileValue } from './DjProfileEditor';
import { JunctionRoster } from './JunctionRoster';
import { NetworkSettingsSection } from './NetworkSettingsSection';
import { ShareMusicsAccountCard, ShareMusicsHostSection, ShareMusicsLobby } from './ShareMusicsJunction';
import { useShareMusics } from '@/hooks/useShareMusics';
import { automaticPeerIds, publishSession, reportShareMusicsError } from '@/services/junction/share-musics/coordinator';
import './junction.css';

const PROFILE_KEY = 'plumdeck.junction.profile';

interface CardState { busy: boolean; error: string; copiedPacket?: string }
type CardMap = Record<string, CardState>;

interface Props {
  open: boolean;
  onClose: () => void;
  incomingInvite: string | null;
  onConsumeIncoming: () => void;
  notice?: string;
  onDismissNotice?: () => void;
}

export function JunctionPanel({open, onClose, incomingInvite, onConsumeIncoming, notice, onDismissNotice}: Props) {
  const snapshot = useJunction();
  const headingRef = useRef<HTMLHeadingElement | null>(null);
  const active = Boolean(snapshot?.active);
  const host = active && snapshot?.hostPeerId === snapshot?.localPeerId;
  const serverMode = snapshot?.exchange?.mode === 'server' || Boolean(snapshot?.invite && !snapshot?.exchange);
  const shareMusics = useShareMusics();
  const automaticPeers = useMemo(() => automaticPeerIds(shareMusics, snapshot), [shareMusics, snapshot]);
  const guestAutomatic = Boolean(snapshot && !host && automaticPeers.has(snapshot.hostPeerId));

  const [profile, setProfile] = useState<DjProfileValue>(readProfile);
  const [sessionName, setSessionName] = useState('');
  const [joinText, setJoinText] = useState('');
  const [preview, setPreview] = useState<ExchangeInspection | null>(null);
  const [programDevice, setProgramDevice] = useState('');
  const [adoptCurrent, setAdoptCurrent] = useState(false);
  const [publishToMembers, setPublishToMembers] = useState(true);
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [privatePath, setPrivatePath] = useState('');
  const [position, setPosition] = useState(0);
  const [entryBusy, setEntryBusy] = useState(false);
  const [entryError, setEntryError] = useState('');
  const [inspecting, setInspecting] = useState(false);
  const [cards, setCards] = useState<CardMap>({});
  const [showInvite, setShowInvite] = useState(false);
  const [inviteDjName, setInviteDjName] = useState('');

  useEffect(() => {
    localStorage.setItem(PROFILE_KEY, JSON.stringify({
      ...profile,
      avatarDataUrl: profileAvatarValue(profile.avatarDataUrl),
    }));
    localStorage.setItem('plumdeck.junction.name', profile.djName);
  }, [profile]);

  useEffect(() => {
    if (!open) return;
    void djEngineClient.listAudioDevices()
      .then((data) => setDevices((data as {devices?: AudioDevice[]}).devices ?? []))
      .catch(() => {});
  }, [open, active]);

  useEffect(() => {
    if (open) headingRef.current?.focus();
  }, [open]);

  useEffect(() => {
    if (incomingInvite && !active) {
      setJoinText(incomingInvite);
      setPreview(null);
      onConsumeIncoming();
    }
  }, [incomingInvite, active, onConsumeIncoming]);

  useEffect(() => {
    if (!snapshot?.active) return;
    if (hasProgramDevice(snapshot) && !programDevice) setProgramDevice(snapshot.program.outputDevice!);
  }, [snapshot?.active, snapshot?.program.outputDevice, programDevice]);

  const outputOptions = devices.filter((device) => device.outputChannels >= 2);
  const deviceValue = (id: string) => (/^(coreaudio|portaudio):\d+$/.test(id) ? id.split(':')[1] : id);

  const setCard = (key: string, patch: Partial<CardState>) => {
    setCards((previous) => ({
      ...previous,
      [key]: {...(previous[key] ?? {busy: false, error: ''}), ...patch},
    }));
  };

  const runCard = async (key: string, fn: () => Promise<unknown>, rethrow = false): Promise<void> => {
    setCard(key, {busy: true, error: ''});
    try {
      await fn();
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setCard(key, {error: message});
      if (rethrow) throw cause;
    } finally {
      setCard(key, {busy: false});
    }
  };

  const runEntry = async (op: JunctionOp, params: Record<string, unknown>): Promise<boolean> => {
    setEntryBusy(true);
    setEntryError('');
    try {
      await junctionCommand(op, params);
      setJoinText('');
      setPreview(null);
      return true;
    } catch (cause) {
      setEntryError(cause instanceof Error ? cause.message : String(cause));
      return false;
    } finally {
      setEntryBusy(false);
    }
  };

  const createSession = async () => {
    const created = await runEntry('create', {
      displayName: profile.djName,
      djName: profile.djName,
      avatarDataUrl: profile.avatarDataUrl,
      themeColor: profile.themeColor,
      sessionName,
      programDevice,
      adoptCurrent,
      exchangeMode: 'manual',
      startInLobby: true,
    });
    if (created && publishToMembers && shareMusics.phase === 'signed_in') {
      await publishSession(sessionName.trim(), profile.djName)
        .catch((cause) => reportShareMusicsError(`PlumDeck Liteへの公開に失敗しました：${cause instanceof Error ? cause.message : String(cause)}`));
    }
  };

  const inspectInvite = async () => {
    setInspecting(true);
    setEntryError('');
    setPreview(null);
    try {
      setPreview(await junctionInspectExchange(joinText));
    } catch (cause) {
      setEntryError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setInspecting(false);
    }
  };

  const selfExchange = useMemo(
    () => snapshot?.participants.find((participant) => participant.peerId === snapshot.hostPeerId)?.exchange,
    [snapshot],
  );

  const handleExchangeAction = (peerId: string, action: ExchangeActionId) => {
    const participant = snapshot?.participants.find((item) => item.peerId === peerId);
    const exchange = participant?.exchange ?? (!host ? selfExchange : undefined);
    const exchangeText = exchange?.inviteText ?? snapshot?.exchange?.responseText ?? snapshot?.invite ?? '';
    const noticeText = exchange?.noticeText ?? '';
    switch (action) {
      case 'open_relay': {
        const details = document.getElementById('junction-network-settings') as HTMLDetailsElement | null;
        if (details) {
          details.open = true;
          details.scrollIntoView({block: 'start', behavior: 'smooth'});
          details.querySelector('summary')?.focus();
        }
        return;
      }
      case 'copy_invite':
      case 'copy_answer':
        void runCard(peerId, async () => {
          await copyExchangeText(exchangeText);
          setCard(peerId, {copiedPacket: exchangeText});
        });
        return;
      case 'copy_notice':
        void runCard(peerId, () => copyExchangeText(noticeText));
        return;
      case 'approve':
        void runCard(peerId, () => junctionCommand('peer.approve', {peerId, accept: true}));
        return;
      case 'reject':
        void runCard(peerId, () => junctionCommand('peer.approve', {peerId, accept: false}));
        return;
      case 'reexchange':
        void runCard(peerId, () => junctionCommand('invite.create', {peerId}));
        return;
      case 'cancel':
        void runCard(peerId, () => junctionCommand('invite.cancel', {peerId}));
        return;
    }
  };

  const cardErrors = Object.fromEntries(Object.entries(cards).map(([key, value]) => [key, value.error]));
  const copiedPackets = Object.fromEntries(Object.entries(cards).map(([key, value]) => [key, value.copiedPacket]));
  const busyKey = Object.entries(cards).find(([, value]) => value.busy)?.[0];

  const onPanelKeyDown = (event: React.KeyboardEvent) => {
    event.stopPropagation();
    if (event.key === 'Escape') onClose();
  };

  return (
    <div
      id="junction-panel"
      className={`junction-panel${open ? ' junction-panel-open' : ''}`}
      role="complementary"
      aria-label="Junction セッション"
      aria-hidden={!open}
      inert={!open}
      onKeyDown={onPanelKeyDown}
      onKeyUp={(event) => event.stopPropagation()}
    >
      <div className="junction-panel-head">
        <h2 tabIndex={-1} ref={headingRef}>Junction</h2>
        <button type="button" className="junction-btn junction-btn-default" onClick={onClose} aria-label="パネルを閉じる">閉じる</button>
      </div>

      <div className="junction-panel-body">
        {notice && (
          <p className="junction-card-error" role="alert">
            {notice}
            {onDismissNotice && <button type="button" className="junction-btn junction-btn-default" onClick={onDismissNotice}>閉じる</button>}
          </p>
        )}

        {!active ? (
          <EntrySection
            profile={profile}
            setProfile={setProfile}
            sessionName={sessionName}
            setSessionName={setSessionName}
            joinText={joinText}
            setJoinText={(value) => { setJoinText(value); setPreview(null); }}
            outputOptions={outputOptions}
            deviceValue={deviceValue}
            programDevice={programDevice}
            setProgramDevice={setProgramDevice}
            adoptCurrent={adoptCurrent}
            setAdoptCurrent={setAdoptCurrent}
            busy={entryBusy}
            inspecting={inspecting}
            error={entryError}
            preview={preview}
            onInspect={() => void inspectInvite()}
            panelOpen={open}
            signedIn={shareMusics.phase === 'signed_in'}
            guestWaiting={shareMusics.link?.role === 'guest' && !shareMusics.link.joined}
            publishToMembers={publishToMembers}
            setPublishToMembers={setPublishToMembers}
            onCreate={() => void createSession()}
            onJoin={() => void runEntry('join', {
              displayName: profile.djName,
              djName: profile.djName,
              avatarDataUrl: profile.avatarDataUrl,
              themeColor: profile.themeColor,
              text: joinText.trim(),
            })}
            onInputError={setEntryError}
          />
        ) : snapshot && (
          <>
            <section className="junction-session-summary">
              <div className="junction-session-title">
                <span className={`junction-session-dot junction-session-${snapshot.lifecycle ?? 'live'}`} aria-hidden="true" />
                <div>
                  <strong>{snapshot.sessionName?.trim() || '名称未設定のセッション'}</strong>
                  <small>{sessionStatus(snapshot)}</small>
                </div>
              </div>
              {host && (
                <button type="button" className="junction-btn junction-btn-primary" onClick={() => setShowInvite((value) => !value)} aria-expanded={showInvite}>
                  DJを招待
                </button>
              )}
            </section>

            {host && showInvite && (
              <section className="junction-invite-composer">
                <div>
                  <h3>DJを招待</h3>
                  <p>招待を相手へ送り、届いた返答を確認すると接続できます。</p>
                </div>
                <label>
                  招待するDJ名
                  <input value={inviteDjName} onChange={(event) => setInviteDjName(limitDjName(event.target.value))} maxLength={DJ_NAME_MAX_LENGTH} placeholder="未定でも作成できます" />
                </label>
                <button
                  type="button"
                  className="junction-btn junction-btn-primary"
                  disabled={cards.invite?.busy}
                  onClick={() => void runCard('invite', async () => {
                    await junctionCommand('invite.create', {
                      djName: inviteDjName.trim() || undefined,
                      themeColor: stableThemeColor(inviteDjName || 'DJ'),
                    });
                    setInviteDjName('');
                    setShowInvite(false);
                  })}
                >
                  招待を作る
                </button>
                {cards.invite?.error && <p className="junction-card-error" role="alert">{cards.invite.error}</p>}
              </section>
            )}

            {snapshot.lifecycle === 'lobby' && host && (
              <p className="junction-lobby-guide">
                DJが揃ったら、一覧から最初にプレイするDJを選んでください。
                {!hasProgramDevice(snapshot) && <> 開始前に「セッション設定」の「会場への音声出力」で出力先を反映してください。</>}
              </p>
            )}

            {host && <ShareMusicsHostSection snapshot={snapshot} djName={profile.djName} />}

            {!host && !serverMode && <section className="junction-own-connection" aria-labelledby="junction-own-connection-title">
              <h3 id="junction-own-connection-title">あなたの接続 · 参加DJ</h3>
              <p className="junction-card-note">管理DJ：{participantName(snapshot.participants.find((p) => p.peerId === snapshot.hostPeerId) ?? {peerId: snapshot.hostPeerId, displayName: '管理DJ', approved: true})}</p>
              {guestAutomatic && <p className="junction-card-note">PlumDeck Lite経由で招待・返答を自動で受け渡しています（{shareMusics.link?.sessionName}）。</p>}
              {guestAutomatic && (shareMusics.error || shareMusics.pollError) && <p className="junction-card-error" role="alert">{shareMusics.error || shareMusics.pollError}</p>}
              {selfExchange?.state === 'connected' && <p className="junction-exchange-next">セッションに接続済みです。</p>}
              <ExchangeFlow
                key={`${snapshot.sessionId}:${selfExchange?.inviteId ?? snapshot.exchange?.inviteId ?? ''}`}
                host={false} connected={selfExchange?.state === 'connected'} automatic={guestAutomatic}
                guidance={deriveGuestGuidance(snapshot, Boolean(copiedPackets[snapshot.hostPeerId] && copiedPackets[snapshot.hostPeerId] === snapshot.exchange?.responseText), guestAutomatic)}
                busy={Boolean(cards[snapshot.hostPeerId]?.busy)} error={cardErrors[snapshot.hostPeerId]}
                onAction={(action) => handleExchangeAction(snapshot.hostPeerId, action)}
                onImport={(text) => runCard(snapshot.hostPeerId, () => junctionCommand('exchange.import', {text: text.trim()}), true)}
              />
            </section>}

            <JunctionRoster
              snapshot={snapshot}
              host={host}
              busyKey={busyKey}
              errors={cardErrors}
              copiedPackets={copiedPackets}
              automaticPeers={automaticPeers}
              onExchangeAction={handleExchangeAction}
              onImportText={(peerId, text) => runCard(peerId, () => junctionCommand('exchange.import', {text: text.trim(), peerId}), true)}
              onChooseParticipant={(peerId, first) => void runCard('handoff', () => junctionCommand(first ? 'session.start' : 'handoff.request', first ? {performerPeerId: peerId} : {targetPeerId: peerId}))}
              onAcceptHandoff={() => void runCard('handoff', () => junctionCommand('handoff.accept'))}
              onCancelHandoff={() => void runCard('handoff', () => junctionCommand('handoff.cancel'))}
              onRequestTurn={() => void runCard('handoff', () => junctionCommand('handoff.request', {targetPeerId: snapshot.localPeerId}))}
              onReorder={(peerIds) => runCard('roster', () => junctionCommand('roster.reorder', {peerIds}), true)}
            />

            {djEngineClient.getState().snapshot?.audio.microphone?.enabled && (
              <button
                type="button"
                className="junction-btn junction-btn-default junction-full-button"
                disabled={cards.microphone?.busy}
                onClick={() => void runCard('microphone', async () => {
                  await djEngineClient.setMicrophone({enabled: false});
                  await junctionCommand('snapshot');
                })}
              >
                マイクを閉じて引き継ぎに備える
              </button>
            )}

            <details className="junction-section junction-settings">
              <summary>セッション設定</summary>
              <div className="junction-settings-body">
                <ShareMusicsAccountCard open={open} />

                <h3>あなたのプロフィール</h3>
                <DjProfileEditor value={profile} onChange={setProfile} compact />
                <button
                  type="button"
                  className="junction-btn junction-btn-default"
                  disabled={cards.profile?.busy || !profile.djName.trim()}
                  onClick={() => void runCard('profile', () => junctionCommand('profile.update', {
                    displayName: profile.djName,
                    djName: profile.djName,
                    avatarDataUrl: profile.avatarDataUrl,
                    themeColor: profile.themeColor,
                  }))}
                >
                  プロフィールを反映
                </button>
                {cards.profile?.error && <p className="junction-card-error" role="alert">{cards.profile.error}</p>}

                {host && (
                  <>
                    <h3>会場への音声出力</h3>
                    <p className="junction-card-note">状態：{programStatus(snapshot.program.state)}</p>
                    <label>
                      出力デバイス
                      <select value={programDevice} onChange={(event) => setProgramDevice(event.target.value)}>
                        <option value="">出力先を選択</option>
                        {outputOptions.map((device) => <option key={device.id} value={deviceValue(device.id)}>{device.name}</option>)}
                      </select>
                    </label>
                    <div className="junction-card-actions">
                      <button type="button" className="junction-btn junction-btn-primary" disabled={cards.program?.busy || !programDevice} onClick={() => void runCard('program', () => junctionCommand('program.configure', {programDevice}))}>出力先を反映</button>
                      <button
                        type="button"
                        className="junction-btn junction-btn-default"
                        disabled={cards.program?.busy}
                        onClick={() => snapshot.program.recording
                          ? void runCard('program', () => junctionCommand('program.record.stop'))
                          : void saveDialog({defaultPath: 'Junction.wav', filters: [{name: 'WAV', extensions: ['wav']}]})
                              .then((path) => { if (path) void runCard('program', () => junctionCommand('program.record.start', {path})); })
                              .catch(() => setCard('program', {error: '保存先を選択できませんでした'}))}
                      >
                        {snapshot.program.recording ? '録音を停止' : 'セッションを録音'}
                      </button>
                    </div>
                    {cards.program?.error && <p className="junction-card-error" role="alert">{cards.program.error}</p>}
                  </>
                )}
              </div>
            </details>

            <details className="junction-section junction-settings">
              <summary>手元だけで試聴する</summary>
              <div className="junction-settings-body">
                <p className="junction-card-note">セッションの再生内容を変えず、ヘッドホンCUEで確認します。</p>
                <button type="button" className="junction-btn junction-btn-default" onClick={() => void openAudioDialog({multiple: false, filters: [{name: '音源', extensions: ['mp3', 'wav', 'flac', 'aiff', 'm4a', 'ogg']}]})
                  .then((path) => { if (typeof path === 'string') setPrivatePath(path); })
                  .catch(() => setCard('private', {error: '音源を選択できませんでした'}))}>音源を選択</button>
                <p className="junction-card-note">{privatePath.split(/[\\/]/).pop() || '音源が未選択です'}</p>
                <div className="junction-card-actions">
                  <button type="button" className="junction-btn junction-btn-default" disabled={cards.private?.busy || !privatePath} onClick={() => void runCard('private', () => junctionCommand('private.load', {path: privatePath}))}>試聴へロード</button>
                  <button type="button" className="junction-btn junction-btn-default" disabled={cards.private?.busy} onClick={() => void runCard('private', () => junctionCommand('private.play'))}>再生</button>
                  <button type="button" className="junction-btn junction-btn-default" disabled={cards.private?.busy} onClick={() => void runCard('private', () => junctionCommand('private.pause'))}>停止</button>
                </div>
                <label>CUE位置（秒）<input type="number" min={0} value={position} onChange={(event) => setPosition(Number(event.target.value))} /></label>
                <button type="button" className="junction-btn junction-btn-default" disabled={cards.private?.busy} onClick={() => void runCard('private', () => junctionCommand('private.seek', {positionMs: position * 1000}))}>位置を合わせる</button>
                {cards.private?.error && <p className="junction-card-error" role="alert">{cards.private.error}</p>}
              </div>
            </details>

            <details className="junction-section junction-settings">
              <summary>トラブル対応・高度な情報</summary>
              <div className="junction-settings-body">
                <p className="junction-card-note">接続状態：{connectionStatus(snapshot.connection.state)}</p>
                <p className="junction-card-note">引き継ぎ：{snapshot.handoffState}</p>
                <p className="junction-card-note">接続方法：{serverMode ? '自動接続' : automaticPeers.size > 0 ? '招待用の文字を直接受け渡し（PlumDeck Liteのメンバーとは自動で受け渡し）' : '招待用の文字を直接受け渡し'}</p>
                {host && snapshot.handoffState === 'recovery' && (
                  <button type="button" className="junction-btn junction-btn-primary" disabled={cards.recovery?.busy} onClick={() => void runCard('recovery', () => junctionCommand('recovery.resume'))}>このPCの演奏で再開</button>
                )}
              </div>
            </details>

            <NetworkSettingsSection />

            <button type="button" className="junction-btn junction-btn-danger junction-session-exit" disabled={cards.end?.busy} onClick={() => void runCard('end', () => junctionCommand(host ? 'end' : 'leave'))}>
              {host ? 'セッションを終了' : 'セッションから退出'}
            </button>
            {cards.end?.error && <p className="junction-card-error" role="alert">{cards.end.error}</p>}
          </>
        )}

        {!active && <NetworkSettingsSection />}
      </div>
    </div>
  );
}

interface EntryProps {
  profile: DjProfileValue;
  setProfile: (profile: DjProfileValue) => void;
  sessionName: string;
  setSessionName: (value: string) => void;
  joinText: string;
  setJoinText: (value: string) => void;
  outputOptions: AudioDevice[];
  deviceValue: (id: string) => string;
  programDevice: string;
  setProgramDevice: (value: string) => void;
  adoptCurrent: boolean;
  setAdoptCurrent: (value: boolean) => void;
  busy: boolean;
  inspecting: boolean;
  error: string;
  preview: ExchangeInspection | null;
  onInspect: () => void;
  panelOpen: boolean;
  signedIn: boolean;
  guestWaiting: boolean;
  publishToMembers: boolean;
  setPublishToMembers: (value: boolean) => void;
  onCreate: () => void;
  onJoin: () => void;
  onInputError: (message: string) => void;
}

function EntrySection(props: EntryProps) {
  const [choice, setChoice] = useState<'create' | 'join' | null>(null);
  useEffect(() => { if (props.joinText || props.guestWaiting) setChoice('join'); }, [props.joinText, props.guestWaiting]);
  return (
    <div className="junction-entry">
      <p className="junction-entry-lead">DJ同士をつないで、順番と演奏を共有します。</p>
      <ShareMusicsAccountCard open={props.panelOpen} />
      <div className="junction-entry-actions">
        <button type="button" className={`junction-entry-choice${choice === 'create' ? ' is-selected' : ''}`} aria-pressed={choice === 'create'} onClick={() => setChoice('create')}>
          <strong>セッションを作成</strong>
          <span>このPCでDJを招待・管理する</span>
        </button>
        <button type="button" className={`junction-entry-choice${choice === 'join' ? ' is-selected' : ''}`} aria-pressed={choice === 'join'} onClick={() => setChoice('join')}>
          <strong>招待から参加</strong>
          <span>{props.signedIn ? 'メンバーのセッション・受け取った文字から参加する' : '受け取った文字から参加する'}</span>
        </button>
      </div>

      {choice && <DjProfileEditor value={props.profile} onChange={props.setProfile} disabled={props.busy} />}

      {choice === 'create' && (
        <section className="junction-entry-form">
          <label>
            セッション名
            <input value={props.sessionName} onChange={(event) => props.setSessionName(event.target.value)} maxLength={80} placeholder="例：Friday Session" />
          </label>
          <label>
            会場へ音を出すデバイス
            <select value={props.programDevice} onChange={(event) => props.setProgramDevice(event.target.value)}>
              <option value="">あとで選択</option>
              {props.outputOptions.map((device) => <option key={device.id} value={props.deviceValue(device.id)}>{device.name}</option>)}
            </select>
          </label>
          <label className="junction-check">
            <input type="checkbox" checked={props.adoptCurrent} onChange={(event) => props.setAdoptCurrent(event.target.checked)} />
            すでに再生中の音をセッションに含める
          </label>
          {props.signedIn && (
            <label className="junction-check">
              <input type="checkbox" checked={props.publishToMembers} onChange={(event) => props.setPublishToMembers(event.target.checked)} />
              PlumDeck Liteのメンバーに公開する（申請を許可すると自動で接続）
            </label>
          )}
          <p className="junction-card-note">作成後にDJを招待し、最初にプレイするDJを一覧から選びます。</p>
          <button type="button" className="junction-btn junction-btn-primary junction-full-button" disabled={props.busy || !props.profile.djName.trim() || !props.sessionName.trim()} onClick={props.onCreate}>
            セッションを作成
          </button>
        </section>
      )}

      {choice === 'join' && <ShareMusicsLobby profile={props.profile} disabled={props.busy} />}

      {choice === 'join' && !props.guestWaiting && (
        <section className="junction-entry-form">
          {props.signedIn && <h3 className="junction-entry-subhead">招待用の文字から参加</h3>}
          <p className="junction-card-note">招待・返答の文字はサーバーを経由しません。管理DJから届いた招待用の文字をそのまま貼り付けます。</p>
          <label>
            招待用の文字
            <textarea value={props.joinText} onChange={(event) => props.setJoinText(event.target.value)} rows={5} maxLength={131072} placeholder="PLUMDECK-JUNCTION-…" />
          </label>
          <div className="junction-card-actions">
            <button type="button" className="junction-btn junction-btn-primary" disabled={props.inspecting || !props.joinText.trim()} onClick={props.onInspect}>招待を確認</button>
            <button type="button" className="junction-btn junction-btn-default" disabled={props.inspecting} onClick={() => void readExchangeClipboard().then(props.setJoinText).catch((cause) => props.onInputError(String(cause)))}>貼り付け</button>
          </div>
          {props.preview && (
            <div className="junction-invite-preview">
              <span className="junction-state junction-state-ready">確認済み</span>
              <strong>{props.preview.sessionName || '名称未設定のセッション'}</strong>
              {props.preview.hostName && <small>管理DJ：{props.preview.hostName}</small>}
              <button type="button" className="junction-btn junction-btn-primary junction-full-button" disabled={props.busy || !props.profile.djName.trim() || props.preview.kind !== 'invite'} onClick={props.onJoin}>このセッションに参加</button>
            </div>
          )}
        </section>
      )}
      {props.error && <p className="junction-card-error" role="alert">{props.error}</p>}
    </div>
  );
}

function readProfile(): DjProfileValue {
  const legacyName = localStorage.getItem('plumdeck.junction.name') ?? '';
  try {
    const saved = JSON.parse(localStorage.getItem(PROFILE_KEY) ?? '{}') as Partial<DjProfileValue>;
    const djName = limitDjName(typeof saved.djName === 'string' ? saved.djName : legacyName);
    return {
      djName,
      avatarDataUrl: profileAvatarValue(saved.avatarDataUrl),
      themeColor: /^#[0-9a-f]{6}$/i.test(saved.themeColor ?? '') ? saved.themeColor! : stableThemeColor(djName),
    };
  } catch {
    const djName = limitDjName(legacyName);
    return {djName, themeColor: stableThemeColor(djName)};
  }
}

function sessionStatus(snapshot: JunctionSnapshot): string {
  if (snapshot.lifecycle === 'lobby') return `${snapshot.participants.length}人 · 準備中`;
  if (snapshot.lifecycle === 'starting') return '演奏を開始しています';
  const performer = snapshot.participants.find((participant) => participant.peerId === snapshot.performerPeerId);
  return performer ? `${participantName(performer)}が演奏中` : `${snapshot.participants.length}人が参加`;
}

/** Native reports an unset venue output as "-1". */
function hasProgramDevice(snapshot: JunctionSnapshot): boolean {
  const device = snapshot.program.outputDevice;
  return Boolean(device) && device !== '-1';
}

function programStatus(state: string): string {
  if (state === 'running') return '出力中';
  if (state === 'error') return '確認が必要';
  return '準備中';
}

function connectionStatus(state: string): string {
  const labels: Record<string, string> = {
    stable: '安定', connected: '接続済み', connecting: '接続中', interrupted: '接続が中断',
    reconnecting: '再接続中', disconnected: '未接続', error: '確認が必要', unknown: '確認中',
  };
  return labels[state] ?? '確認中';
}
