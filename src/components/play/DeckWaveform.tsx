import { useNativeWaveform } from "./useNativeWaveform";
import { waveformRenderer } from "@/services/waveform/renderer";
import { deckRealtimeStore } from "@/services/dj-engine/deck-realtime-store";
import { classifyRelease, MotionHistory } from "@/services/dj-engine/scratch-release";
import { assetWaveform } from '@/services/junction/asset-resolver';
import type { AssetWaveform } from '@/types/dj-engine';
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { cn } from "@/lib/utils";
import { useThreeBand } from "./useThreeBand";
import { drawThreeBand, fromBandPeaks, PHRASE_COLORS, ThreeBandTiles, type Phrase } from "@/services/waveform/three-band";
import { laneSpanMs, stepWaveformZoom, useWaveformZoom } from "./waveform-zoom";
import type { MemoryCue } from "@/services/dj-engine/memory-cues";
import { createWaveformRenderLoop } from "./waveform-render-loop";
import { waveformPlayhead, type SeekPreview } from "./waveform-playhead";
import { barNumbers, lowerBeat } from "./beat-grid-math";
import { hotCueColor } from "@/services/midi/pad-colors";
import type { LoopRegion, ScratchPhase } from "@/types/dj-engine";

export type WaveformLayout = "horizontal" | "vertical";
type Props = {
  assetId?: string; remoteWaveform?: AssetWaveform;
  trackId: number | null; positionMs: number; durationMs: number;
  layout: WaveformLayout; side: "left" | "right"; color: "cyan" | "fuchsia";
  onSeek?: (ms: number) => void; mode?: "overview" | "scroll";
  bpm?: number | null; beatgridOffsetMs?: number; beatsPerBar?: number; hotCues?: (number | null)[];
  label?: string; compact?: boolean; playing?: boolean; rate?: number;
  beatTimesMs?: number[]; beatNumbers?: number[]; gridAvailable?: boolean;
  onGridShift?: (deltaMs: number) => void;
  onScratch?: (command: { phase: ScratchPhase; positionMs: number; gestureId: string; capturedAt?: number; keepalive?: boolean }) => Promise<unknown>;
  onScratchError?: (error: unknown) => void;
  /** 手を離した瞬間にバックスピンと判定したときだけ呼ぶ。end は惰性の後に呼び出し側が送る。 */
  onBackspin?: (release: { gestureId: string; positionMs: number; velocity: number }) => void;
  /** 惰性で回っているジェスチャーを掴み直す。回っていなければ null。 */
  onScratchGrab?: () => { gestureId: string; positionMs: number } | null;
  scratching?: boolean;
  loopRegion?: LoopRegion | null;
  /** Manual loop IN waiting for OUT: drawn as a dotted line. */
  pendingLoopInMs?: number | null;
  /** 曲頭より前に置いた無音の助走（ms）。0 なら助走なし。 */
  /** Hot cue colours by slot (the pads' source). */
  cueColors?: (string | null)[];
  memoryCues?: readonly MemoryCue[];
  /** rekordbox phrases on the playback timeline. */
  phrases?: readonly Phrase[];
  /** Phrase names: always, or while the pointer is over the lane. */
  phraseLabels?: "always" | "hover";
};
const NO_CUES: (number | null)[] = [];
const NO_MEMORY: readonly MemoryCue[] = [];
const NO_PHRASES: readonly Phrase[] = [];
/** Stable, so the overview raster cache key matches between frames. */
const NO_TILES: never[] = [];
/** Phrase strip thickness: 2px on compact lanes, up to 3px on tall ones. */
const phraseStrip = (breadth: number) => Math.max(2, Math.min(3, Math.round(breadth / 28)));

type DragGesture = {
  lastMotionAt: number; layout: WaveformLayout; pointer: number; coordinate: number; origin: number; moved: boolean;
  kind: "overview" | "grid" | "scratch";
  length: number; span: number; positionMs: number; gestureId: string; errorReported: boolean;
  /** スクラッチ中のポインター履歴。離した瞬間のバックスピン判定に使う。 */
  motion?: MotionHistory;
  /** 0 より大きければ、このジェスチャーはスクラッチではなくプリロール調整に切り替わっている。 */
};

export const DeckWaveform = memo(function DeckWaveform({ assetId, remoteWaveform, trackId, positionMs, durationMs, layout, side, color, onSeek, mode = "overview", bpm, beatgridOffsetMs = 0, beatsPerBar = 4, hotCues = NO_CUES, label, compact, playing = false, rate = 1, beatTimesMs, beatNumbers, gridAvailable = true, onGridShift, onScratch, onScratchError, onBackspin, onScratchGrab, scratching = false, loopRegion = null, pendingLoopInMs = null, cueColors, memoryCues = NO_MEMORY, phrases = NO_PHRASES, phraseLabels = "hover" }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const sizeRef = useRef({ width: 0, height: 0 });
  const paintRef = useRef<() => void>(() => undefined);
  const continuousRef = useRef(false);
  const detailTiles = useRef(new ThreeBandTiles());
  const visibleRef = useRef(true);
  const invalidateRef = useRef<() => void>(() => undefined);
  const overviewRaster = useRef<{ key: unknown[]; canvas: HTMLCanvasElement } | null>(null);
  const hovering = useRef(false);
  const local = useThreeBand(trackId, mode === "scroll" ? "detail" : "overview");
  const zoomSeconds = useWaveformZoom();
  useEffect(() => {
    const zoom = (event: Event) => {
      const action = (event as CustomEvent<{ control: string; value: number }>).detail;
      if (action.control !== "zoom" || !action.value) return;
      stepWaveformZoom(action.value > 0 ? 1 : -1);
    };
    // One listener is enough for the shared zoom: deck A's lane takes it.
    if (mode === "scroll" && label === "A") window.addEventListener("plumdeck:controller-library", zoom);
    return () => window.removeEventListener("plumdeck:controller-library", zoom);
  }, [mode, label]);
  const bars = useMemo(() => barNumbers(beatTimesMs?.length ?? 0, beatNumbers, beatsPerBar), [beatTimesMs, beatNumbers, beatsPerBar]);
  const [dragging, setDragging] = useState(false);
  const drag = useRef<DragGesture | null>(null);
  /** thrown は指を離したことによる解放。フォーカス喪失や曲の入れ替えでは惰性を付けない。 */
  const releaseDrag = useRef<(thrown?: boolean) => void>(() => undefined);
  const scratchHeartbeat = useRef<ReturnType<typeof setInterval> | null>(null);
  const optimistic = useRef<SeekPreview | null>(null);
  const telemetry = useRef({ position: positionMs, receivedAt: performance.now() });
  const pendingSeek = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastSeekAt = useRef(0);
  // Tempo-linked: a lane shows the same real time at any tempo, so synced
  // decks draw their beats at the same spacing.
  const span = mode === "scroll" ? laneSpanMs(zoomSeconds, rate) : durationMs;
  const [physicalPixels,setPhysicalPixels]=useState(2000);
  const nativeWaveform = useNativeWaveform(label, positionMs, span, mode === "overview", physicalPixels);
  useEffect(()=>{const size=sizeRef.current;setPhysicalPixels((layout==='vertical'?size.height:size.width)*(window.devicePixelRatio||1));},[layout]);
  const remoteBands = useMemo(() => assetWaveform(remoteWaveform), [remoteWaveform]);
  const wave = useMemo(() => remoteBands ? fromBandPeaks(remoteBands) : local.wave, [remoteBands, local.wave]);
  const clamp = (position: number) => Math.max(mode === "scroll" ? -60_000 : 0, Math.min(durationMs, position));

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
  }, [trackId,assetId]);

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

      // Below half a second the engine's sample-accurate PCM window shows more
      // than 150 columns per second can; elsewhere the 3-band model wins.
      const fine = mode === "scroll" && zoomSeconds < 0.5 && nativeWaveform.tiles.some((tile) => tile.framesPerBin === 1);
      const tiles = wave && !fine ? NO_TILES : nativeWaveform.tiles;
      const rasterKey = [tiles, wave, width, height, ratio, layout, side, compact, durationMs];
      const cached = mode === "overview" && overviewRaster.current?.key.every((value, i) => value === rasterKey[i]);
      if (cached) ctx.drawImage(overviewRaster.current!.canvas, 0, 0, width, height);
      else if (wave && !fine) {
        // Keep the ruler band of a zoomed lane and the phrase strip free.
        const reserve = compact ? 2 : (mode === "scroll" ? Math.min(24, breadth * 0.2) : 0) + (phrases.length ? phraseStrip(breadth) + 1 : 0);
        if (mode === "scroll") detailTiles.current.draw(ctx, wave, { startMs, spanMs: span, width, height, vertical, side, reserve, ratio });
        else drawThreeBand(ctx, wave, { startMs, spanMs: span, width, height, vertical, side, style: "overview", reserve, ratio });
      } else if (tiles.length) waveformRenderer.draw(ctx, tiles, startMs, span, width, height, vertical, side);
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
      if (pendingLoopInMs !== null && pendingLoopInMs >= startMs && pendingLoopInMs <= startMs + span) {
        ctx.setLineDash([2, 3]);
        line((pendingLoopInMs - startMs) / span * length, "#ff6b6b", compact ? 1 : 2);
        ctx.setLineDash([]);
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
      // Phrase bands under the waveform (rekordbox colours by family; Fill-in hatched).
      if (phrases.length && !compact) {
        // A thin, translucent strip that scales with the lane, so it marks the
        // structure without reading as a divider between stacked decks.
        const strip = phraseStrip(breadth);
        const showNames = phraseLabels === "always" || hovering.current;
        const toOffset = (time: number) => (time - startMs) / span * length;
        let lastLabelEnd = -Infinity;
        for (const phrase of phrases) {
          const from = toOffset(phrase.startMs), to = toOffset(phrase.endMs);
          if (to < 0 || from > length) continue;
          const a = Math.max(0, from), b = Math.min(length, to);
          ctx.fillStyle = `${PHRASE_COLORS[phrase.kind]}8c`;
          if (vertical) ctx.fillRect(side === "left" ? 0 : width - strip, a, strip, Math.max(1, b - a - 1));
          else ctx.fillRect(a, height - strip, Math.max(1, b - a - 1), strip);
          if (phrase.fillMs !== null) {
            const f = Math.max(a, toOffset(phrase.fillMs));
            ctx.fillStyle = "#00000088";
            for (let x = f; x < b - 1; x += 4) {
              if (vertical) ctx.fillRect(side === "left" ? 0 : width - strip, x, strip, 1.5); else ctx.fillRect(x, height - strip, 1.5, strip);
            }
          }
          if (showNames && !vertical && b - a > 18) {
            ctx.font = "bold 10px ui-sans-serif, system-ui, sans-serif";
            const text = phrase.label, textWidth = ctx.measureText(text).width;
            const x = Math.max(a + 2, 2);
            if (x > lastLabelEnd + 4 && x + textWidth < length) {
              ctx.fillStyle = "#000000b0"; ctx.fillRect(x - 2, height - strip - 13, textWidth + 4, 12);
              ctx.fillStyle = PHRASE_COLORS[phrase.kind]; ctx.textBaseline = "alphabetic";
              ctx.fillText(text, x, height - strip - 3);
              lastLabelEnd = x + textWidth;
            }
          }
        }
      }
      // MEMORY CUEs: red marks at the top edge; memory loops as an amber bar.
      for (const memory of memoryCues) {
        const at = (memory.positionMs - startMs) / span * length;
        if (at < -6 || at > length + 6) continue;
        if (memory.endMs !== undefined) {
          const to = Math.min(length, (memory.endMs - startMs) / span * length);
          ctx.fillStyle = "#f2c94c99";
          if (vertical) ctx.fillRect(side === "left" ? 0 : width - 3, Math.max(0, at), 3, Math.max(2, to - Math.max(0, at)));
          else ctx.fillRect(Math.max(0, at), 0, Math.max(2, to - Math.max(0, at)), 3);
        }
        ctx.fillStyle = memory.endMs !== undefined ? "#f2c94c" : "#ff4b4b";
        ctx.beginPath();
        const mark = compact ? 3 : 5;
        if (vertical) { const x = side === "left" ? 0 : width; const d = side === "left" ? 1 : -1; ctx.moveTo(x, at - mark); ctx.lineTo(x + d * mark * 1.4, at); ctx.lineTo(x, at + mark); }
        else { ctx.moveTo(at - mark, 0); ctx.lineTo(at + mark, 0); ctx.lineTo(at, mark * 1.4); }
        ctx.fill();
        if (mode === "scroll" && !compact) { ctx.setLineDash([2, 3]); line(at, "#ff4b4b88", 1); ctx.setLineDash([]); }
      }
      const cueGroups:{at:number;indices:number[]}[]=[];
      hotCues.map((time,index)=>({time,index})).filter(item=>item.time!==null&&item.time>=startMs&&item.time<=startMs+span).sort((a,b)=>a.time!-b.time!).forEach(({time,index})=>{
        const at=(time!-startMs)/span*length,last=cueGroups[cueGroups.length-1];
        if(last&&at-last.at<22)last.indices.push(index);else cueGroups.push({at,indices:[index]});
      });
      hotCues.forEach((time, index) => {
        if (time === null || time < startMs || time > startMs + span) return;
        const at = (time - startMs) / span * length;
        const cueColor = hotCueColor(index, cueColors?.[index]);
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
  releaseDrag.current = (thrown = false) => {
    const gesture = drag.current;
    if (!gesture) return;
    drag.current = null;
    stopHeartbeat();
    if (gesture.kind === "scratch") {
      // 判定は記録済みの履歴だけで即座に行う。スクラッチなら待たずに end を送る。
      const release = thrown && onBackspin && gesture.motion ? classifyRelease(gesture.motion.samples, performance.now(), playing) : null;
      if (release?.kind === "backspin") onBackspin!({ gestureId: gesture.gestureId, positionMs: gesture.positionMs, velocity: release.velocity });
      else sendScratch(gesture, "end");
    }
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
          // 惰性で回っているところを掴んだら、同じジェスチャーを指で続ける。
          const grabbed = onScratchGrab?.();
          if (grabbed) { gesture.gestureId = grabbed.gestureId; gesture.positionMs = grabbed.positionMs; }
          gesture.motion = new MotionHistory();
          gesture.motion.push(event.timeStamp, gesture.positionMs);
          if (!grabbed) sendScratch(gesture, "begin");
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
          gesture.motion?.push(sample.timeStamp, gesture.positionMs);
          gesture.moved = true; sendScratch(gesture, "move", sample.timeStamp); continue;
        }
        gesture.moved = true;
        seekLive(clamp((coordinate - gesture.origin) / gesture.length * durationMs));
        }
      }}
      onPointerUp={(event) => {
        const gesture = drag.current; if (!gesture || gesture.pointer !== event.pointerId) return;
        releaseDrag.current(true);
        if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
      }}
      onPointerEnter={() => { hovering.current = true; invalidateRef.current(); }}
      onPointerLeave={() => { hovering.current = false; invalidateRef.current(); }}
      onLostPointerCapture={() => releaseDrag.current()}
      onPointerCancel={() => releaseDrag.current()} />
    {dragging&&<span className="dj-wave-grip" aria-hidden="true">⋮⋮</span>}
    {label && <span title={hotCues.map((time,i)=>time===null?null:`${String.fromCharCode(65+i)}: ${(time/1000).toFixed(3)}s`).filter(Boolean).join(" / ")} className={cn("dj-wave-label", color === "cyan" ? "dj-blue" : "dj-orange")}>{label}</span>}
    {nativeWaveform.error&&<button className="dj-wave-retry" onClick={nativeWaveform.retry} title={nativeWaveform.error}>波形を再試行</button>}
    {!wave && !nativeWaveform.tiles.length && <span className="dj-wave-empty" style={{ pointerEvents: local.error ? "auto" : "none" }}>
      {local.loading ? (compact ? "" : "Loading waveform…") : local.error ? <button onClick={local.retry} title={local.error}>波形を再試行</button> : assetId ? "共有音源の波形を準備中" : trackId ? "波形未解析" : compact ? "—" : "No track loaded"}
    </span>}
    {mode === "scroll" && wave && !compact && <span className="dj-wave-source" title={wave.source === "rekordbox" ? "rekordbox の解析波形" : "plumdeck の解析波形"}>{wave.source === "rekordbox" ? "RB" : ""}</span>}
  </div>;
});
