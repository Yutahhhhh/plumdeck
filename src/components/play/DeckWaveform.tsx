import { useNativeWaveform } from "./useNativeWaveform";
import { waveformRenderer } from "@/services/waveform/renderer";
import { deckRealtimeStore } from "@/services/dj-engine/deck-realtime-store";
import { assetWaveform } from '@/services/junction/asset-resolver';
import type { AssetWaveform } from '@/types/dj-engine';
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { cn } from "@/lib/utils";
import { useTrackVisuals } from "./useTrackVisuals";
import { useWaveformDetail } from "./useWaveformDetail";
import { createWaveformRenderLoop } from "./waveform-render-loop";
import { waveformPlayhead, type SeekPreview } from "./waveform-playhead";
import { barNumbers, lowerBeat } from "./beat-grid-math";
import { CUE_COLORS } from "./PerformancePads";
import type { LoopRegion, ScratchPhase } from "@/types/dj-engine";

export type WaveformLayout = "horizontal" | "vertical";
type Props = {
  assetId?: string; remoteWaveform?: AssetWaveform;
  /** Content-addressed Junction asset used without loading the real deck. */
  monitorAssetId?: string;
  trackId: number | null; positionMs: number; durationMs: number;
  layout: WaveformLayout; side: "left" | "right"; color: "cyan" | "fuchsia";
  onSeek?: (ms: number) => void; mode?: "overview" | "scroll";
  bpm?: number | null; beatgridOffsetMs?: number; beatsPerBar?: number; hotCues?: (number | null)[];
  label?: string; compact?: boolean; playing?: boolean; rate?: number;
  beatTimesMs?: number[]; beatNumbers?: number[]; gridAvailable?: boolean;
  onGridShift?: (deltaMs: number) => void;
  onScratch?: (command: { phase: ScratchPhase; positionMs: number; gestureId: string; capturedAt?: number; keepalive?: boolean }) => Promise<unknown>;
  onScratchError?: (error: unknown) => void;
  scratching?: boolean;
  loopRegion?: LoopRegion | null;
  /** 曲頭より前に置いた無音の助走（ms）。0 なら助走なし。 */
};
const NO_CUES: (number | null)[] = [];
// rekordbox の 3 バンド波形色。低域＝青、中域＝アンバー、高域＝白。
const WAVE_LOW = "#1a7bf0", WAVE_MID = "#ffa02c", WAVE_HIGH = "#e9f1ff";
// バックエンドが返すのは帯域通過後の「ピーク」なので、そのまま高さにすると
// 音楽的にはずっと小さいはずの高域が中央を白く埋めてしまう。音楽のスペクトル
// は高い方へ傾いて減衰するので、その傾きぶんを表示側で戻して rekordbox と
// 同じ見た目の比率（青が輪郭・橙が胴・白が芯）にする。
const WAVE_TILT_MID = 0.85, WAVE_TILT_HIGH = 0.5;

type DragGesture = {
  lastMotionAt: number; layout: WaveformLayout; pointer: number; coordinate: number; origin: number; moved: boolean;
  kind: "overview" | "grid" | "scratch";
  length: number; span: number; positionMs: number; gestureId: string; errorReported: boolean;
  /** 0 より大きければ、このジェスチャーはスクラッチではなくプリロール調整に切り替わっている。 */
};

export const DeckWaveform = memo(function DeckWaveform({ assetId, remoteWaveform, monitorAssetId, trackId, positionMs, durationMs, layout, side, color, onSeek, mode = "overview", bpm, beatgridOffsetMs = 0, beatsPerBar = 4, hotCues = NO_CUES, label, compact, playing = false, rate = 1, beatTimesMs, beatNumbers, gridAvailable = true, onGridShift, onScratch, onScratchError, scratching = false, loopRegion = null }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const sizeRef = useRef({ width: 0, height: 0 });
  const paintRef = useRef<() => void>(() => undefined);
  const continuousRef = useRef(false);
  const visibleRef = useRef(true);
  const invalidateRef = useRef<() => void>(() => undefined);
  const overviewRaster = useRef<{ key: unknown[]; canvas: HTMLCanvasElement } | null>(null);
  const envelopes = useRef({ low: new Float32Array(0), mid: new Float32Array(0), high: new Float32Array(0) });
  const { data, loading } = useTrackVisuals(mode === "overview" ? trackId : null);
  const detail = useWaveformDetail(mode === "scroll" || mode === "overview" ? trackId : null, compact ? 400 : undefined);
  const [zoomSeconds, setZoomSeconds] = useState(8);
  useEffect(() => {
    const zoom = (event: Event) => {
      const action = (event as CustomEvent<{ control: string; value: number }>).detail;
      if (action.control !== "zoom" || !action.value) return;
      const levels = [0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1, 2, 4, 8, 16, 32];
      setZoomSeconds(current => levels[Math.max(0, Math.min(levels.length - 1, levels.indexOf(current) - Math.sign(action.value)))]);
    };
    if (mode === "scroll") window.addEventListener("plumdeck:controller-library", zoom);
    return () => window.removeEventListener("plumdeck:controller-library", zoom);
  }, [mode]);
  const bars = useMemo(() => barNumbers(beatTimesMs?.length ?? 0, beatNumbers, beatsPerBar), [beatTimesMs, beatNumbers, beatsPerBar]);
  const [dragging, setDragging] = useState(false);
  const drag = useRef<DragGesture | null>(null);
  const releaseDrag = useRef<() => void>(() => undefined);
  const scratchHeartbeat = useRef<ReturnType<typeof setInterval> | null>(null);
  const optimistic = useRef<SeekPreview | null>(null);
  const telemetry = useRef({ position: positionMs, receivedAt: performance.now() });
  const pendingSeek = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastSeekAt = useRef(0);
  const span = mode === "scroll" ? zoomSeconds * 1000 : durationMs;
  const [physicalPixels,setPhysicalPixels]=useState(2000);
  const nativeWaveform = useNativeWaveform(label, positionMs, span, mode === "overview", physicalPixels, monitorAssetId);
  useEffect(()=>{const size=sizeRef.current;setPhysicalPixels((layout==='vertical'?size.height:size.width)*(window.devicePixelRatio||1));},[layout]);
  const remoteBands = useMemo(() => assetWaveform(remoteWaveform), [remoteWaveform]);
  const bands = remoteBands ?? detail.data;
  const peaks = bands ? bands.peaks : mode === "scroll" ? undefined : data?.waveform_peaks;
  const clamp = (position: number) => Math.max(mode === "scroll" ? -60_000 : 0, Math.min(durationMs, position));
  // 表示ゲイン。上位 2% に入る高さをレーンの天井付近へ合わせる。基準を最大値
  // ではなく分位点に置くのは、単発のクリック 1 本で曲全体が縮むのを避けるため。
  // 持ち上げる側だけを許し（下限 1）、大きいマスターを縮めることはしない。
  // 分位点をこれ以上下げると、現代のマスターでは頭打ちが増えて抑揚が消える。
  // peak はビンのピークを描くレーン用、summary は全体波形の RMS 用。描く統計
  // ごとに基準が違うので、同じ規則をそれぞれの分布に当てて別々に求める。
  const gain = useMemo(() => {
    if (!bands) return { peak: 1, summary: 1 };
    const peakLevels = new Uint32Array(256), summaryLevels = new Uint32Array(256);
    const group = 45; // ≒150ms。全体波形で 1 列に収まるビン数の目安。
    let lowSquares = 0, midSquares = 0, highSquares = 0, filled = 0, groups = 0;
    for (let index = 0; index < bands.low.length; index++) {
      const lowBin = bands.low[index], midBin = bands.mid[index] * WAVE_TILT_MID, highBin = bands.high[index] * WAVE_TILT_HIGH;
      const loudest = Math.max(lowBin, midBin, highBin);
      peakLevels[loudest < 0 ? 0 : loudest > 255 ? 255 : loudest | 0]++;
      lowSquares += lowBin * lowBin; midSquares += midBin * midBin; highSquares += highBin * highBin;
      if (++filled === group) {
        const rms = Math.sqrt(Math.max(lowSquares, midSquares, highSquares) / group);
        summaryLevels[rms > 255 ? 255 : rms | 0]++; groups++;
        lowSquares = midSquares = highSquares = filled = 0;
      }
    }
    const ceiling = (levels: Uint32Array, total: number) => {
      let remaining = Math.floor(total * 0.02), value = 255;
      for (; value > 1; value--) {
        remaining -= levels[value];
        if (remaining < 0) break;
      }
      return value;
    };
    const headroom = bands.amplitude_scale * 0.95;
    return {
      peak: Math.min(3, Math.max(1, headroom / ceiling(peakLevels, bands.low.length))),
      summary: groups ? Math.min(6, Math.max(1, headroom / ceiling(summaryLevels, groups))) : 1,
    };
  }, [bands]);

  useEffect(() => {
    const now = performance.now();
    telemetry.current = { position: positionMs, receivedAt: now };
    const preview = optimistic.current;
    if (preview && lastSeekAt.current >= preview.at) {
      const expected = clamp(preview.position + (playing ? (now - preview.at) * rate : 0));
      if (Math.abs(expected - positionMs) < 40) optimistic.current = null;
    }
  }, [positionMs, playing, rate]);
  useEffect(() => {
    releaseDrag.current();
    drag.current = null; optimistic.current = null; setDragging(false);
    telemetry.current = { position: positionMs, receivedAt: performance.now() };
    if (pendingSeek.current) clearTimeout(pendingSeek.current);
    return () => { releaseDrag.current(); if (pendingSeek.current) clearTimeout(pendingSeek.current); };
  }, [trackId,assetId,monitorAssetId]);

  const floor = mode === "scroll" ? Number.NEGATIVE_INFINITY : 0;
  const visualPosition = () => deckRealtimeStore.position(label, performance.now())?.positionMs ?? (drag.current?.kind === "scratch" || scratching
    ? Math.max(floor, Math.min(durationMs, telemetry.current.position))
    : waveformPlayhead(telemetry.current, optimistic.current, performance.now(), playing, rate, durationMs, floor));

  useLayoutEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const draw = () => {
      const { width, height } = sizeRef.current;
      if (width <= 0 || height <= 0) return;
      // Bound backing-store allocation on large/high-DPI displays, not source detail.
      const ratio = Math.min(2, Math.max(1, window.devicePixelRatio || 1), 8192 / Math.max(width, height));
      const pixelWidth = Math.max(1, Math.round(width * ratio)), pixelHeight = Math.max(1, Math.round(height * ratio));
      if (canvas.width !== pixelWidth) canvas.width = pixelWidth;
      if (canvas.height !== pixelHeight) canvas.height = pixelHeight;
      const ctx = canvas.getContext("2d"); if (!ctx || !width || !height) return;
      ctx.setTransform(ratio, 0, 0, ratio, 0, 0); ctx.clearRect(0, 0, width, height);
      // Browser previews sit on striped rows, so only the deck lanes paint a bed.
      if (!compact) { ctx.fillStyle = "#050607"; ctx.fillRect(0, 0, width, height); }
      const vertical = layout === "vertical";
      const length = vertical ? height : width, breadth = vertical ? width : height;
      const axis = vertical ? (side === "left" ? width - 2 : 2) : height / 2;
      const position = visualPosition(), startMs = mode === "scroll" ? position - span / 2 : 0;
      const line = (at: number, stroke: string, lineWidth = 1) => {
        ctx.strokeStyle = stroke; ctx.lineWidth = lineWidth; ctx.beginPath();
        if (vertical) { ctx.moveTo(0, at); ctx.lineTo(width, at); }
        else { ctx.moveTo(at, 0); ctx.lineTo(at, height); }
        ctx.stroke();
      };
      if (!span || !durationMs) return;
      ctx.strokeStyle = compact ? "#26292d" : "#1f2225"; ctx.lineWidth = 1; ctx.beginPath();
      if (vertical) { ctx.moveTo(axis, 0); ctx.lineTo(axis, height); }
      else { ctx.moveTo(0, axis); ctx.lineTo(width, axis); }
      ctx.stroke();

      const rasterKey = [nativeWaveform.tiles, peaks, bands, width, height, ratio, layout, side, compact, color, gain, durationMs];
      const cached = mode === "overview" && overviewRaster.current?.key.every((value, i) => value === rasterKey[i]);
      if (cached) ctx.drawImage(overviewRaster.current!.canvas, 0, 0, width, height);
      if (nativeWaveform.tiles.length && !cached) waveformRenderer.draw(ctx, nativeWaveform.tiles, startMs, span, width, height, vertical, side);
      if (!nativeWaveform.tiles.length && peaks?.length && !cached) {
        const density = bands ? bands.bins_per_second / 1000 : peaks.length / durationMs;
        const scale = bands ? bands.amplitude_scale : 1;
        const columns = Math.max(1, Math.ceil(length * ratio));
        if (envelopes.current.low.length < columns) {
          // Grow in chunks and retain capacity when shrinking a live window.
          const capacity = Math.ceil(columns / 256) * 256;
          envelopes.current = { low: new Float32Array(capacity), mid: new Float32Array(capacity), high: new Float32Array(capacity) };
        }
        const { low, mid, high } = envelopes.current;
        low.fill(0, 0, columns); mid.fill(0, 0, columns); high.fill(0, 0, columns);
        // Keep short overview strips filled while reserving proportional
        // headroom for the beat ruler in larger detail lanes.
        const maxSize = Math.max(0, breadth - (compact ? 2 : Math.min(24, breadth * 0.2))) * (vertical ? 1 : 0.5);
        const lowValues = bands ? bands.low : peaks, midValues = bands?.mid, highValues = bands?.high;
        const bins = lowValues.length, step = span / columns;
        const perColumn = step * density;
        // 1 列に 1 ビンも入らないズーム域では、最大値だけ拾うと稜線が階段になる。
        // そこは隣接ビンを線形補間して、rekordbox のように滑らかな輪郭にする。
        const interpolate = perColumn < 1;
        // 逆に 1 列が数十ビンをまたぐ全体波形では、最大値は「曲のどこかで一度
        // 鳴った音」に収束し、どの帯域も飽和して抑揚も色の差も消える。そこは
        // 実効値（RMS）に切り替えて、曲の起伏と帯域バランスが出るようにする。
        const summarise = perColumn > 8;
        const level = summarise ? gain.summary : gain.peak;
        for (let index = 0; index < columns; index++) {
          const time = startMs + index * step;
          if (time < 0 || time >= durationMs) continue;
          let lowBand = 0, midBand = 0, highBand = 0;
          if (interpolate) {
            const exact = time * density;
            const first = Math.min(bins - 1, Math.max(0, Math.floor(exact)));
            const next = Math.min(bins - 1, first + 1), blend = exact - first;
            lowBand = lowValues[first] + (lowValues[next] - lowValues[first]) * blend;
            if (midValues && highValues) {
              midBand = midValues[first] + (midValues[next] - midValues[first]) * blend;
              highBand = highValues[first] + (highValues[next] - highValues[first]) * blend;
            }
          } else {
            const first = Math.max(0, Math.floor(time * density));
            const end = Math.min(bins, Math.max(first + 1, Math.ceil((time + step) * density)));
            for (let bin = first; bin < end; bin++) {
              const lowBin = lowValues[bin], midBin = midValues?.[bin] ?? 0, highBin = highValues?.[bin] ?? 0;
              if (summarise) { lowBand += lowBin * lowBin; midBand += midBin * midBin; highBand += highBin * highBin; }
              else {
                if (lowBin > lowBand) lowBand = lowBin;
                if (midBin > midBand) midBand = midBin;
                if (highBin > highBand) highBand = highBin;
              }
            }
            if (summarise) {
              const count = end - first;
              lowBand = Math.sqrt(lowBand / count); midBand = Math.sqrt(midBand / count); highBand = Math.sqrt(highBand / count);
            }
          }
          low[index] = Math.min(1, lowBand / scale * level) * maxSize;
          mid[index] = Math.min(1, midBand / scale * level * WAVE_TILT_MID) * maxSize;
          high[index] = Math.min(1, highBand / scale * level * WAVE_TILT_HIGH) * maxSize;
        }
        const fillBand = (values: Float32Array, fill: string) => {
          ctx.fillStyle = fill; ctx.beginPath();
          const unit = length / columns;
          // 列ごとに垂直な辺で立ち上げる。隣の列と斜めに結ぶとキックの立ち上がりが
          // 鈍り、rekordbox のような切れ味のある輪郭にならない。
          const edge = (index: number, sign: number, back: boolean) => {
            const from = index * unit, size = values[index] * sign;
            const head = back ? from + unit : from, tail = back ? from : from + unit;
            if (vertical) { ctx.lineTo(axis + size, head); ctx.lineTo(axis + size, tail); }
            else { ctx.lineTo(head, axis + size); ctx.lineTo(tail, axis + size); }
          };
          const front = vertical ? (side === "left" ? -1 : 1) : -1;
          if (vertical) ctx.moveTo(axis, 0); else ctx.moveTo(0, axis);
          for (let index = 0; index < columns; index++) edge(index, front, false);
          if (vertical) ctx.lineTo(axis, length);
          else for (let index = columns - 1; index >= 0; index--) edge(index, 1, true);
          ctx.closePath(); ctx.fill();
        };
        // rekordbox 準拠：低域＝青／中域＝橙／高域＝白を、それぞれ独立した高さで
        // 重ねる。以前は全帯域の輪郭を帯域比で塗り分けていたので 3 色が同じ
        // シルエットを共有し、キックもハイハットも同じ形に潰れていた。
        if (bands) { fillBand(low, WAVE_LOW); fillBand(mid, WAVE_MID); fillBand(high, WAVE_HIGH); }
        else fillBand(low, color === "cyan" ? WAVE_LOW : "#ffac48");
      }
      if (mode === "overview" && !cached) {
        const raster = document.createElement("canvas");
        raster.width = pixelWidth; raster.height = pixelHeight;
        raster.getContext("2d")?.drawImage(canvas, 0, 0);
        overviewRaster.current = { key: rasterKey, canvas: raster };
      }
      // Overview: dim the part already played so the remaining time reads at a glance.
      if (mode === "overview" && !compact && position > 0) {
        const played = Math.max(0, Math.min(length, position / durationMs * length));
        ctx.fillStyle = "#05060755";
        if (vertical) ctx.fillRect(0, 0, width, played); else ctx.fillRect(0, 0, played, height);
      }
      // 曲頭より前に置いた無音の助走。ここにはビートグリッドを描かない
      // （拍を増やすのではなく、再生開始位置を手前に置くだけ）。
      if (mode === "scroll" && startMs < 0) {
        const toOffset = (time: number) => (time - startMs) / span * length;
        const from = 0, to = Math.min(length, toOffset(0));
        if (to > 0 && from < length && to > from) {
          ctx.fillStyle = "#7f88941f";
          if (vertical) ctx.fillRect(0, from, width, to - from); else ctx.fillRect(from, 0, to - from, height);
          line(to, "#9fb0c4", 1);
          if (!compact) {
            const label = "曲頭前 · 無音";
            ctx.font = "bold 9px ui-monospace, monospace";
            ctx.fillStyle = "#a8b6c6";
            ctx.textBaseline = "top";
            if (vertical) ctx.fillText(label, side === "left" ? 4 : Math.max(4, width - 4 - ctx.measureText(label).width), Math.max(2, from + 3));
            else ctx.fillText(label, Math.max(2, Math.min(width - 4 - ctx.measureText(label).width, from + 4)), height - 13);
            ctx.textBaseline = "alphabetic";
          }
        }
      }
      // オートビートループ／マニュアルループの範囲。波形の上に帯を敷き、
      // IN / OUT を縦線で示す。無効化中のループも薄く残して復帰先を見せる。
      if (loopRegion && loopRegion.endMs > loopRegion.startMs) {
        const on = loopRegion.enabled;
        const toOffset = (time: number) => (time - startMs) / span * length;
        const from = Math.max(0, toOffset(loopRegion.startMs)), to = Math.min(length, toOffset(loopRegion.endMs));
        if (to > 0 && from < length) {
          ctx.fillStyle = on ? "#f0a94a2e" : "#f0a94a14";
          // 全体波形では 4 拍が 1px 未満になることがあるので、帯は最低 2px 残す。
          const thickness = Math.max(2, to - from);
          if (vertical) ctx.fillRect(0, from, width, thickness); else ctx.fillRect(from, 0, thickness, height);
          const edge = on ? "#f5b95c" : "#f5b95c66";
          if(!on)ctx.setLineDash([3,3]);
          if (from >= 0) line(from, edge, on ? 2 : 1);
          if (to <= length) line(to, edge, on ? 2 : 1);
          ctx.setLineDash([]);
          if (!compact) {
            const beatMs = bpm && bpm > 0 ? 60_000 / bpm : 0;
            const beats = beatMs ? (loopRegion.endMs - loopRegion.startMs) / beatMs : 0;
            const label = beats
              ? beats >= beatsPerBar
                ? `${Number((beats / beatsPerBar).toFixed(2))} Bars`
                : `${Number(beats.toFixed(2))} Beats`
              : `${((loopRegion.endMs - loopRegion.startMs) / 1000).toFixed(2)}s`;
            ctx.font = "bold 10px ui-monospace, monospace";
            ctx.fillStyle = on ? "#ffd08a" : "#ffd08a80";
            ctx.textBaseline = "top";
            // 左端に寄せるときはデッキ名バッジ（HTML）とぶつかるので、その幅ぶん逃がす。
            const badge = 22;
            if (vertical) ctx.fillText(label, side === "left" ? badge : Math.max(4, width - 4 - ctx.measureText(label).width), Math.max(2, Math.min(height - 12, from + 3)));
            else ctx.fillText(label, Math.max(badge, Math.min(width - 4 - ctx.measureText(label).width, from + 4)), 2);
            ctx.textBaseline = "alphabetic";
          }
        }
      }
      if (mode === "scroll" && gridAvailable && (beatTimesMs?.length || bpm && bpm > 0)) {
        const beatMs = bpm ? 60_000 / bpm : 0;
        const exact = Boolean(beatTimesMs?.length);
        const first = exact ? lowerBeat(beatTimesMs!, Math.max(0, startMs)) : Math.ceil((Math.max(0, startMs) - beatgridOffsetMs) / beatMs);
        const last = exact ? lowerBeat(beatTimesMs!, Math.min(durationMs, startMs + span)) - 1 : Math.floor((Math.min(durationMs, startMs + span) - beatgridOffsetMs) / beatMs);
        let lastBarLabel=-Infinity;
        for (let beat = first; beat <= last && beat - first < 512; beat++) {
          const at = ((exact ? beatTimesMs![beat] : beat * beatMs + beatgridOffsetMs) - startMs) / span * length;
          const downbeat = exact ? (beatNumbers?.[beat] ?? beat % beatsPerBar + 1) === 1 : beat % beatsPerBar === 0;
          // rekordbox と同じく、拍は淡いシアン、小節頭は赤で通す。波形の上に
          // 乗せるので、トランジェントを隠さない程度の濃さに留める。
          line(at, downbeat ? "#ff4152a8" : "#8fdcff2e", 1);
          const tick = downbeat ? 9 : 5;
          ctx.fillStyle = downbeat ? "#ff5364" : "#9fdcf5";
          if (vertical) {
            ctx.fillRect(side === "left" ? 0 : width - tick, at, tick, 1);
          } else {
            ctx.fillRect(at, 0, 1, tick);
            ctx.fillRect(at, height - tick, 1, tick);
          }
          if (downbeat) {
            ctx.fillStyle = "#ff424f";
            ctx.beginPath();
            ctx.arc(vertical ? (side === "left" ? width - 4 : 4) : at, vertical ? at : 4, 3, 0, Math.PI * 2);
            ctx.fill();
            ctx.font = "bold 10px ui-monospace, monospace"; ctx.fillStyle = "#dce6f1";
            const bar = exact ? bars[beat] : Math.floor(beat / beatsPerBar) + 1;
            if(at-lastBarLabel>=32){
              if (vertical) ctx.fillText(String(bar), side === "left" ? 3 : width - 24, at - 3);
              else ctx.fillText(String(bar), at + 4, height-2);
              lastBarLabel=at;
            }
          }
        }
      }
      const cueGroups:{at:number;indices:number[]}[]=[];
      hotCues.map((time,index)=>({time,index})).filter(item=>item.time!==null&&item.time>=startMs&&item.time<=startMs+span).sort((a,b)=>a.time!-b.time!).forEach(({time,index})=>{
        const at=(time!-startMs)/span*length,last=cueGroups[cueGroups.length-1];
        if(last&&at-last.at<22)last.indices.push(index);else cueGroups.push({at,indices:[index]});
      });
      hotCues.forEach((time, index) => {
        if (time === null || time < startMs || time > startMs + span) return;
        const at = (time - startMs) / span * length;
        const cueColor = CUE_COLORS[index % CUE_COLORS.length];
        line(at, cueColor, 1.5);
        const group=cueGroups.find(group=>group.indices[0]===index);if(!group)return;
        const letter = group.indices.length>1?`${String.fromCharCode(65+index)}+${group.indices.length-1}`:String.fromCharCode(65 + index);
        const tab = group.indices.length>1?28:16;
        ctx.fillStyle = cueColor;
        ctx.font = `bold 10px ui-monospace, monospace`;
        ctx.textBaseline = "middle";
        ctx.textAlign = "center";
        // ラベルはキュー位置を中心に置く。先頭を起点にすると重心が半分ずれて、
        // 線とラベルが食い違って見える。端では画面内に収まるよう寄せるが、
        // 位置を示す線そのものは動かさない。
        const boxStart = Math.max(0, Math.min(length - tab, at - tab / 2));
        if (vertical) {
          const left = side === "left" ? width - tab : 0;
          ctx.fillRect(left, boxStart, tab, tab);
          ctx.fillStyle = "#0d1014";
          ctx.fillText(letter, left + tab / 2, boxStart + tab / 2 + 0.5);
        } else {
          ctx.fillRect(boxStart, 0, tab, tab);
          ctx.fillStyle = "#0d1014";
          ctx.fillText(letter, boxStart + tab / 2, tab / 2 + 0.5);
        }
        ctx.textAlign = "start";
        ctx.textBaseline = "alphabetic";
      });
      if (onSeek || mode === "scroll") {
        const cursor = mode === "scroll" ? length / 2 : Math.max(0, Math.min(length - 1, position / durationMs * length));
        // A dark keyline separates the playhead even from white high frequencies.
        line(cursor, "#000000cc", compact ? 3 : 5);
        line(cursor, dragging ? "#ffcf80" : "#ffffff", compact ? 1 : 2);
        ctx.fillStyle = dragging ? "#ffcf80" : "#ff5364";
        const marker = compact ? 3 : 5;
        ctx.beginPath();
        if (vertical) {
          ctx.moveTo(0, cursor - marker); ctx.lineTo(marker + 2, cursor); ctx.lineTo(0, cursor + marker);
          ctx.moveTo(width, cursor - marker); ctx.lineTo(width - marker - 2, cursor); ctx.lineTo(width, cursor + marker);
        } else {
          ctx.moveTo(cursor - marker, 0); ctx.lineTo(cursor, marker + 2); ctx.lineTo(cursor + marker, 0);
          ctx.moveTo(cursor - marker, height); ctx.lineTo(cursor, height - marker - 2); ctx.lineTo(cursor + marker, height);
        }
        ctx.fill();
      }
    };
    paintRef.current = draw;
    continuousRef.current = !compact && (trackId !== null || Boolean(assetId)) && (playing || dragging);
    invalidateRef.current();
  });

  useEffect(() => {
    const canvas = canvasRef.current;
    const container = canvas?.parentElement;
    if (!canvas || !container) return;
    const loop = createWaveformRenderLoop(() => paintRef.current(), () => visibleRef.current && continuousRef.current && !document.hidden);
    invalidateRef.current = loop.invalidate;
    // Observe the CSS container, never the canvas whose backing size we mutate.
    // No layout reads/writes or rasterization inside ResizeObserver delivery.
    const observer = new ResizeObserver(([entry]) => {
      sizeRef.current = { width: entry.contentRect.width, height: entry.contentRect.height };
      setPhysicalPixels((layout === "vertical" ? entry.contentRect.height : entry.contentRect.width)*(window.devicePixelRatio||1));
      loop.invalidate();
    });
    observer.observe(container);
    const visibility = new IntersectionObserver(([entry]) => {
      visibleRef.current = entry.isIntersecting;
      if (entry.isIntersecting) loop.invalidate();
    });
    visibility.observe(container);
    window.addEventListener("resize", loop.invalidate);
    document.addEventListener("visibilitychange", loop.invalidate);
    return () => {
      visibility.disconnect(); observer.disconnect(); loop.dispose(); invalidateRef.current = () => undefined;
      window.removeEventListener("resize", loop.invalidate);
      document.removeEventListener("visibilitychange", loop.invalidate);
    };
  }, [layout]);

  const seekLive = (position: number, final = false) => {
    const now = performance.now();
    optimistic.current = { position, at: now, until: now + 600 };
    invalidateRef.current();
    if (pendingSeek.current) { clearTimeout(pendingSeek.current); pendingSeek.current = null; }
    const send = () => { lastSeekAt.current = performance.now(); onSeek?.(position); pendingSeek.current = null; };
    const remaining = 40 - (performance.now() - lastSeekAt.current);
    if (final || remaining <= 0) send(); else pendingSeek.current = setTimeout(send, remaining);
  };

  const reportScratch = (gesture: DragGesture, error: unknown) => {
    if (gesture.errorReported) return;
    gesture.errorReported = true;
    onScratchError?.(error);
  };
  const sendScratch = (gesture: DragGesture, phase: ScratchPhase, capturedAt = performance.now(), keepalive = false) => {
    void onScratch?.({ phase, positionMs: gesture.positionMs, gestureId: gesture.gestureId, capturedAt, keepalive })
      .catch((error) => reportScratch(gesture, error));
  };
  const stopHeartbeat = () => {
    if (scratchHeartbeat.current) clearInterval(scratchHeartbeat.current);
    scratchHeartbeat.current = null;
  };
  releaseDrag.current = () => {
    const gesture = drag.current;
    if (!gesture) return;
    drag.current = null;
    stopHeartbeat();
    if (gesture.kind === "scratch") sendScratch(gesture, "end");
    else if (gesture.kind === "overview" && gesture.moved) seekLive(visualPosition(), true);
    setDragging(false);
    invalidateRef.current();
  };

  useEffect(() => {
    const release = () => releaseDrag.current();
    const releaseWhenHidden = () => { if (document.hidden) release(); };
    window.addEventListener("blur", release);
    document.addEventListener("visibilitychange", releaseWhenHidden);
    return () => {
      release();
      window.removeEventListener("blur", release);
      document.removeEventListener("visibilitychange", releaseWhenHidden);
    };
  }, []);

  const interactive = Boolean(onSeek || mode === "scroll" && onScratch);

  return <div className={cn("dj-waveform", `dj-waveform--${layout}`, compact && "dj-waveform--compact")}>
    <canvas ref={canvasRef} style={{ touchAction: interactive ? "none" : undefined, cursor: interactive ? dragging ? "grabbing" : mode === "scroll" ? "grab" : "crosshair" : undefined }} aria-label={label ? `Deck ${label} ${mode} waveform` : "Track waveform"} role={interactive ? "slider" : "img"} tabIndex={interactive && durationMs ? 0 : undefined} aria-valuemin={interactive ? mode === "scroll" ? Math.min(-60_000, positionMs) : 0 : undefined} aria-valuemax={interactive ? durationMs : undefined} aria-valuenow={interactive ? mode === "overview" ? Math.max(0, positionMs) : positionMs : undefined}
      onKeyDown={(event) => { if (!onSeek || !durationMs || !["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) return; event.preventDefault(); seekLive(clamp(visualPosition() + (["ArrowLeft", "ArrowUp"].includes(event.key) ? -1 : 1) * (event.shiftKey ? 10 : 1000)), true); }}
      onPointerDown={(event) => {
        if (!interactive || !durationMs || event.button !== 0) return;
        const grid = mode === "scroll" && Boolean(onGridShift && !event.altKey);
        if (mode === "scroll" && !grid && !onScratch) return;
        event.preventDefault(); event.currentTarget.setPointerCapture(event.pointerId);
        const coordinate = layout === "horizontal" ? event.clientX : event.clientY;
        const rect = event.currentTarget.getBoundingClientRect();
        const origin = layout === "horizontal" ? rect.left : rect.top;
        const length = layout === "horizontal" ? rect.width : rect.height;
        const kind: DragGesture["kind"] = mode === "overview" ? "overview" : grid ? "grid" : "scratch";
        const gesture: DragGesture = { lastMotionAt: performance.now(), layout, pointer: event.pointerId, coordinate, origin, moved: mode === "overview", kind, length, span, positionMs: 0, gestureId: crypto.randomUUID(), errorReported: false };
        drag.current = gesture; setDragging(true);
        if (kind === "grid") onGridShift?.(0);
        else if (kind === "overview") seekLive(clamp((coordinate - origin) / length * durationMs), true);
        else {
          optimistic.current = null;
          sendScratch(gesture, "begin");
          scratchHeartbeat.current = setInterval(() => {
            if (drag.current === gesture && performance.now() - gesture.lastMotionAt >= 250) sendScratch(gesture, "move", performance.now(), true);
          }, 250);
        }
      }}
      onPointerMove={(event) => {
        const gesture = drag.current; if (!gesture || gesture.pointer !== event.pointerId) return;
        const samples = event.nativeEvent.getCoalescedEvents?.() ?? [];
        if (!samples.length) samples.push(event.nativeEvent);
        for (const sample of samples) {
        const coordinate = gesture.layout === "horizontal" ? sample.clientX : sample.clientY;
        if (gesture.length <= 0 || coordinate === gesture.coordinate) continue;
        const delta = coordinate - gesture.coordinate;
        gesture.coordinate = coordinate;
        if (gesture.kind === "grid") { onGridShift?.(delta / gesture.length * gesture.span * (sample.shiftKey ? 0.1 : 1)); continue; }
        if (gesture.kind === "scratch") {
          gesture.positionMs = Math.max(-60_000, Math.min(60_000, gesture.positionMs - delta / gesture.length * gesture.span));
          gesture.lastMotionAt = performance.now();
          gesture.moved = true; sendScratch(gesture, "move", sample.timeStamp); continue;
        }
        gesture.moved = true;
        seekLive(clamp((coordinate - gesture.origin) / gesture.length * durationMs));
        }
      }}
      onPointerUp={(event) => {
        const gesture = drag.current; if (!gesture || gesture.pointer !== event.pointerId) return;
        releaseDrag.current();
        if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
      }}
      onLostPointerCapture={() => releaseDrag.current()}
      onPointerCancel={() => releaseDrag.current()} />
    {dragging&&<span className="dj-wave-grip" aria-hidden="true">⋮⋮</span>}
    {label && <span title={hotCues.map((time,i)=>time===null?null:`${String.fromCharCode(65+i)}: ${(time/1000).toFixed(3)}s`).filter(Boolean).join(" / ")} className={cn("dj-wave-label", color === "cyan" ? "dj-blue" : "dj-orange")}>{label}</span>}
    {mode === "scroll" && <select className="dj-wave-zoom" aria-label={`Deck ${label ?? side} waveform zoom`} title="表示範囲 / 青:低域・橙:中域・白:高域" value={zoomSeconds} onChange={(event) => setZoomSeconds(Number(event.target.value))}>{[0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1, 2, 4, 8, 16, 32].map((seconds) => <option key={seconds} value={seconds}>{seconds}s</option>)}</select>}
    {nativeWaveform.error&&<button className="dj-wave-retry" onClick={nativeWaveform.retry} title={nativeWaveform.error}>波形を再試行</button>}
    {!peaks?.length && !nativeWaveform.tiles.length && <span className="dj-wave-empty" style={{ pointerEvents: detail.error ? "auto" : "none" }}>
      {(mode === "scroll" ? detail.loading : loading) ? "Loading waveform…" : detail.error ? <button onClick={detail.retry} title={detail.error}>詳細波形を再試行</button> : assetId ? "共有音源の波形を準備中" : trackId ? "波形未解析" : compact ? "—" : "No track loaded"}
    </span>}
  </div>;
});
