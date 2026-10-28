import { useRef, useState, type PointerEvent } from "react";
import { cn } from "@/lib/utils";
import type { BeatFxState, DeckId } from "@/types/dj-engine";
import { BEAT_FX, BEAT_FX_MENU, beatFxHasBeats, beatFxMaxBeats, beatFxMinBeats } from "@/services/midi/beat-fx";
import { formatBeats } from "./loop-math";

const BEAT_STEPS = [1 / 16, 1 / 8, 1 / 4, 1 / 2, 1, 2, 4, 8, 16];

type Props = {
  state?: BeatFxState;
  /** Why the unit cannot be used; undefined when it can. */
  disabledReason?: string;
  releaseSupported: boolean;
  decks: readonly DeckId[];
  micAvailable: boolean;
  samplerAvailable: boolean;
  /** Engine capabilities; the beat range widens when the host declares it. */
  capabilities: readonly string[];
  send: (params: Record<string, unknown>) => Promise<unknown>;
};

/**
 * BEAT FX unit (FX1). The generic effect panel is gone: Mixxx's generic
 * effects live on as PAD FX slot types, so nothing competes for the deck
 * chains. FX2, MULTI and SNAPSHOT come with the engine that provides them.
 */
export function BeatFxPanel({ state, disabledReason, releaseSupported, decks, micAvailable, samplerAvailable, capabilities, send }: Props) {
  const [error, setError] = useState("");
  const [releaseHeld, setReleaseHeld] = useState(false);
  const releaseRef = useRef(false);
  const taps = useRef<number[]>([]);
  const off = Boolean(disabledReason);
  const set = (params: Record<string, unknown>) => { setError(""); void send(params).catch((cause) => setError(cause instanceof Error ? cause.message : String(cause))); };
  const effect = state?.effect ?? "echo";
  const beats = state?.beats ?? 1;
  const hasBeats = beatFxHasBeats(effect);
  const steps = BEAT_STEPS.filter((value) => value >= beatFxMinBeats(effect, capabilities) && value <= beatFxMaxBeats(effect, capabilities));
  const index = steps.reduce((best, value, at) => Math.abs(value - beats) < Math.abs(steps[best] - beats) ? at : best, 0);
  const targets: { value: string; label: string; title: string }[] = [
    ...decks.map((deck) => ({ value: deck, label: deck, title: `Deck ${deck} にかける` })),
    ...(state?.target && !decks.includes(state.target as DeckId) && /^[A-D]$/.test(state.target) ? [{ value: state.target, label: state.target, title: `Deck ${state.target} にかける` }] : []),
    { value: "master", label: "MST", title: "MASTER 出力にかける" },
    ...(micAvailable ? [{ value: "mic", label: "MIC", title: "マイクにかける" }] : []),
    ...(samplerAvailable ? [{ value: "sampler", label: "SMP", title: "サンプラーにかける" }] : []),
  ];
  const tap = () => {
    const now = performance.now(), last = taps.current[taps.current.length - 1];
    if (last === undefined || now - last > 1500) taps.current = [];
    else if (now - last < 200) return;
    taps.current = [...taps.current, now].slice(-5);
    if (taps.current.length < 2) return;
    const bpm = 60_000 * (taps.current.length - 1) / (now - taps.current[0]);
    if (bpm >= 40 && bpm <= 300) set({ bpm: Number(bpm.toFixed(1)) });
  };
  const hold = (held: boolean) => {
    if (held === releaseRef.current) return;
    releaseRef.current = held;
    setReleaseHeld(held);
    set({ release: held });
  };
  const release = (held: boolean) => (event: PointerEvent<HTMLButtonElement>) => {
    if (held && event.button !== 0) return;
    if (held) event.currentTarget.setPointerCapture(event.pointerId);
    hold(held);
  };
  const releaseReason = disabledReason ?? (releaseSupported ? undefined : "RELEASE FX：この音声エンジン未対応");
  const releasing = releaseHeld || Boolean(state?.releaseActive);

  return <section className="dj-beatfx" aria-label="BEAT FX">
    <span className="dj-beatfx-label">BEAT FX</span>
    <button type="button" className={cn("dj-beatfx-on", state?.enabled && "is-on")} disabled={off} aria-pressed={Boolean(state?.enabled)}
      title={disabledReason ?? "BEAT FX の ON / OFF"} onClick={() => set({ enabled: !state?.enabled })}>ON</button>
    <select className="dj-beatfx-select" aria-label="BEAT FX の種類" disabled={off} value={effect} title={disabledReason ?? "エフェクトの種類"} onChange={(event) => set({ effect: event.target.value })}>
      {BEAT_FX_MENU.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
      {!BEAT_FX_MENU.some(([value]) => value === effect) && <option value={effect} disabled>{BEAT_FX.find(([value]) => value === effect)?.[1] ?? effect}</option>}
    </select>
    {hasBeats && <div className="dj-beatfx-beats" role="group" aria-label="BEAT FX の拍">
      <button type="button" disabled={off || index <= 0} aria-label="拍を半分にする" title={disabledReason ?? "拍を半分にする"} onClick={() => set({ beats: steps[index - 1] })}>◀</button>
      <output className="dj-num" title="エフェクトの拍">{formatBeats(steps[index] ?? beats)}</output>
      <button type="button" disabled={off || index >= steps.length - 1} aria-label="拍を倍にする" title={disabledReason ?? "拍を倍にする"} onClick={() => set({ beats: steps[index + 1] })}>▶</button>
    </div>}
    <label className="dj-beatfx-level" title={disabledReason ?? "LEVEL / DEPTH"}>
      <span>LEVEL/DEPTH</span>
      <input type="range" min="0" max="1" step=".01" disabled={off} aria-label="BEAT FX の LEVEL / DEPTH" value={state?.mix ?? .5} onChange={(event) => set({ mix: Number(event.target.value) })} />
      <b className="dj-num">{Math.round((state?.mix ?? .5) * 100)}%</b>
    </label>
    <div className="dj-beatfx-assign" role="group" aria-label="BEAT FX をかける先">
      {targets.map((target) => <button key={target.value} type="button" className={cn(state?.target === target.value && "is-on")} disabled={off} aria-pressed={state?.target === target.value}
        title={disabledReason ?? target.title} onClick={() => set({ target: target.value })}>{target.label}</button>)}
    </div>
    <div className="dj-beatfx-bpm" role="group" aria-label="BEAT FX の BPM">
      <button type="button" className={cn(state?.auto !== false && "is-on")} disabled={off} aria-pressed={state?.auto !== false}
        title={disabledReason ?? "AUTO：デッキの BPM に合わせます"} onClick={() => { taps.current = []; set({ auto: true }); }}>AUTO</button>
      <button type="button" disabled={off} title={disabledReason ?? "TAP：リズムに合わせて 2 回以上押すと BPM を決めます"} onClick={tap}>TAP</button>
      <span className="dj-num" title="BEAT FX の BPM">{state?.auto === false && state.bpm ? `${state.bpm.toFixed(1)} BPM` : "AUTO"}</span>
    </div>
    <button type="button" className={cn("dj-beatfx-release", releasing && "is-on")} disabled={Boolean(releaseReason)} aria-pressed={releasing}
      title={releaseReason ?? "RELEASE FX（ECHO OUT）：押している間だけかかり、離すと元に戻ります"}
      onPointerDown={release(true)} onPointerUp={release(false)} onPointerCancel={release(false)} onLostPointerCapture={release(false)} onBlur={() => hold(false)}
      onKeyDown={(event) => { if (!event.repeat && (event.key === " " || event.key === "Enter")) { event.preventDefault(); hold(true); } }}
      onKeyUp={(event) => { if (event.key === " " || event.key === "Enter") hold(false); }}>
      <b>RELEASE FX</b><small>ECHO OUT</small>
    </button>
    {error && <small className="dj-beatfx-error" role="alert">{error}</small>}
  </section>;
}
