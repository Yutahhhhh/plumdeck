import { useEffect, useState } from "react";
import { Radio } from "lucide-react";
import { cn } from "@/lib/utils";
import { djEngineClient } from "@/services/dj-engine/client";
import { junctionCommand } from "@/services/junction/client";
import { programMixerView } from "@/services/junction/program-mixer";
import { participantName } from "@/services/junction/roster-model";
import { CUE_LABEL, SIGNAL_LABEL, notOnAirReason, turnGuidance, turnSignal } from "@/services/junction/turn-state";
import type { AudioDevice } from "@/types/dj-engine";
import type { JunctionSnapshot, TurnCueKind, TurnNextStatus } from "@/types/junction";

/** A cue stays on screen long enough to be seen across a booth, then fades. */
const CUE_VISIBLE_MS = 15_000;
const CUES: TurnCueKind[] = ["one_more", "go_ahead", "hold", "ok"];
const NEXT_STATUS: Record<Exclude<TurnNextStatus, "">, string> = {
  receiving: "受信中",
  loaded: "ロード済み",
  cueing: "CUE中",
  ready: "READY",
};

/**
 * The booth at a glance: this DJ's signal light, one line of guidance, and the
 * few actions a real handover needs. Routes, latency and the return feed live
 * in the collapsed diagnostics.
 */
export function JunctionTurnBar({ snapshot }: { snapshot: JunctionSnapshot }) {
  const turn = snapshot.turn;
  const signal = turnSignal(snapshot);
  const host = snapshot.localPeerId === snapshot.hostPeerId;
  const view = programMixerView(snapshot);
  const name = (peerId: string) => {
    const row = snapshot.participants.find((participant) => participant.peerId === peerId);
    return row ? participantName(row) : "DJ";
  };
  const [error, setError] = useState<string | null>(null);
  const run = (task: () => Promise<unknown>) => void task().then(() => setError(null), (cause) => setError(cause instanceof Error ? cause.message : String(cause)));

  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [programDevice, setProgramDevice] = useState(snapshot.program.outputDevice ?? "");
  useEffect(() => {
    if (!host) return;
    void djEngineClient.listAudioDevices().then((data) => setDevices(((data as { devices?: AudioDevice[] }).devices ?? []).filter((device) => device.outputChannels >= 2))).catch(() => undefined);
  }, [host]);
  const deviceValue = (id: string) => (/^(coreaudio|portaudio):\d+$/.test(id) ? id.split(":")[1] : id);

  const guidance = turnGuidance(snapshot);
  const notOnAir = turn?.localAudible ? notOnAirReason(snapshot) : "";
  const cue = turn?.cue && Date.now() - turn.cue.at < CUE_VISIBLE_MS ? turn.cue : undefined;
  const live = snapshot.lifecycle === "live";
  const nextStatus = signal === "onair" && turn?.nextPeerId && turn.nextStatus ? NEXT_STATUS[turn.nextStatus] : "";
  const meter = Math.max(0, Math.min(1, snapshot.program.meter ?? 0));

  return <section className={cn("dj-turn-bar", `is-${signal}`)} aria-label="Junctionの交代">
    <span className="dj-turn-light" role="status" aria-label={`信号 ${SIGNAL_LABEL[signal]}`}><Radio />{SIGNAL_LABEL[signal]}</span>
    <p className="dj-turn-guidance">{guidance || (live ? "順番を待っています" : "セッション開始を待っています")}</p>
    {nextStatus && <span className={cn("dj-turn-next", turn?.nextStatus === "ready" && "is-ready")} title="次のDJの準備状況">次 {name(turn!.nextPeerId)} · {nextStatus}</span>}
    {cue && <span className="dj-turn-cue" role="status">{cue.fromPeerId === snapshot.localPeerId ? "送信：" : `${name(cue.fromPeerId)}：`}{CUE_LABEL[cue.kind]}</span>}
    <div className="dj-turn-actions">
      {signal === "ready" && <button type="button" className="dj-button is-on" title="フェーダーを上げなくても、今すぐ本番に出ます" onClick={() => run(() => junctionCommand("turn.onair"))}>今すぐON AIR</button>}
      {signal === "onair" && turn?.outgoingPeerId && <button type="button" className="dj-button" title="前のDJの曲を止めます。JUNCTION MASTERを下げ切って1.5秒たつと自動で解放されます" onClick={() => run(() => junctionCommand("turn.release"))}>前のDJを解放</button>}
      {live && signal !== "off" && <span className="dj-segmented dj-turn-cues" role="group" aria-label="ブースの合図">
        {CUES.map((kind) => <button key={kind} type="button" onClick={() => run(() => junctionCommand("turn.cue", { kind }))}>{CUE_LABEL[kind]}</button>)}
      </span>}
    </div>
    {notOnAir && <p className="dj-turn-warning" role="alert">{notOnAir}</p>}
    {error && <p className="dj-turn-warning" role="alert">{error}</p>}
    <details className="dj-turn-diagnostics">
      <summary>診断</summary>
      <dl>
        <dt>会場の音</dt><dd>{view.venueLabel}</dd>
        <dt>返送</dt><dd>{view.returnLabel}</dd>
        {snapshot.junctionInput?.peerId && <><dt>JUNCTION MASTER</dt><dd>{snapshot.junctionInput.receiving ? `受信中 · バッファ ${Math.round(snapshot.junctionInput.latencyMs)}ms · 途切れ ${snapshot.junctionInput.underruns}` : "音声を待っています"}</dd></>}
        {turn?.latency && <><dt>回線の遅延</dt><dd>{turn.latency.pathMs}ms / 上限 {Math.round(turn.latency.budgetMs)}ms</dd></>}
        <dt>会場出力</dt><dd><span className="dj-turn-meter" aria-label={`会場出力のレベル ${Math.round(meter * 100)}%`}><i style={{ width: `${meter * 100}%` }} /></span></dd>
      </dl>
      {host && <div className="dj-turn-device">
        <select aria-label="会場への音声出力" value={programDevice} onChange={(event) => setProgramDevice(event.target.value)}>
          <option value="">出力先を選択</option>
          {devices.map((device) => <option key={device.id} value={deviceValue(device.id)}>{device.name}</option>)}
        </select>
        <button type="button" className="dj-button" disabled={!programDevice || programDevice === snapshot.program.outputDevice} onClick={() => run(() => junctionCommand("program.configure", { programDevice }))}>出力を反映</button>
      </div>}
    </details>
  </section>;
}
