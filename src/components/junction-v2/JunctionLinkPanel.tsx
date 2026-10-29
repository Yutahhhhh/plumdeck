import { useEffect, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { djEngineClient } from "@/services/dj-engine/client";
import { junctionLink, PROGRAM_DELAY_SECONDS, type LinkSnapshot } from "@/services/junction-v2/session";
import { shareMusicsApi } from "@/services/junction/share-musics/client";
import type { SharedSessionSummary } from "@/services/junction/share-musics/automation";
import { ShareMusicsAccountCard } from "@/components/junction/ShareMusicsJunction";
import { useShareMusics } from "@/hooks/useShareMusics";
import { roleOf } from "@/services/junction-v2/core/turns";
import { ROLE_LABEL, nameOf, turnView, useJunctionLink } from "./useJunctionLink";
import "@/components/junction/junction.css";
import "./junction-link.css";

const NAME_KEY = "plumdeck.junction2.djName";
const SPLICE_LABEL: Record<string, string> = {
  started: "開始しました",
  aligned: "つなぎ目を合わせて交代しました",
  unaligned: "前のDJの音が次のDJのミックスに無かったため、そのまま交代しました",
  late: "回線の遅れが大きく、前のDJの音が少し重なりました",
  same: "",
};

const run = (task: () => unknown) => { void Promise.resolve().then(task).catch(() => undefined); };

function CopyField({ label, value }: { label: string; value: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="jl-copy">
      <span>{label}</span>
      <textarea readOnly value={value} rows={3} onFocus={(e) => e.currentTarget.select()} />
      <button type="button" className="junction-btn" onClick={() => void navigator.clipboard.writeText(value).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1500); })}>
        {copied ? "コピーしました" : "コピー"}
      </button>
    </div>
  );
}

function Entry() {
  const account = useShareMusics();
  const signedIn = account.phase === "signed_in";
  const [djName, setDjName] = useState(() => localStorage.getItem(NAME_KEY) ?? "");
  const [sessionName, setSessionName] = useState("B2B Session");
  const [hostPlays, setHostPlays] = useState(true);
  const [share, setShare] = useState(true);
  const [code, setCode] = useState("");
  const [sessions, setSessions] = useState<SharedSessionSummary[] | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => { localStorage.setItem(NAME_KEY, djName); }, [djName]);
  const refresh = () => run(async () => setSessions(await shareMusicsApi.listSessions()));
  useEffect(() => { if (signedIn) refresh(); }, [signedIn]);
  const go = (task: () => Promise<unknown>) => { setBusy(true); void task().catch(() => undefined).finally(() => setBusy(false)); };
  const name = djName.trim() || "DJ";
  return (
    <div className="junction-entry">
      <p className="junction-entry-lead">離れた場所のDJが順番にプレイし、1本の途切れないセットとして配信します。ホストのPCが配信の音を出します。</p>
      <label className="jl-field">DJ名<input value={djName} maxLength={40} placeholder="DJ" onChange={(e) => setDjName(e.target.value)} /></label>
      <ShareMusicsAccountCard open />
      <section className="junction-card">
        <h3 className="junction-card-title">ホストとして始める</h3>
        <label className="jl-field">セッション名<input value={sessionName} maxLength={60} onChange={(e) => setSessionName(e.target.value)} /></label>
        <label className="junction-check"><input type="checkbox" checked={hostPlays} onChange={(e) => setHostPlays(e.target.checked)} />自分もDJの順番に入る</label>
        {signedIn && <label className="junction-check"><input type="checkbox" checked={share} onChange={(e) => setShare(e.target.checked)} />PlumDeck Liteのメンバーに公開する（コード不要で参加できます）</label>}
        <button type="button" className="junction-btn junction-btn-primary" disabled={busy}
          onClick={() => go(() => junctionLink.startHost({ sessionName: sessionName.trim() || "B2B Session", djName: name, hostPlays, share: signedIn && share }))}>
          ホストとして開始
        </button>
      </section>
      <section className="junction-card">
        <h3 className="junction-card-title">参加する</h3>
        {signedIn && (
          <div className="jl-sessions">
            {sessions?.filter((s) => !s.mine).map((s) => (
              <div key={s.id} className="jl-session-row">
                <span><strong>{s.name}</strong> · {s.hostName}（{s.participantCount}人）</span>
                <button type="button" className="junction-btn" disabled={busy}
                  onClick={() => go(() => junctionLink.startGuest({ djName: name, shareSessionId: s.id, sessionName: s.name }))}>参加</button>
              </div>
            ))}
            {sessions && !sessions.some((s) => !s.mine) && <p className="junction-card-note">公開中のセッションはありません</p>}
            <button type="button" className="junction-btn junction-btn-quiet" onClick={refresh}>一覧を更新</button>
          </div>
        )}
        <label className="jl-field">ホストから受け取った招待コード<textarea rows={3} value={code} onChange={(e) => setCode(e.target.value)} placeholder="JLINK2.…" /></label>
        <button type="button" className="junction-btn junction-btn-primary" disabled={busy || !code.trim()}
          onClick={() => go(() => junctionLink.startGuest({ djName: name, inviteCode: code }))}>コードで参加</button>
      </section>
    </div>
  );
}

function ProgramOutput({ s }: { s: LinkSnapshot }) {
  const [devices, setDevices] = useState<{ id: string; displayName: string }[]>([]);
  const [device, setDevice] = useState("");
  useEffect(() => {
    run(async () => {
      const result = (await djEngineClient.send("audio.devices.list")) as { devices?: { id: string; displayName?: string; name: string; outputChannels: number }[] };
      setDevices((result.devices ?? []).filter((d) => d.outputChannels >= 2).map((d) => ({ id: d.id, displayName: d.displayName ?? d.name })));
    });
  }, []);
  const program = s.engine?.program;
  return (
    <section className="junction-card">
      <h3 className="junction-card-title">配信の出力</h3>
      <p className="junction-card-note">ON AIRのDJの音を{PROGRAM_DELAY_SECONDS}秒遅らせて、選んだデバイスへ出します（OBSにはBlackHoleなどを経由）。</p>
      {program?.open ? (
        <div className="jl-row">
          <span className="junction-badge">出力中：{devices.find((d) => d.id === program.deviceId)?.displayName ?? program.deviceId}</span>
          <button type="button" className="junction-btn" onClick={() => run(junctionLink.closeProgram)}>止める</button>
        </div>
      ) : (
        <div className="jl-row">
          <select value={device} onChange={(e) => setDevice(e.target.value)}>
            <option value="">デバイスを選ぶ</option>
            {devices.map((d) => <option key={d.id} value={d.id}>{d.displayName}</option>)}
          </select>
          <button type="button" className="junction-btn junction-btn-primary" disabled={!device} onClick={() => run(() => junctionLink.openProgram(device))}>出力する</button>
        </div>
      )}
      {s.lastSplice && SPLICE_LABEL[s.lastSplice.status] && (
        <p className="junction-card-note">
          最後の交代：{SPLICE_LABEL[s.lastSplice.status]}
          <details><summary>詳細</summary>遅れ {(s.lastSplice.lagFrames / 48).toFixed(1)} ms · 一致度 {s.lastSplice.confidence.toFixed(2)} · 配信の遅延 {s.programDelayMs?.toFixed(0) ?? "—"} ms</details>
        </p>
      )}
    </section>
  );
}

function Invites({ s }: { s: LinkSnapshot }) {
  const [answers, setAnswers] = useState<Record<string, string>>({});
  const invited = Object.entries(s.members).filter(([, m]) => m.inviteCode);
  return (
    <section className="junction-card">
      <h3 className="junction-card-title">コードで招待</h3>
      {invited.map(([id, m]) => (
        <div key={id} className="jl-invite">
          <CopyField label="1. この招待コードを相手に送る" value={m.inviteCode!} />
          <label className="jl-field">2. 相手から届いた応答コード<textarea rows={2} value={answers[id] ?? ""} onChange={(e) => setAnswers({ ...answers, [id]: e.target.value })} /></label>
          <div className="jl-row">
            <button type="button" className="junction-btn junction-btn-primary" disabled={!answers[id]?.trim()} onClick={() => run(() => junctionLink.acceptAnswerCode(id, answers[id]))}>接続</button>
            <button type="button" className="junction-btn junction-btn-quiet" onClick={() => junctionLink.cancelInvite(id)}>取り消す</button>
          </div>
        </div>
      ))}
      <button type="button" className="junction-btn" onClick={() => run(junctionLink.inviteByCode)}>招待コードを作る</button>
    </section>
  );
}

function Order({ s }: { s: LinkSnapshot }) {
  const view = turnView(s)!;
  const isHost = s.role === "host";
  const move = (index: number, delta: number) => {
    const order = [...view.session.order];
    const [id] = order.splice(index, 1);
    order.splice(Math.max(0, Math.min(order.length, index + delta)), 0, id);
    junctionLink.setOrder(order);
  };
  const outside = view.session.participants.filter((p) => !view.session.order.includes(p.id) && p.connected);
  return (
    <section className="junction-card">
      <h3 className="junction-card-title">順番</h3>
      <ol className="jl-order">
        {view.session.order.map((id, index) => {
          const p = view.session.participants.find((x) => x.id === id);
          const role = roleOf(view.session, id);
          const m = s.members[id];
          return (
            <li key={id} className={`jl-order-row jl-role-${role}${p?.connected ? "" : " is-offline"}`}>
              <span className="jl-order-name">{p?.name ?? "DJ"}{id === s.me && "（自分）"}{p?.kind === "lite" && <small> Lite</small>}</span>
              {ROLE_LABEL[role] && <span className={`jl-badge jl-badge-${role}`}>{ROLE_LABEL[role]}</span>}
              {!p?.connected && <span className="jl-badge">未接続</span>}
              {m?.rttMs != null && <small className="jl-dim">{Math.round(m.rttMs)} ms</small>}
              {isHost && (
                <span className="jl-order-actions">
                  <button type="button" className="junction-btn junction-btn-quiet" aria-label="上へ" disabled={index === 0} onClick={() => move(index, -1)}>▲</button>
                  <button type="button" className="junction-btn junction-btn-quiet" aria-label="下へ" disabled={index === view.session.order.length - 1} onClick={() => move(index, 1)}>▼</button>
                  {role !== "onAir" && role !== "next" && <button type="button" className="junction-btn junction-btn-quiet" onClick={() => junctionLink.setNext(id)}>次にする</button>}
                  {id !== s.me && <button type="button" className="junction-btn junction-btn-quiet" onClick={() => junctionLink.kick(id)}>外す</button>}
                </span>
              )}
            </li>
          );
        })}
      </ol>
      {isHost && outside.length > 0 && (
        <p className="junction-card-note">順番に入っていない参加者：{outside.map((p) => p.name).join("、")}
          {outside.some((p) => p.id === s.me) && <button type="button" className="junction-btn junction-btn-quiet" onClick={() => junctionLink.setHostPlays(true)}>自分を順番に入れる</button>}
        </p>
      )}
      {isHost && view.session.order.includes(s.me!) && <button type="button" className="junction-btn junction-btn-quiet" onClick={() => junctionLink.setHostPlays(false)}>自分は順番から外れる（運営だけ）</button>}
    </section>
  );
}

export function TurnControls({ s, compact = false }: { s: LinkSnapshot; compact?: boolean }) {
  const view = turnView(s);
  if (!view) return null;
  const hint = view.myRole === "next"
    ? "次はあなたです。JUNCTIONデッキで今の音を聴きながら準備し、NEXTを押してから自分の曲を上げてください。"
    : view.myRole === "onAir" && view.previous
      ? "あなたがON AIRです。前のDJの曲はJUNCTIONデッキで鳴っています。フェーダーを下げると自動で止まります。"
      : view.myRole === "onAir" ? "あなたがON AIRです。" : "";
  return (
    <div className={compact ? "jl-turn jl-turn-compact" : "jl-turn"}>
      <div className="jl-turn-now">
        <span className="jl-badge jl-badge-onAir">ON AIR</span><strong>{nameOf(s, view.onAir)}</strong>
        <span className="jl-badge jl-badge-next">NEXT</span><span>{nameOf(s, view.next)}</span>
      </div>
      {!compact && hint && <p className="junction-card-note">{hint}</p>}
      <div className="jl-row">
        <button type="button" className="junction-btn junction-btn-primary jl-next" disabled={!view.canNext} onClick={junctionLink.next}>
          {view.onAir ? "NEXT" : "セットを始める"}
        </button>
        {view.canRelease && <button type="button" className="junction-btn" onClick={junctionLink.releasePrevious}>前のDJの音を止める</button>}
        {!compact && s.role === "host" && view.onAir && <button type="button" className="junction-btn junction-btn-quiet" onClick={junctionLink.stopSet}>セットを止める</button>}
      </div>
    </div>
  );
}

function Session({ s }: { s: LinkSnapshot }) {
  const isHost = s.role === "host";
  return (
    <div className="jl-session">
      {s.answerCode && (
        <section className="junction-card">
          <h3 className="junction-card-title">応答コードをホストに送ってください</h3>
          <CopyField label="応答コード" value={s.answerCode} />
        </section>
      )}
      {s.phase === "joining" && !s.answerCode && <p className="junction-card-note" role="status">ホストの承認と接続を待っています…</p>}
      {s.session && <TurnControls s={s} />}
      {isHost && <ProgramOutput s={s} />}
      {s.session && <Order s={s} />}
      {isHost && s.pending.length > 0 && (
        <section className="junction-card">
          <h3 className="junction-card-title">参加リクエスト</h3>
          {s.pending.map((p) => (
            <div key={p.memberId} className="jl-row">
              <span>{p.name}{p.client === "lite" && <small> Lite</small>}</span>
              <button type="button" className="junction-btn junction-btn-primary" onClick={() => run(() => junctionLink.approve(p.memberId, true))}>承認</button>
              <button type="button" className="junction-btn junction-btn-quiet" onClick={() => run(() => junctionLink.approve(p.memberId, false))}>断る</button>
            </div>
          ))}
        </section>
      )}
      {isHost && <Invites s={s} />}
      <button type="button" className="junction-btn junction-btn-danger" onClick={() => run(junctionLink.leave)}>{isHost ? "セッションを終了" : "退出"}</button>
    </div>
  );
}

export function JunctionLinkPanel({ open, onClose }: { open: boolean; onClose: () => void }) {
  const s = useJunctionLink();
  return (
    <div id="junction-panel" className={`junction-panel${open ? " junction-panel-open" : ""}`} role="complementary" aria-label="Junction" aria-hidden={!open} inert={!open}
      onKeyDown={(e) => e.stopPropagation()} onKeyUp={(e) => e.stopPropagation()}>
      <div className="junction-panel-head">
        <h2>Junction{s.sessionName && ` · ${s.sessionName}`}</h2>
        <button type="button" className="junction-btn" onClick={onClose}>閉じる</button>
      </div>
      <div className="junction-panel-body">
        {s.error && <p className="junction-card-error" role="alert">{s.error}</p>}
        {!isTauri() ? <p className="junction-card-note">Junctionはデスクトップアプリで使えます。</p>
          : s.phase === "idle" ? <Entry />
          : s.phase === "starting" ? <p className="junction-card-note" role="status">準備しています…</p>
          : <Session s={s} />}
      </div>
    </div>
  );
}

export function JunctionLinkBar({ showRootButton = true }: { showRootButton?: boolean }) {
  const s = useJunctionLink();
  const [open, setOpen] = useState(false);
  const view = turnView(s);
  return (
    <>
      {showRootButton && (
        <button id="junction-toggle" type="button" className="junction-root" aria-expanded={open} aria-controls="junction-panel" onClick={() => setOpen((v) => !v)}>
          <b>Junction</b>
          {view ? (
            <>
              <span>ON AIR：{nameOf(s, view.onAir)}</span>
              <span className="junction-root-dim">NEXT：{nameOf(s, view.next)}</span>
            </>
          ) : <span className="junction-root-dim">{s.phase === "idle" ? "セッションなし" : "接続中"}</span>}
          {s.pending.length > 0 && <span className="junction-root-badge">{s.pending.length}</span>}
        </button>
      )}
      <JunctionLinkPanel open={open} onClose={() => setOpen(false)} />
    </>
  );
}
