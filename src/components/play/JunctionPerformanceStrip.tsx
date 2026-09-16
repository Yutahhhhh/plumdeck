import { useEffect, useRef, useState } from "react";
import { Headphones, Pause, Play, Radio, Volume2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { djEngineClient } from "@/services/dj-engine/client";
import { junctionCommand } from "@/services/junction/client";
import {
  JUNCTION_MASTER, LOCAL_NEXT, PROGRAM_MASTER, STAGE_LABEL,
  junctionRoles, junctionStage, programMixerView, stripActions,
} from "@/services/junction/program-mixer";
import { participantName } from "@/services/junction/roster-model";
import type { AudioDevice, ChannelState, DeckId, DeckState } from "@/types/dj-engine";
import type { JunctionInputSettings, JunctionSnapshot } from "@/types/junction";
import { Fader } from "./SoftwareDeck";

const SEND_INTERVAL_MS = 50;
const NEXT_DECK_KEY = "plumdeck.junction.localNextDeck";

type Props = {
  snapshot: JunctionSnapshot;
  decks: readonly DeckId[];
  connected: boolean;
  deckState: (deck: DeckId) => DeckState | undefined;
  channelState: (deck: DeckId) => ChannelState | undefined;
  /** Makes the deck the library's load target. */
  onSelectDeck: (deck: DeckId) => void;
  onTogglePlay: (deck: DeckId) => void;
  onPfl: (deck: DeckId, enabled: boolean) => void;
};

/**
 * Always-visible Junction controls on the player: JUNCTION MASTER (the previous
 * DJ's current sound), LOCAL NEXT (what this DJ plays next), PROGRAM MASTER
 * (what the venue hears) and the handoff actions, without opening the panel.
 */
export function JunctionPerformanceStrip({ snapshot, decks, connected, deckState, channelState, onSelectDeck, onTogglePlay, onPfl }: Props) {
  const roles = junctionRoles(snapshot);
  const stage = junctionStage(snapshot);
  const view = programMixerView(snapshot);
  const actions = stripActions(snapshot);
  const host = snapshot.localPeerId === snapshot.hostPeerId;
  const input = snapshot.junctionInput;
  const channel = input?.channel;
  const name = (peerId: string) => {
    const row = snapshot.participants.find((participant) => participant.peerId === peerId);
    return row ? participantName(row) : peerId ? "DJ" : "—";
  };

  const [error, setError] = useState<string | null>(null);
  const run = (task: () => Promise<unknown>) => void task().then(() => setError(null), (cause) => setError(cause instanceof Error ? cause.message : String(cause)));

  // JUNCTION MASTER level is coalesced like the full channel strip.
  const [draftVolume, setDraftVolume] = useState<number | null>(null);
  const pending = useRef<JunctionInputSettings>({});
  const timer = useRef<number | null>(null);
  const sendInput = (settings: JunctionInputSettings) => {
    pending.current = { ...pending.current, ...settings };
    if (timer.current !== null) return;
    timer.current = window.setTimeout(() => {
      timer.current = null;
      const next = pending.current;
      pending.current = {};
      run(() => junctionCommand("input.set", { ...next }).finally(() => setDraftVolume(null)));
    }, SEND_INTERVAL_MS);
  };
  useEffect(() => () => { if (timer.current !== null) window.clearTimeout(timer.current); }, []);

  const [nextDeck, setNextDeck] = useState<DeckId>(() => {
    const saved = localStorage.getItem(NEXT_DECK_KEY) as DeckId | null;
    return saved && decks.includes(saved) ? saved : decks[1] ?? decks[0];
  });
  useEffect(() => { if (!decks.includes(nextDeck)) setNextDeck(decks[0]); }, [decks, nextDeck]);
  const chooseNextDeck = (deck: DeckId) => { setNextDeck(deck); localStorage.setItem(NEXT_DECK_KEY, deck); onSelectDeck(deck); };
  const next = deckState(nextDeck);
  const nextPfl = Boolean(channelState(nextDeck)?.pfl);
  const canPrepare = connected && (roles.localOperator || Boolean(snapshot.localPrep));

  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [programDevice, setProgramDevice] = useState(snapshot.program.outputDevice ?? "");
  useEffect(() => {
    if (!host) return;
    void djEngineClient.listAudioDevices().then((data) => setDevices(((data as { devices?: AudioDevice[] }).devices ?? []).filter((device) => device.outputChannels >= 2))).catch(() => undefined);
  }, [host]);
  const deviceValue = (id: string) => (/^(coreaudio|portaudio):\d+$/.test(id) ? id.split(":")[1] : id);

  const [nominee, setNominee] = useState("");
  const nominate = actions.find((action) => action.kind === "nominate");
  useEffect(() => {
    if (nominate?.kind === "nominate" && !nominate.candidates.some((row) => row.peerId === nominee)) setNominee(nominate.candidates[0]?.peerId ?? "");
  }, [nominate, nominee]);

  const junctionControllable = Boolean(input?.peerId && channel?.available) && stage !== "sending";
  const volume = draftVolume ?? channel?.volume ?? 1;
  const jmPfl = Boolean(channel?.pfl);
  const meter = Math.max(0, Math.min(1, snapshot.program.meter ?? 0));

  return <section className={cn("dj-junction-strip", `is-${stage}`)} aria-label="Junction演奏コントロール">
    <div className="dj-junction-strip-roles">
      <span className="dj-junction-strip-stage"><Radio />{STAGE_LABEL[stage]}</span>
      <span title="セッションの管理者">ホスト <b>{name(roles.coordinatorPeerId)}</b></span>
      <span title="Program Masterを操作しているDJ">操作権 <b>{name(roles.operatorPeerId)}</b></span>
      {roles.soundingPeerId && <span title="受け手が下げ切るまで音を送り続けています">送出中 <b>{name(roles.soundingPeerId)}</b></span>}
      {roles.nextPeerId && <span>次のDJ <b>{name(roles.nextPeerId)}</b></span>}
    </div>

    <div className="dj-junction-strip-cell" aria-label={JUNCTION_MASTER}>
      <header><strong>{JUNCTION_MASTER}</strong><small>{input?.peerId ? `${name(input.peerId)}の現在の音` : "前のDJの音声はまだありません"}</small></header>
      <div className="dj-junction-strip-row">
        <Fader label="JUNCTION MASTERのレベル" min={0} max={1} value={volume} disabled={!junctionControllable}
          onChange={(value) => { setDraftVolume(value); sendInput({ volume: value }); }} />
        <button type="button" className={cn("dj-mixer-cue", jmPfl && "is-on")} aria-pressed={jmPfl} disabled={!junctionControllable || !channel?.pflAvailable}
          title={channel?.pflAvailable ? "ヘッドホンでJUNCTION MASTERを聴く" : "ヘッドホン出力のあるデバイスでCUEを使えます"} onClick={() => sendInput({ pfl: !jmPfl })}>CUE</button>
      </div>
      <small className={cn("dj-junction-strip-flag", view.junctionMasterInProgram && "is-live")}>
        {input?.peerId ? input.receiving ? `${view.junctionMasterInProgram ? "Programに出力中" : "CUEのみ"} · 遅延 ${Math.round(input.latencyMs)}ms` : "音声を待っています" : "—"}
      </small>
    </div>

    <div className="dj-junction-strip-cell" aria-label={LOCAL_NEXT}>
      <header><strong>{LOCAL_NEXT}</strong>
        <span className="dj-segmented" role="group" aria-label="LOCAL NEXTのデッキ">{decks.map((deck) => <button key={deck} type="button" aria-pressed={deck === nextDeck} className={deck === nextDeck ? "is-on" : ""} onClick={() => chooseNextDeck(deck)}>{deck}</button>)}</span>
      </header>
      <div className="dj-junction-strip-row">
        <button type="button" className="dj-button" disabled={!canPrepare || !next?.track} aria-label={next?.status === "playing" ? "LOCAL NEXTを一時停止" : "LOCAL NEXTを再生"}
          title={canPrepare ? undefined : "操作権がないため、この画面ではまだ再生できません"} onClick={() => onTogglePlay(nextDeck)}>{next?.status === "playing" ? <Pause /> : <Play />}</button>
        <span className="dj-junction-strip-track" title={next?.track?.title ?? undefined}>{next?.track ? next.track.title || "タイトル未設定" : `ライブラリからDECK ${nextDeck}へロード`}</span>
        <button type="button" className={cn("dj-mixer-cue", nextPfl && "is-on")} aria-pressed={nextPfl} disabled={!connected} title="ヘッドホンでLOCAL NEXTを聴く" onClick={() => onPfl(nextDeck, !nextPfl)}>CUE</button>
      </div>
      <small className={cn("dj-junction-strip-flag", view.localNextInProgram && "is-live")}>{view.localNextCueOnly ? "CUEのみ（会場には出ません）" : "Programに出力"}</small>
    </div>

    <div className="dj-junction-strip-cell" aria-label={PROGRAM_MASTER}>
      <header><strong><Volume2 />{PROGRAM_MASTER}</strong><small>{view.venueLabel}</small></header>
      <div className="dj-junction-strip-row">
        <span className="dj-junction-strip-meter" aria-label={`Program Masterのレベル ${Math.round(meter * 100)}%`}><i style={{ width: `${meter * 100}%` }} /></span>
        <span className="dj-segmented" role="group" aria-label="ヘッドホンモニター">
          <button type="button" className={jmPfl && !nextPfl ? "is-on" : ""} disabled={!junctionControllable || !channel?.pflAvailable || !connected} title="JUNCTION MASTERだけをモニター"
            onClick={() => { sendInput({ pfl: true }); if (nextPfl) onPfl(nextDeck, false); }}><Headphones />JM</button>
          <button type="button" className={nextPfl && !jmPfl ? "is-on" : ""} disabled={!connected} title="LOCAL NEXTだけをモニター"
            onClick={() => { onPfl(nextDeck, true); if (jmPfl && junctionControllable) sendInput({ pfl: false }); }}>NEXT</button>
        </span>
      </div>
      {host && <div className="dj-junction-strip-row">
        <select aria-label="Program Masterの出力先" value={programDevice} onChange={(event) => setProgramDevice(event.target.value)}>
          <option value="">出力先を選択</option>
          {devices.map((device) => <option key={device.id} value={deviceValue(device.id)}>{device.name}</option>)}
        </select>
        <button type="button" className="dj-button" disabled={!programDevice || programDevice === snapshot.program.outputDevice} onClick={() => run(() => junctionCommand("program.configure", { programDevice }))}>出力を反映</button>
      </div>}
      <small className={cn("dj-junction-strip-flag", view.bypass && "is-warning")}>{view.bypass ? "引き継ぐまで会場には前のDJの送出音がそのまま出ます" : view.returnLabel}</small>
    </div>

    <div className="dj-junction-strip-actions">
      {actions.map((action) => {
        if (action.kind === "release") return <button key="release" type="button" className="dj-button" title="前のDJの送出を止めます。JUNCTION MASTERを下げ切って1.5秒たつと自動で解放されます" onClick={() => run(() => junctionCommand("input.release"))}>{action.label}</button>;
        if (action.kind === "accept") return <button key="accept" type="button" className="dj-button is-on" disabled={action.disabled} onClick={() => run(() => junctionCommand("handoff.accept"))}>{action.label}</button>;
        if (action.kind === "request") return <button key="request" type="button" className="dj-button" onClick={() => run(() => junctionCommand("handoff.request", { targetPeerId: snapshot.localPeerId }))}>{action.label}</button>;
        return <span key="nominate" className="dj-junction-strip-nominate">
          <select aria-label="次のDJの候補" value={nominee} onChange={(event) => setNominee(event.target.value)}>
            {action.candidates.map((row) => <option key={row.peerId} value={row.peerId}>{participantName(row)}{row.rosterStatus === "requested" ? "（演奏希望）" : row.rosterStatus === "finished" ? "（演奏済み）" : ""}</option>)}
          </select>
          <button type="button" className="dj-button" disabled={!nominee} onClick={() => run(() => junctionCommand("handoff.request", { targetPeerId: nominee }))}>{action.label}</button>
        </span>;
      })}
    </div>
    {error && <p role="alert" className="dj-junction-strip-error">{error}</p>}
  </section>;
}
