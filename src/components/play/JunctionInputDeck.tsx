import { useEffect, useRef, useState } from "react";
import { Radio } from "lucide-react";
import { cn } from "@/lib/utils";
import type { JunctionInputDeck as RemoteDeck, JunctionInputSettings, JunctionInputState } from "@/types/junction";
import { RotaryKnob } from "./RotaryKnob";
import { Fader, formatTime } from "./SoftwareDeck";

const WINDOW_MS = 10_000;
// Snapshots arrive about once a second; drawing one second behind keeps the
// scroll continuous instead of jumping on every update.
const DISPLAY_DELAY_MS = 1_000;
const SEND_INTERVAL_MS = 50;
const DECK_COLOR: Record<string, string> = { A: "--dj-blue", B: "--dj-orange", C: "--dj-blue", D: "--dj-orange" };
const ASSIGN = [{ value: 0, label: "A" }, { value: 1, label: "THRU" }, { value: 2, label: "B" }] as const;

type Props = {
  state: JunctionInputState;
  performerName?: string;
  onSet: (settings: JunctionInputSettings) => Promise<unknown>;
  onRelease: () => Promise<unknown>;
};

function loudest(decks: readonly RemoteDeck[] | undefined, name: string | undefined): RemoteDeck | undefined {
  return decks?.find((deck) => deck.deck === name) ?? decks?.find((deck) => deck.role === "current");
}

/**
 * The JUNCTION deck: the other DJ's audio as one mixer channel. One lane shows
 * what arrived; the colour of each range says which of their decks was loudest.
 */
export function JunctionInputDeck({ state, performerName, onSet, onRelease }: Props) {
  const channel = state.channel;
  const [draft, setDraft] = useState<JunctionInputSettings>({});
  const pending = useRef<JunctionInputSettings>({});
  const timer = useRef<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const laneRef = useRef({ state, receivedAt: performance.now() });
  laneRef.current = { state, receivedAt: laneRef.current.state === state ? laneRef.current.receivedAt : performance.now() };

  const value = <K extends keyof JunctionInputSettings>(key: K, fallback: NonNullable<JunctionInputSettings[K]>) =>
    (draft[key] ?? channel[key as keyof typeof channel] ?? fallback) as NonNullable<JunctionInputSettings[K]>;

  const flush = () => {
    timer.current = null;
    const settings = pending.current;
    pending.current = {};
    if (!Object.keys(settings).length) return;
    void onSet(settings).then(() => setError(null), (cause) => setError(cause instanceof Error ? cause.message : String(cause)))
      .finally(() => setDraft((old) => { const next = { ...old }; for (const key of Object.keys(settings)) delete next[key as keyof JunctionInputSettings]; return next; }));
  };
  const change = (settings: JunctionInputSettings) => {
    setDraft((old) => ({ ...old, ...settings }));
    pending.current = { ...pending.current, ...settings };
    if (timer.current === null) timer.current = window.setTimeout(flush, SEND_INTERVAL_MS);
  };
  useEffect(() => () => { if (timer.current !== null) window.clearTimeout(timer.current); }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    let frame = 0;
    const draw = () => {
      frame = requestAnimationFrame(draw);
      const context = canvas.getContext("2d");
      if (!context) return;
      const ratio = window.devicePixelRatio || 1;
      const width = canvas.clientWidth, height = canvas.clientHeight;
      if (canvas.width !== Math.round(width * ratio) || canvas.height !== Math.round(height * ratio)) { canvas.width = Math.round(width * ratio); canvas.height = Math.round(height * ratio); }
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
      context.clearRect(0, 0, width, height);
      const { state: current, receivedAt } = laneRef.current;
      const styles = getComputedStyle(canvas);
      const color = (deck: string) => (DECK_COLOR[deck] && styles.getPropertyValue(DECK_COLOR[deck]).trim()) || "#5b616a";
      const viewEnd = performance.now() - DISPLAY_DELAY_MS;
      const scale = width / WINDOW_MS;
      const { peaks, decks, bucketMs } = current.lane;
      const barWidth = Math.max(1, bucketMs * scale - .5);
      for (let index = 0; index < peaks.length; index += 1) {
        const x = width - (viewEnd - (receivedAt - (peaks.length - 1 - index) * bucketMs)) * scale;
        if (x < -barWidth || x > width) continue;
        const bar = Math.max(1, peaks[index] * (height - 6));
        context.fillStyle = color(decks[index] ?? "");
        context.fillRect(x, (height - bar) / 2, barWidth, bar);
      }
      const deck = loudest(current.decks, current.audibleDeck);
      if (deck && deck.bpm > 0 && deck.rate > 0 && deck.firstBeatMs !== undefined) {
        // Beat positions on the loudest remote deck, scrolled with the lane.
        const beatMs = 60_000 / deck.bpm;
        const position = deck.positionMs + (deck.playing ? (viewEnd - receivedAt) * deck.rate : 0);
        const first = Math.floor((position - deck.firstBeatMs) / beatMs);
        context.strokeStyle = color(deck.deck);
        for (let beat = first; beat > first - 64; beat -= 1) {
          const x = width - ((position - (deck.firstBeatMs + beat * beatMs)) / deck.rate) * scale;
          if (x < 0) break;
          const downbeat = ((beat % deck.beatsPerBar) + deck.beatsPerBar) % deck.beatsPerBar === 0;
          context.globalAlpha = downbeat ? .85 : .3;
          context.lineWidth = downbeat ? 1.5 : 1;
          context.beginPath(); context.moveTo(Math.round(x) + .5, 0); context.lineTo(Math.round(x) + .5, height); context.stroke();
        }
        context.globalAlpha = 1;
      }
    };
    frame = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(frame);
  }, []);

  const deck = loudest(state.decks, state.audibleDeck);
  const releasing = Boolean(state.releasingPeerId);
  const stage = releasing ? "MIXING" : state.receiving ? "CUEING" : "WAITING";
  const vu = Math.max(0, Math.min(1, state.channel.vu ?? 0));
  const source = state.djName || performerName || "前のDJ";
  const run = (task: () => Promise<unknown>) => void task().then(() => setError(null), (cause) => setError(cause instanceof Error ? cause.message : String(cause)));

  return <section className={cn("dj-junction-input", releasing && "is-releasing")} aria-label="JUNCTION MASTER">
    <div className="dj-junction-input-head">
      <span className="dj-junction-input-badge"><Radio />JUNCTION MASTER</span>
      <strong title={source}>{source}</strong>
      <span className={cn("dj-junction-input-status", state.receiving && "is-live")}>{state.receiving ? `${stage} · 遅延 ${Math.round(state.latencyMs)}ms` : `${stage} · 音声を待っています`}</span>
    </div>
    <div className="dj-junction-input-track">
      {deck ? <><i style={{ background: `var(${DECK_COLOR[deck.deck] ?? "--dj-blue"})` }} /><b title={deck.title}>{deck.title || "タイトル未設定"}</b><span>{[deck.artist, deck.bpm ? `${(deck.bpm * deck.rate).toFixed(1)} BPM` : "", `${formatTime(deck.positionMs)} / ${formatTime(deck.durationMs)}`].filter(Boolean).join(" · ")}</span></>
        : <span>曲の情報はまだ届いていません</span>}
    </div>
    <canvas ref={canvasRef} className="dj-junction-input-lane" aria-label="JUNCTION MASTERの波形。色は前のDJのどのデッキが鳴っていたかを表します" />
    <div className="dj-junction-input-controls">
      <div className="dj-junction-input-eq">
        {(["eqHigh", "eqMid", "eqLow"] as const).map((key) => <RotaryKnob key={key} label={key === "eqHigh" ? "HIGH" : key === "eqMid" ? "MID" : "LOW"} min={0} max={4} step={.04} fineStep={.01} defaultValue={1} center={1}
          value={value(key, 1)} disabled={!channel.available} valueText={(gain) => `${gain.toFixed(2)}×`} onChange={(gain) => change({ [key]: gain })} />)}
      </div>
      <div className="dj-junction-input-level">
        <span>LEVEL</span>
        <Fader label="JUNCTION MASTERのレベル" min={0} max={1} value={value("volume", 1)} disabled={!channel.available} onChange={(volume) => change({ volume })} />
        <span className="dj-junction-input-vu" aria-label={`JUNCTION MASTERの入力レベル ${Math.round(vu * 100)}%`}><i style={{ width: `${vu * 100}%` }} /></span>
      </div>
      <div className="dj-segmented dj-junction-input-assign" role="group" aria-label="クロスフェーダーの割り当て">
        {ASSIGN.map((option) => <button key={option.value} type="button" aria-pressed={value("orientation", 1) === option.value} className={value("orientation", 1) === option.value ? "is-on" : ""} disabled={!channel.available} onClick={() => change({ orientation: option.value })}>{option.label}</button>)}
      </div>
      <button type="button" className={cn("dj-mixer-cue", value("pfl", false) && "is-on")} aria-pressed={value("pfl", false)} disabled={!channel.available || !channel.pflAvailable}
        title={channel.pflAvailable ? "ヘッドホンでJUNCTION MASTERをモニターする" : "ヘッドホン出力のあるデバイスでCUEを使えます"} onClick={() => change({ pfl: !value("pfl", false) })}>CUE</button>
      {releasing && <button type="button" className="dj-button dj-junction-input-release" title="前のDJの送出を止めます。JUNCTION MASTERのレベルを0にして1.5秒たつと自動で解放します" onClick={() => run(onRelease)}>前のDJを解放</button>}
    </div>
    {releasing && <p className="dj-junction-input-note">{source}の音はここで鳴り続けています。フェーダーで下げ切ると自動で解放されます。</p>}
    {error && <p role="alert" className="dj-junction-input-error">{error}</p>}
  </section>;
}
