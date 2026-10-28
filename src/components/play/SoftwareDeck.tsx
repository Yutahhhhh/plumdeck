import { assetIdentity, localTrackId } from '@/services/junction/asset-resolver';
import { SamplerPads } from "./SamplerPads";
import { djEngineClient } from "@/services/dj-engine/client";
import { useEffect, useMemo, useState, useSyncExternalStore, type ReactNode } from "react";
import { getPadSelection, selectPads, subscribePads, PAD_MODE_NAMES } from "@/services/midi/pad-state";
import { sampler } from "@/services/dj-engine/sampler";
import { memoryCues, selectedMemoryCue } from "@/services/dj-engine/memory-cues";
import { AlignJustify, Disc3, LayoutGrid, Pause, Play, Upload } from "lucide-react";
import type { ChannelState, DeckId, DeckState } from "@/types/dj-engine";
import type { PerformanceLoop } from "@/types/performance-metadata";
import { cn } from "@/lib/utils";
import { DJ_ENGINE_CAPABILITIES } from "@/types/dj-engine";
import { currentKey, formatKeyDelta, formatLibraryKey, parseKey } from "@/services/dj-engine/musical-key";
import { usePlayPreferences } from "./play-preferences";
import { DeckWaveform } from "./DeckWaveform";
import { artworkUrl, useTrackVisuals } from "./useTrackVisuals";
import { barNumbers, lowerBeat } from "./beat-grid-math";
import { availablePadModes, PerformancePads, UNBACKED_PAD_MODES, type FxEffect, type PadMode } from "./PerformancePads";
import { PadModeSelect } from "./PadModeSelect";
import { AutoBeatLoop, type LoopMode } from "./AutoBeatLoop";
import { beatLoopPress, clampBeats, loopBeats, LOOP_BEATS_RANGE, LOOP_BEATS_RANGE_EXTENDED, resizeLoop, stepBeats } from "./loop-math";
import { TempoPlatter, TEMPO_RANGES, type TempoRange } from "./TempoPlatter";
import { RotaryKnob } from "./RotaryKnob";
import { JOG_WEIGHT_DEFAULT, jogWeight, jogWeightLabel, setJogWeight, subscribeJogWeight } from "@/services/dj-engine/jog-weight";
import "./play-controls.css";
import { usePlayDeckDrop } from "./PlayDragDrop";

export function formatTime(ms: number) {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
}

export function Fader({ label, value, min, max, step = .01, vertical, disabled, onChange }: { label: string; value: number; min: number; max: number; step?: number; vertical?: boolean; disabled?: boolean; onChange: (value: number) => void }) {
  return <input type="range" aria-label={label} aria-orientation={vertical ? "vertical" : "horizontal"} min={min} max={max} step={step} value={value} disabled={disabled} onChange={(event) => onChange(Number(event.target.value))} className={cn("dj-fader", vertical && "dj-fader--vertical")} />;
}

type Props = {
  id: DeckId; deck?: DeckState; channel?: ChannelState; active: boolean; connected: boolean; capability: boolean; capabilities: readonly string[];
  /** OUTGOING tail: everything but loops is locked, with this reason. */
  tailLock?: string;
  onActivate: () => void; onToggle: () => void; onCue: () => void;
  onSeek: (delta: number) => void; onSeekAbsolute: (ms: number) => void; onTempo: (rate: number) => void;
  onKeylock: (enabled: boolean) => void; onSync: (enabled: boolean) => void; onMaster?: () => void; onUnload: () => void;
  /** The deck the engine reports as sync leader (MASTER), if any. */
  syncLeader?: DeckId | null;
  onHotCue: (index: number, clear: boolean) => void; onLoop: (beats: number) => void;
  onBeatJump?: (beats: number) => void; onBeatLoop?: (beats: number) => void; onLoopEnable?: (enabled: boolean) => void;
  onQuantize?: (enabled: boolean) => void; onFx?: (effect: FxEffect, enabled: boolean, mix: number, depth: number) => void; onSaveLoop?: () => void;
  onLoopIn?: () => void; onLoopOut?: () => void; onReloop?: () => void;
  /** Set and enable a loop (resize keeping IN). */
  onLoopSet?: (startMs: number, endMs: number) => void;
  /** Manual loop IN waiting for OUT. */
  pendingLoopIn?: number | null;
  onMemory?: (action: "save" | "delete" | "previous" | "next") => void; onMemoryCall?: (index: number) => void;
  onKeyShift?: (semitones: number) => void; onKeySync?: () => void; onKeyReset?: () => void;
  onSlip?: (enabled: boolean) => void; onReverse?: (enabled: boolean) => void;
  savedLoops?: readonly PerformanceLoop[]; onRecallLoop?: (id: string) => void;
  trackKey?: string | null;
  cueColors?: (string|null)[];
  /** 曲頭より前に置いた無音の助走。countdown は再生開始待ちの残り時間。 */
  onGridEdit: () => void; onGridClose?: () => void; gridEditor?: ReactNode;
};

export function SoftwareDeck({ id, deck, channel, active, connected, capability, capabilities, tailLock, onActivate, onToggle, onCue, onSeekAbsolute, onTempo, onKeylock, onSync, onMaster, onUnload, syncLeader, onHotCue, onLoop, onBeatJump, onBeatLoop, onLoopEnable, onQuantize, onFx, onSaveLoop, onLoopIn, onLoopOut, onReloop, onLoopSet, pendingLoopIn, onMemory, onMemoryCall, onKeyShift, onKeySync, onKeyReset, onSlip, onReverse, savedLoops, onRecallLoop, trackKey, cueColors, onGridEdit, onGridClose, gridEditor }: Props) {
  const deckDrop = usePlayDeckDrop(`play-deck-controls-${id}`, id);
  const left = id === "A" || id === "C";
  const [autoBeats, setAutoBeats] = useState(4);
  const selection = useSyncExternalStore(subscribePads, () => getPadSelection(id));
  const samplerState = useSyncExternalStore(sampler.subscribe,sampler.getSnapshot);
  const weight = useSyncExternalStore(subscribeJogWeight, () => jogWeight(id));
  const padPage = selection.page;
  const setPadPage = (update: number | ((page: number) => number)) => selectPads(id,selection.mode,typeof update === "function" ? update(padPage) : update,true);
  const [keyboardCue, setKeyboardCue] = useState(0);
  const [padError, setPadError] = useState("");
  const [extraMode, setExtraMode] = useState<PadMode | null>(null);
  // Modes the controller does not know page locally; paging the controller
  // selection would drop back to its mode.
  const [extraPage, setExtraPage] = useState(0);
  const padMode = extraMode ?? PAD_MODE_NAMES[selection.mode];
  const [memoryRevision, setMemoryRevision] = useState(0);
  useEffect(() => {
    const changed = () => setMemoryRevision((value) => value + 1);
    window.addEventListener("plumdeck:memory-cues", changed);
    return () => window.removeEventListener("plumdeck:memory-cues", changed);
  }, []);
  const memoryTrack = deck?.track ? assetIdentity(deck.track) : undefined;
  const memoryPoints = useMemo(() => memoryTrack ? memoryCues(memoryTrack, deck?.track?.durationMs) : [], [memoryTrack, deck?.track?.durationMs, memoryRevision]);
  const memorySelected = selectedMemoryCue(id, memoryTrack);
  const displayedPage = padMode==="sampler"?samplerState.bank:extraMode?extraPage:padPage;
  const lastPage = padMode==="sampler"?3:padMode==="memorycue"?Math.min(7,Math.max(1,Math.ceil(memoryPoints.length/8)-1)):1;
  const movePage = (delta:number) => {
    if(padMode==="sampler") void sampler.command("bank",{bank:Math.max(0,Math.min(3,samplerState.bank+delta))}).catch(error=>setPadError(String(error)));
    else if(extraMode) setExtraPage(page=>Math.max(0,Math.min(lastPage,page+delta)));
    else setPadPage(page=>page+delta);
  };
  const setPadMode = (mode: PadMode) => {
    const index = PAD_MODE_NAMES.indexOf(mode as typeof PAD_MODE_NAMES[number]);
    setExtraMode(index < 0 ? mode : null);
    setExtraPage(0);
    if (index >= 0) selectPads(id,index,index===4 || index===7 ? 1 : 0,true);
  };
  useEffect(() => { setExtraMode(null); }, [selection]);
  useEffect(() => {
    const receive = (event: Event) => {
      const action = (event as CustomEvent<{ deck?: DeckId; control: string; value: number; mode?: number }>).detail;
      if (action.deck !== id) return;
      if (action.control === "tempoRange" && TEMPO_RANGES.includes(action.value as TempoRange)) { setTempoRange(action.value as TempoRange); localStorage.setItem(`plumdeck.tempoRange.${id}`, String(action.value)); }
    };
    window.addEventListener("plumdeck:controller-library", receive);
    return () => window.removeEventListener("plumdeck:controller-library", receive);
  }, [id]);
  const [loopMode, setLoopMode] = useState<LoopMode>("auto");
  const [tempoRange, setTempoRange] = useState<TempoRange>(() => { const saved = Number(localStorage.getItem(`plumdeck.tempoRange.${id}`)) as TempoRange; return TEMPO_RANGES.includes(saved) ? saved : TEMPO_RANGES[2]; });
  const [fxMix, setFxMix] = useState(.8);
  const trackId = localTrackId(deck?.track);
  const { data } = useTrackVisuals(trackId);
  const duration = deck?.track?.durationMs ?? 0;
  const position = deck?.positionMs ?? 0;
  const rate = deck?.rate ?? 1;
  const trackBpm = deck?.track?.bpm ?? null;
  const bpm = trackBpm !== null && trackBpm > 0 ? trackBpm * rate : deck?.effectiveBpm ?? null;
  const disabled = Boolean(tailLock) || !connected || !capability || !deck?.track;
  const supported = (name: string) => !disabled && capabilities.includes(name);
  const quantize = (deck as (DeckState & { quantize?: boolean }) | undefined)?.quantize ?? false;
  const available = (capabilityName: string, feature: string, controller = true, needsTrack = true) =>
    !connected ? "未接続" : !capability ? `Deck ${id} 未実装` : needsTrack && !deck?.track ? "曲が未ロード"
      : !capabilities.includes(capabilityName) ? `${feature} 未実装` : !controller ? `${feature} コントローラー未接続` : undefined;
  const unavailable = (capabilityName: string, feature: string, controller = true, needsTrack = true) => tailLock ?? available(capabilityName, feature, controller, needsTrack);
  const hotCueReason = unavailable("deck.hotcue", "HOT CUE", Boolean(onHotCue));
  const loopReason = available("deck.loop", "LOOP", Boolean(onBeatLoop || onLoop));
  const jumpReason = unavailable("deck.beatjump", "BEAT JUMP", Boolean(onBeatJump));
  const fxReason = unavailable("mixer.fx", "PAD FX", Boolean(onFx));
  const quantizeReason = unavailable("deck.quantize", "QUANTIZE", Boolean(onQuantize), false);
  const gridReason = unavailable("deck.beatgrid", "GRID EDIT", true);
  const memoryReason = unavailable("deck.transport", "MEMORY CUE", Boolean(onMemory && onMemoryCall));
  const { keyNotation } = usePlayPreferences();
  const keyShift = Math.round(deck?.keyShift ?? 0);
  const key = currentKey(trackKey, keyShift, rate, Boolean(deck?.keylock), keyNotation);
  const originalKey = formatLibraryKey(trackKey, keyNotation);
  const keyReason = unavailable("deck.key", "KEY", Boolean(onKeyShift));
  const keySyncReason = keyReason ?? (!capabilities.includes(DJ_ENGINE_CAPABILITIES.deckKeySync) ? "KEY SYNC：この音声エンジンは曲のキーを検出できないため使えません"
    : !parseKey(trackKey) ? "KEY SYNC：この曲のキーが不明です" : !onKeySync ? "KEY SYNC コントローラー未接続" : undefined);
  const slipReason = unavailable("deck.slip", "SLIP", Boolean(onSlip));
  const reverseReason = unavailable("deck.reverse", "REVERSE", Boolean(onReverse));
  const loopRange = capabilities.includes(DJ_ENGINE_CAPABILITIES.deckLoopExtended) ? LOOP_BEATS_RANGE_EXTENDED : LOOP_BEATS_RANGE;
  const loopGrid = { bpm: trackBpm, beatgridOffsetMs: deck?.track?.beatgridOffsetMs, beatTimesMs: deck?.track?.beatTimesMs };
  const activeLoop = deck?.loopRegion?.enabled ? deck.loopRegion : null;
  const activeBeats = activeLoop ? loopBeats(activeLoop, loopGrid) : null;
  const createLoop = onBeatLoop ?? onLoop;
  // BEAT LOOP pad: same size exits, another size keeps IN, idle starts a new loop.
  const pressBeatLoop = (beats: number) => {
    setAutoBeats(beats);
    const press = beatLoopPress(activeLoop, beats, loopGrid, duration, loopRange);
    if (press.kind === "create") createLoop(beats);
    else if (press.kind === "exit") onLoopEnable?.(false);
    else onLoopSet?.(press.loop.startMs, press.loop.endMs);
  };
  const resized = (direction: -1 | 1) => activeLoop ? resizeLoop(activeLoop, direction < 0 ? 0.5 : 2, duration, loopGrid, loopRange) : null;
  const stepLoop = (direction: -1 | 1) => {
    if (!activeLoop) { setAutoBeats(stepBeats(autoBeats, direction, loopRange)); return; }
    const next = resized(direction);
    if (!next) return;
    if (activeBeats) setAutoBeats(clampBeats(activeBeats * (direction < 0 ? 0.5 : 2), loopRange));
    onLoopSet?.(next.startMs, next.endMs);
  };

  // Bar/beat come from the same grid the scrolling waveform draws, so the
  // numbers and the visible bar lines can never disagree.
  const beatMs = trackBpm && trackBpm > 0 ? 60_000 / trackBpm : 0;
  const beatTimes = deck?.track?.beatTimesMs;
  const exactIndex = beatTimes?.length ? Math.max(-1, lowerBeat(beatTimes, position + 0.0001) - 1) : null;
  const beatIndex = exactIndex ?? (beatMs && deck?.track?.beatgridOffsetMs !== undefined ? Math.floor((position - deck.track.beatgridOffsetMs) / beatMs) : null);
  const beatsPerBar = deck?.track?.beatsPerBar ?? 4;
  const exactBars = useMemo(() => barNumbers(beatTimes?.length ?? 0, deck?.track?.beatNumbers, beatsPerBar), [beatTimes, deck?.track?.beatNumbers, beatsPerBar]);
  const phaseCells = beatsPerBar * 2;
  const phase = beatIndex === null ? -1 : ((beatIndex + ((deck?.track?.beatNumbers?.[0] ?? 1) - 1)) % phaseCells + phaseCells) % phaseCells;
  const bar = beatIndex === null ? null : exactIndex !== null ? exactBars[exactIndex] ?? null : Math.floor(beatIndex / beatsPerBar) + 1;
  const beatInBar = beatIndex === null ? null : exactIndex !== null && exactIndex >= 0 ? deck?.track?.beatNumbers?.[exactIndex] ?? exactIndex % beatsPerBar + 1 : ((beatIndex % beatsPerBar) + beatsPerBar) % beatsPerBar + 1;
  const describeCue = (ms: number) => {
    if (beatTimes?.length) {
      const index = Math.max(0, lowerBeat(beatTimes, ms + 0.5) - 1);
      if (Math.abs(beatTimes[index] - ms) > 15) return `${formatTime(ms)}.${Math.floor(ms / 100) % 10}（拍外）`;
      return `BAR ${exactBars[index] ?? "—"} · ${deck?.track?.beatNumbers?.[index] ?? index % beatsPerBar + 1}/${beatsPerBar}`;
    }
    if (!beatMs || deck?.track?.beatgridOffsetMs === undefined) return `${formatTime(ms)}.${Math.floor(ms / 100) % 10}`;
    const beat = (ms - deck.track.beatgridOffsetMs) / beatMs;
    if (Math.abs(beat - Math.round(beat)) * beatMs > 15) return `${formatTime(ms)}.${Math.floor(ms / 100) % 10}（拍外）`;
    return `BAR ${Math.floor(Math.round(beat) / beatsPerBar) + 1} · ${Math.round(beat) % beatsPerBar + 1}/${beatsPerBar}`;
  };

  const status = !capability ? "N/A" : !connected ? "OFFLINE" : deck?.status === "error" ? "ERROR"
    : deck?.status === "playing" ? "PLAYING" : deck?.status === "loading" ? "LOADING"
    : deck?.track ? deck.status === "paused" ? "PAUSED" : "READY" : "EMPTY";
  // MASTER lights from the engine's leader only, never from local intent.
  const leading = deck?.syncLeader === id;
  const stateKind = leading ? "master" : deck?.status === "error" ? "error" : deck?.status === "playing" ? "playing" : "idle";

  return <article ref={deckDrop.setNodeRef} aria-label={`Deck ${id}`} data-deck={id} data-track-drop-deck={id} data-track-drop-label={`DECK ${id} へロード`} data-track-drop-active={deckDrop.isOver ? "true" : undefined} onClick={onActivate}
    title={tailLock} className={cn("dj-deck", left ? "dj-deck--left" : "dj-deck--right", active && "dj-deck--active", !deck?.track && "dj-deck--empty", tailLock && "dj-deck--tail")}>
    <div className="dj-track-info">
      <button className="dj-deck-number" aria-label={`Select deck ${id}`} aria-pressed={active} onClick={onActivate}>{id}</button>
      <div className="dj-cover">{data?.artwork ? <img src={artworkUrl(data.artwork)} alt="" /> : <Disc3 />}</div>
      <div className="dj-track-name">
        <strong title={deck?.track?.title || ""}>{deck?.track?.title || (deck?.track?.assetId ? "共有音源" : "曲をドラッグしてロード")}</strong>
        <span>{deck?.track?.artist || (deck?.track?.assetId ? "" : "ライブラリで曲を選び、ダブルクリック")}</span>
      </div>
      <div className="dj-track-time">
        <strong className="dj-num">{deck?.track ? `−${formatTime(duration - position)}` : "−−:−−"}</strong>
        <span className="dj-num">{position < 0 ? "−" : ""}{formatTime(Math.abs(position))}.{Math.floor(Math.abs(position) / 100) % 10}</span>
      </div>
      <div className="dj-deck-flags">
        {tailLock && <span className="dj-chip dj-chip--tail" title={tailLock}>残りの曲 · ループのみ</span>}
        <span className="dj-chip dj-chip--state" data-state={stateKind}>{leading ? "MASTER" : deck?.syncEnabled ? `→ ${syncLeader && syncLeader !== id ? syncLeader : "—"}` : status}</span>
        {/* rekordbox のデッキヘッダ：左に KEY SYNC とキー表示、右に BEAT SYNC / MASTER。 */}
        <div className="dj-sync-block" role="group" aria-label={`Deck ${id} SYNC`}>
          <button className="dj-chip dj-chip--keysync" disabled={Boolean(keySyncReason)}
            title={keySyncReason ?? "KEY SYNC：MASTER のキーに近い方向へキーを合わせます"} onClick={() => onKeySync?.()}>KEY SYNC</button>
          <button className={cn("dj-chip dj-chip--beatsync", deck?.syncEnabled && "is-on")} disabled={!supported("deck.sync")} aria-pressed={Boolean(deck?.syncEnabled)}
            title={capabilities.includes("deck.sync") ? "テンポシンク：BPMを合わせます。拍位置の自動移動は行いません" : "この音声エンジンはシンク非対応"}
            onClick={() => onSync(!deck?.syncEnabled)}><span>BEAT</span><span>SYNC</span></button>
          <div className="dj-key-shift" title={keyReason ?? `現在のキー${key.approximate ? "（MT OFF のためテンポで音程が動いています。≈ は最寄りの半音）" : ""}${originalKey ? ` · 原キー ${originalKey}` : " · 原キー不明"}。ダブルクリックで KEY RESET`}
            onDoubleClick={() => { if (!keyReason && keyShift !== 0) onKeyReset?.(); }}>
            <button type="button" disabled={Boolean(keyReason) || keyShift <= -12} aria-label="キーを半音下げる" title={keyReason ?? "キーを半音下げる"} onClick={() => onKeyShift?.(keyShift - 1)}>‹</button>
            <b className={cn(key.delta !== 0 && "is-shifted")}>{key.approximate ? "≈" : ""}{key.text ?? "—"}</b><i className="dj-num">{formatKeyDelta(key.delta)}</i>
            <button type="button" disabled={Boolean(keyReason) || keyShift >= 12} aria-label="キーを半音上げる" title={keyReason ?? "キーを半音上げる"} onClick={() => onKeyShift?.(keyShift + 1)}>›</button>
          </div>
          <button className={cn("dj-chip dj-chip--master", leading && "is-on")} disabled={!supported("deck.sync") || !onMaster} aria-pressed={leading}
            title={leading ? "MASTER を解除します（この音声エンジンではこのデッキの BEAT SYNC も解除されます）" : "シンクマスター：このデッキを BEAT SYNC の基準にします（この音声エンジンではこのデッキの SYNC も ON になります）"} onClick={() => onMaster?.()}>MASTER</button>
        </div>
      </div>
    </div>
    <div className="dj-overview">
      <DeckWaveform assetId={deck?.track?.assetId} remoteWaveform={deck?.track?.waveform} trackId={trackId} positionMs={position} durationMs={duration} layout="horizontal" side={left ? "left" : "right"} color={left ? "cyan" : "fuchsia"} hotCues={deck?.hotCues} playing={deck?.status === "playing"} rate={deck?.rate ?? 1} bpm={deck?.track?.bpm} loopRegion={deck?.loopRegion} pendingLoopInMs={pendingLoopIn} onSeek={disabled ? undefined : onSeekAbsolute} />
    </div>
    <div className="dj-deck-controls">
      <div className="dj-perf">
        {/* rekordbox 同様、左のレールでパッドとグリッド編集を切り換える。 */}
        <div className="dj-panel-rail" role="group" aria-label={`Deck ${id} パネル切り換え`}>
          <button type="button" className={cn("dj-panel-tab", !gridEditor && "is-on")} aria-pressed={!gridEditor}
            title="パフォーマンスパッド" aria-label={`Deck ${id} パフォーマンスパッド`}
            onClick={() => { if (gridEditor) onGridClose?.(); }}><LayoutGrid /></button>
          <button type="button" className={cn("dj-panel-tab dj-panel-tab--grid", gridEditor && "is-on")} aria-pressed={Boolean(gridEditor)}
            disabled={Boolean(gridReason) || !trackId} title={gridReason ?? (trackId ? "GRID EDIT：BPM・拍位置の修正" : "曲が未ロード")}
            aria-label={`Deck ${id} グリッド編集`} onClick={onGridEdit}><AlignJustify /></button>
        </div>
        <div className="dj-perf-main">
          <div className="dj-phase" aria-hidden style={{ gridTemplateColumns: `repeat(${phaseCells}, 1fr)` }}>{Array.from({ length: phaseCells }, (_, index) => <i key={index} className={cn(index % beatsPerBar === 0 && "is-bar", index === phase && "is-on")} />)}</div>
          {gridEditor ? <div className="dj-grid-editor-slot">{gridEditor}</div> : <>
            {padMode === "sampler" ? <SamplerPads offset={left?8:0} enabled={Boolean(connected && capabilities.includes("sampler"))} cueAvailable={capabilities.includes("mixer.pfl")} /> : <PerformancePads page={displayedPage} cueColors={cueColors} onKey={(semitones, keyboard) => {
              setPadError(""); const generation = djEngineClient.getDeckGeneration(id);
              void (async () => { await djEngineClient.keyShift(id, semitones);
                if (keyboard && generation === djEngineClient.getDeckGeneration(id)) { const cue = deck?.hotCues[keyboardCue]; if (cue == null) throw new Error("KEYBOARDにはホットキューを設定してください"); await djEngineClient.seek(id,cue); if(generation === djEngineClient.getDeckGeneration(id)) await djEngineClient.play(id); }
              })().catch(e=>setPadError(String(e)));
            }} mode={padMode} hotCues={deck?.hotCues} formatTime={formatTime} describeCue={describeCue} selectedLoopBeats={activeBeats ?? undefined} fxMix={fxMix} activeFx={channel?.fx?.enabled ? channel.fx.effect : undefined}
              hotCueDisabledReason={hotCueReason} beatLoopDisabledReason={loopReason} beatJumpDisabledReason={jumpReason} padFxDisabledReason={fxReason}
              onHotCue={onHotCue} onBeatLoop={pressBeatLoop} onBeatJump={onBeatJump} onFx={onFx}
              memoryCues={memoryPoints} selectedMemory={memorySelected} memoryDisabledReason={memoryReason} onMemoryCall={onMemoryCall} />}
            {padMode === "memorycue" && <div className="dj-memory-actions" role="group" aria-label={`Deck ${id} MEMORY CUE`}>
              <button type="button" disabled={Boolean(memoryReason) || !memoryPoints.length} title={memoryReason ?? "前のメモリーキューへ"} aria-label="前のメモリーキュー" onClick={() => onMemory?.("previous")}>◀</button>
              <button type="button" disabled={Boolean(memoryReason)} title={memoryReason ?? "現在位置（ループ中はループ）をメモリーキューとして保存"} onClick={() => onMemory?.("save")}>MEMORY</button>
              <button type="button" disabled={Boolean(memoryReason) || memorySelected === null} title={memoryReason ?? (memorySelected === null ? "削除するメモリーキューをパッドで選んでください" : "選択中のメモリーキューを削除")} onClick={() => onMemory?.("delete")}>DELETE</button>
              <button type="button" disabled={Boolean(memoryReason) || !memoryPoints.length} title={memoryReason ?? "次のメモリーキューへ"} aria-label="次のメモリーキュー" onClick={() => onMemory?.("next")}>▶</button>
            </div>}
            {padMode === "keyboard" && <label>HOT CUE <select aria-label={`Deck ${id} KEYBOARDのキュー`} value={keyboardCue} onChange={e=>setKeyboardCue(Number(e.target.value))}>{Array.from({length:16},(_,i)=><option key={i} value={i} disabled={deck?.hotCues[i]==null}>{String.fromCharCode(65+i)}</option>)}</select></label>}
            {padError && <small role="alert">{padError}</small>}
            <div className="dj-pad-mode-row">
              <div className="dj-pad-pages" role="group" aria-label={`Deck ${id} ${padMode==="sampler"?"バンク":"ページ"}`}>
                {Array.from({length:lastPage+1},(_,page)=><button key={page} type="button" aria-pressed={displayedPage===page} onClick={()=>movePage(page-displayedPage)}>{padMode==="sampler"?"BANK":"PAGE"} {page+1}</button>)}
              </div>
              <PadModeSelect deckId={id} value={padMode} modes={availablePadModes(capabilities, padMode)} onChange={(mode) => { if (gridEditor) onGridClose?.(); setPadMode(mode); }} />
              {["padfx","padfx2"].includes(padMode) && <label className="dj-pad-fx-depth" title={fxReason ?? "PAD FX の掛かり具合"}>
                <span>DEPTH</span>
                <input type="range" min={0} max={1} step={.01} value={fxMix} disabled={Boolean(fxReason)}
                  aria-label={`Deck ${id} PAD FX の深さ`} onChange={(event) => setFxMix(Number(event.target.value))} />
                <b className="dj-num">{Math.round(fxMix * 100)}%</b>
              </label>}
            </div>
          </>}
          <div className="dj-perf-caption">
            <span>{bar === null ? "BAR —" : `BAR ${bar} · ${beatInBar ?? "—"}/${beatsPerBar}`}</span>
            <span>{gridEditor ? "GRID編集中 · 再生操作は利用可" : UNBACKED_PAD_MODES[padMode] ? "この音声エンジン未対応" : padMode === "hotcue" ? (hotCueReason ?? "SHIFT: CLEAR") : padMode === "padfx" || padMode === "padfx2" ? (fxReason ?? "押している間だけ有効") : padMode === "memorycue" ? (memoryReason ?? `MEMORY ${memoryPoints.length}`) : (padMode === "beatloop" ? loopReason : jumpReason) ?? "READY"}</span>
          </div>
        </div>
      </div>
      <div className="dj-loop-controls">
        <AutoBeatLoop deckId={id} beats={activeBeats ?? autoBeats} mode={loopMode} loopActive={Boolean(activeLoop)} hasLoop={Boolean(deck?.loopRegion)}
          pendingIn={pendingLoopIn !== null && pendingLoopIn !== undefined}
          disabledReason={loopReason}
          canHalve={activeLoop ? Boolean(resized(-1)) : autoBeats > loopRange.min}
          canDouble={activeLoop ? Boolean(resized(1)) : autoBeats < loopRange.max}
          onMode={setLoopMode}
          onStep={stepLoop}
          onAuto={() => activeLoop ? onLoopEnable?.(false) : createLoop(autoBeats)}
          onLoopIn={() => onLoopIn?.()}
          onLoopOut={() => onLoopOut?.()}
          onReloop={() => onReloop?.()} />
        <div className="dj-loop-extra">
          <button disabled={!onSaveLoop || Boolean(loopReason) || !deck?.loopRegion} title={!onSaveLoop ? "保存コントローラー未接続" : !deck?.loopRegion ? "保存するループがありません" : "現在のループを保存"} onClick={onSaveLoop}>保存</button>
          {!!savedLoops?.length && <select className="dj-saved-loops" aria-label={`Deck ${id} 保存済みループ`} defaultValue="" disabled={!onRecallLoop || Boolean(loopReason)} onChange={(event) => { if (event.target.value) onRecallLoop?.(event.target.value); event.currentTarget.value = ""; }}><option value="">保存済み…</option>{savedLoops.map((loop) => <option key={loop.id} value={loop.id}>{loop.label}</option>)}</select>}
        </div>
      </div>
      <div className="dj-transport">
        <div className="dj-transport-buttons">
        <button className="dj-cue-button" disabled={disabled} onClick={onCue} title="一時停止中：現在位置にキューを設定 / 再生中：キューポイントへ戻る">CUE</button>
        <button className={cn("dj-play-button", deck?.status === "playing" && "is-playing")} disabled={disabled} onClick={onToggle} aria-label={`Deck ${id} ${deck?.status === "playing" ? "pause" : "play"}`}>{deck?.status === "playing" ? <Pause /> : <Play />}</button>
        </div>
      <div className="dj-deck-rail" role="group" aria-label={`Deck ${id} SLIP・QUANTIZE・MASTER TEMPO`}>
        <button className={cn("dj-chip dj-chip--toggle", deck?.slip && "is-on")} disabled={Boolean(slipReason)} aria-pressed={Boolean(deck?.slip)}
          title={slipReason ?? "SLIP：演奏操作の間も裏で再生位置を進めます"} onClick={() => onSlip?.(!deck?.slip)}>SLIP</button>
        <button className={cn("dj-chip dj-chip--toggle", deck?.reverse && "is-on")} disabled={Boolean(reverseReason)} aria-pressed={Boolean(deck?.reverse)}
          title={reverseReason ?? "REVERSE：逆再生"} onClick={() => onReverse?.(!deck?.reverse)}>REV</button>
        <button className={cn("dj-chip", quantize && "is-on")} disabled={Boolean(quantizeReason)}
          title={quantizeReason ?? "クオンタイズ：キュー・ループを拍位置に合わせて置きます（再生位置は動きません）"} aria-pressed={quantize}
          onClick={() => onQuantize?.(!quantize)}>Q</button>
        <button className={cn("dj-chip", deck?.keylock && "is-on")} disabled={!supported("deck.keylock")} aria-pressed={Boolean(deck?.keylock)}
          title="マスターテンポ：音程を変えずに再生速度を調整します" onClick={() => onKeylock(!deck?.keylock)}>MT</button>
        {/* 波形を投げたバックスピンの重さ。スクラッチの操作感とハードウェアのジョグには効かない。 */}
        <RotaryKnob className="dj-jog-weight" label="JOG" value={weight} min={0} max={1} step={.05} fineStep={.01} defaultValue={JOG_WEIGHT_DEFAULT}
          valueText={(value) => `${Math.round(value * 100)}% ${jogWeightLabel(value)}`} onChange={(value) => setJogWeight(id, value)} />
      </div>
      </div>
      <TempoPlatter deckId={id} bpm={bpm} trackBpm={trackBpm} rate={rate} positionMs={position}
        playing={deck?.status === "playing"} disabled={!supported("deck.tempo")}
        range={tempoRange} onRange={(range) => { setTempoRange(range); localStorage.setItem(`plumdeck.tempoRange.${id}`, String(range)); window.dispatchEvent(new CustomEvent("plumdeck:controller-tempo-range", { detail: { deck: id, range } })); }} onTempo={onTempo} />
      <button className="dj-eject dj-eject--deck" disabled={disabled} onClick={onUnload} aria-label={`Eject deck ${id}`} title="デッキから取り出す"><Upload /></button>
    </div>
    {deck?.lastError && <div className="dj-deck-error" role="alert">{deck.lastError}</div>}
  </article>;
}
