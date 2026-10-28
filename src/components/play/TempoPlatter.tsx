import { deckRealtimeStore } from "@/services/dj-engine/deck-realtime-store";
import { createWaveformRenderLoop } from "./waveform-render-loop";
import { useEffect, useRef, useState, type PointerEvent } from "react";
import { cn } from "@/lib/utils";
import { formatBpm, formatTempoPercent, rangeLabel, stepTempo, tempoStep, TEMPO_RANGES, type TempoRange } from "./tempo-math";

export { TEMPO_RANGES, rangeLabel, type TempoRange } from "./tempo-math";

/** 一周の実時間。33⅓ 回転のレコードに合わせて 1.8 秒で一回転させる。 */
const REVOLUTION_MS = 1800;

type Props = {
  deckId: string;
  /** 再生中の実効 BPM。未解析なら null。 */
  bpm: number | null;
  /** 楽曲のオリジナル BPM。これが無いと BPM 直操作はできない。 */
  trackBpm: number | null;
  rate: number;
  positionMs: number;
  playing: boolean;
  disabled: boolean;
  range: TempoRange;
  onRange: (range: TempoRange) => void;
  onTempo: (rate: number) => void;
  /** Hold to bend: −1 / +1 while pressed, 0 on release. Hidden without the capability. */
  onBend?: (direction: -1 | 0 | 1) => void;
};

/**
 * rekordbox のデッキ中央にある円形表示。外周の白いリングは切れ目（赤マーカー）
 * ごと回転し、ターンテーブルの回転インジケーターとして働く。中央は BPM と、
 * オリジナル BPM からの変化率、テンポレンジ。
 *
 * 操作は実機に合わせている：
 *  - BPM を上下にドラッグして変更（実機のツールチップ「クリックしたまま上下に
 *    スライドしてBPM値を変更することができます」）
 *  - % をダブルクリックで 0% に戻す
 *  - レンジ表示のクリックで ±6 / ±10 / ±16 / WIDE を切り換え
 *  - ホバーで現れる − / + で微調整
 */
export function TempoPlatter(props: Props) {
  const { rate, trackBpm, range, disabled } = props;
  const percent = (rate - 1) * 100;
  const displayBpm = props.bpm ?? (trackBpm === null ? null : trackBpm * rate);
  const canSetTempo = !disabled && trackBpm !== null && trackBpm > 0;
  const [drag, setDrag] = useState(false);

  const ring = useRef<SVGGElement>(null);
  const spin = useRef({ positionMs: props.positionMs, at: performance.now() });
  const state = useRef(props);
  const invalidate = useRef(() => {});
  state.current = props;
  useEffect(() => {
    spin.current = { positionMs: props.positionMs, at: performance.now() };
    invalidate.current();
  }, [props.positionMs, props.playing, rate]);
  useEffect(() => {
    const loop = createWaveformRenderLoop(() => {
      const elapsed = state.current.playing ? Math.min(200, Math.max(0, performance.now() - spin.current.at)) * state.current.rate : 0;
      const angle = (deckRealtimeStore.position(state.current.deckId, performance.now())?.positionMs ?? (spin.current.positionMs + elapsed)) / REVOLUTION_MS * 360 % 360;
      ring.current?.setAttribute("transform", `rotate(${angle} 50 50)`);
    }, () => state.current.playing && !document.hidden);
    invalidate.current = loop.invalidate;
    document.addEventListener("visibilitychange", loop.invalidate);
    loop.invalidate();
    return () => { loop.dispose(); document.removeEventListener("visibilitychange", loop.invalidate); };
  }, []);

  const applyPercent = (next: number) => {
    const clamped = Math.max(-range, Math.min(range, next));
    props.onTempo(1 + clamped / 100);
  };

  // 実機と同じく、掴んだ位置からの縦移動でテンポを動かす。
  const startDrag = (event: PointerEvent<HTMLDivElement>) => {
    if (!canSetTempo || event.button !== 0) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    const originY = event.clientY;
    const originPercent = percent;
    setDrag(true);
    const move = (moveEvent: globalThis.PointerEvent) => {
      // 上へ動かすと速くなる。細かい調整のため 1px = 0.05%。
      const step = moveEvent.shiftKey ? 0.01 : 0.05;
      applyPercent(originPercent + (originY - moveEvent.clientY) * step);
    };
    const stop = () => {
      setDrag(false);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
  };

  // ± move by the range's resolution (0.02 / 0.04 / 0.04 / 0.5 %).
  const nudge = (steps: number) => props.onTempo(stepTempo(rate, steps, range));
  const step = tempoStep(range);
  const bend = (direction: -1 | 1) => ({
    onPointerDown: (event: PointerEvent<HTMLButtonElement>) => { if (event.button === 0) { event.currentTarget.setPointerCapture(event.pointerId); props.onBend?.(direction); } },
    onPointerUp: () => props.onBend?.(0), onPointerCancel: () => props.onBend?.(0), onLostPointerCapture: () => props.onBend?.(0),
  });

  return <div className={cn("dj-platter", !props.playing && "dj-platter--idle", drag && "is-dragging")}
    role="group" aria-label={`Deck ${props.deckId} テンポ`}>
    <svg className="dj-platter-ring" viewBox="0 0 100 100" aria-hidden>
      {/* 切れ目のある白いリングと赤マーカーをまとめて回す。 */}
      <g ref={ring}>
        <circle className="dj-platter-ring-track" cx="50" cy="50" r="45"
          strokeDasharray="272 11" strokeDashoffset="6" />
        <rect className="dj-platter-index" x="47" y="90" width="6" height="6" rx="1" />
      </g>
      <circle className="dj-platter-ring-inner" cx="50" cy="50" r="38" />
    </svg>

    <div className="dj-platter-face">
      <div className={cn("dj-platter-bpm", canSetTempo && "is-adjustable")}
        role={canSetTempo ? "slider" : undefined}
        aria-label={canSetTempo ? `Deck ${props.deckId} BPM` : undefined}
        aria-valuenow={displayBpm ?? undefined}
        aria-valuemin={trackBpm === null ? undefined : trackBpm * (1 - range / 100)}
        aria-valuemax={trackBpm === null ? undefined : trackBpm * (1 + range / 100)}
        tabIndex={canSetTempo ? 0 : undefined}
        title={canSetTempo ? "クリックしたまま上下にスライドしてBPMを変更（Shiftで微調整）" : "BPM 未解析"}
        onPointerDown={startDrag}
        onKeyDown={(event) => {
          if (!canSetTempo) return;
          if (event.key === "ArrowUp") { event.preventDefault(); nudge(event.shiftKey ? 1 : 5); }
          if (event.key === "ArrowDown") { event.preventDefault(); nudge(event.shiftKey ? -1 : -5); }
        }}>
        {displayBpm === null
          ? <strong>—</strong>
          : <><strong>{formatBpm(displayBpm)?.whole}</strong><small>.{formatBpm(displayBpm)?.fraction}</small></>}
      </div>

      <div className="dj-platter-tempo">
        <button type="button" className="dj-platter-percent" disabled={!canSetTempo}
          title="ダブルクリックで 0% に戻す"
          onDoubleClick={() => canSetTempo && props.onTempo(1)}>
          {formatTempoPercent(rate, range)}
        </button>
        <button type="button" className="dj-platter-range" disabled={disabled}
          title="テンポレンジ：再生速度を調整できる範囲。クリックで変更できます"
          onClick={() => props.onRange(TEMPO_RANGES[(TEMPO_RANGES.indexOf(range) + 1) % TEMPO_RANGES.length])}>
          {rangeLabel(range)}
        </button>
      </div>
    </div>

    <div className="dj-platter-nudge">
      <button type="button" disabled={!canSetTempo} aria-label="テンポを下げる" title={`テンポを ${step}% 下げる`} onClick={() => nudge(-1)}>−</button>
      <button type="button" disabled={!canSetTempo} aria-label="テンポを上げる" title={`テンポを ${step}% 上げる`} onClick={() => nudge(1)}>＋</button>
    </div>
    {props.onBend && <div className="dj-platter-bend" role="group" aria-label={`Deck ${props.deckId} ピッチベンド`}>
      <button type="button" disabled={disabled} aria-label="ピッチベンド（遅く）" title="押している間だけ遅くして位相を合わせます" {...bend(-1)}>‹</button>
      <button type="button" disabled={disabled} aria-label="ピッチベンド（速く）" title="押している間だけ速くして位相を合わせます" {...bend(1)}>›</button>
    </div>}
  </div>;
}
