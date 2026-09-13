import { localTrackId } from '@/services/junction/asset-resolver';
import { DeckPerformanceControls } from "./DeckPerformanceControls";
import { SamplerPads } from "./SamplerPads";
import { djEngineClient } from "@/services/dj-engine/client";
import { useEffect, useMemo, useState, useSyncExternalStore, type ReactNode } from "react";
import { getPadSelection, selectPads, subscribePads, PAD_MODE_NAMES } from "@/services/midi/pad-state";
import { sampler } from "@/services/dj-engine/sampler";
import { AlignJustify, Disc3, LayoutGrid, Pause, Play, Upload } from "lucide-react";
import type { ChannelState, DeckId, DeckState } from "@/types/dj-engine";
import type { PerformanceLoop } from "@/types/performance-metadata";
import { cn } from "@/lib/utils";
import { DeckWaveform } from "./DeckWaveform";
import { artworkUrl, useTrackVisuals } from "./useTrackVisuals";
import { barNumbers, lowerBeat } from "./beat-grid-math";
import { PerformancePads, UNBACKED_PAD_MODES, type FxEffect, type PadMode } from "./PerformancePads";
import { PadModeSelect } from "./PadModeSelect";
import { AutoBeatLoop, type LoopMode } from "./AutoBeatLoop";
import { TempoPlatter, TEMPO_RANGES, type TempoRange } from "./TempoPlatter";
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
  /** Presentation-only Junction binding. No control may reach the real deck. */
  monitorOnly?: boolean; monitorAssetId?: string;
  onActivate: () => void; onToggle: () => void; onCue: () => void;
  onSeek: (delta: number) => void; onSeekAbsolute: (ms: number) => void; onTempo: (rate: number) => void;
  onKeylock: (enabled: boolean) => void; onSync: (enabled: boolean) => void; onMaster?: () => void; onUnload: () => void;
  onHotCue: (index: number, clear: boolean) => void; onLoop: (beats: number) => void;
  onBeatJump?: (beats: number) => void; onBeatLoop?: (beats: number) => void; onLoopEnable?: (enabled: boolean) => void;
  onQuantize?: (enabled: boolean) => void; onFx?: (effect: FxEffect, enabled: boolean, mix: number, depth: number) => void; onSaveLoop?: () => void;
  onLoopIn?: () => void; onLoopOut?: () => void;
  savedLoops?: readonly PerformanceLoop[]; onRecallLoop?: (id: string) => void;
  trackKey?: string | null;
  cueColors?: (string|null)[];
  /** 曲頭より前に置いた無音の助走。countdown は再生開始待ちの残り時間。 */
  onGridEdit: () => void; onGridClose?: () => void; gridEditor?: ReactNode;
};

export function SoftwareDeck({ id, deck, channel, active, connected, capability, capabilities, monitorOnly = false, monitorAssetId, onActivate, onToggle, onCue, onSeekAbsolute, onTempo, onKeylock, onSync, onMaster, onUnload, onHotCue, onLoop, onBeatJump, onBeatLoop, onLoopEnable, onQuantize, onFx, onSaveLoop, onLoopIn, onLoopOut, savedLoops, onRecallLoop, trackKey, cueColors, onGridEdit, onGridClose, gridEditor }: Props) {
  const deckDrop = usePlayDeckDrop(`play-deck-controls-${id}`, id);
  const left = id === "A" || id === "C";
  const [loopBeats, setLoopBeats] = useState(4);
  const selection = useSyncExternalStore(subscribePads, () => getPadSelection(id));
  const samplerState = useSyncExternalStore(sampler.subscribe,sampler.getSnapshot);
  const padPage = selection.page;
  const setPadPage = (update: number | ((page: number) => number)) => selectPads(id,selection.mode,typeof update === "function" ? update(padPage) : update,true);
  const [keyboardCue, setKeyboardCue] = useState(0);
  const [padError, setPadError] = useState("");
  const [extraMode, setExtraMode] = useState<PadMode | null>(null);
  const padMode = extraMode ?? PAD_MODE_NAMES[selection.mode];
  const displayedPage = padMode==="sampler"?samplerState.bank:padPage;
  const lastPage = padMode==="sampler"?3:1;
  const movePage = (delta:number) => {
    if(padMode==="sampler") void sampler.command("bank",{bank:Math.max(0,Math.min(3,samplerState.bank+delta))}).catch(error=>setPadError(String(error)));
    else setPadPage(page=>page+delta);
  };
  const setPadMode = (mode: PadMode) => {
    const index = PAD_MODE_NAMES.indexOf(mode as typeof PAD_MODE_NAMES[number]);
    setExtraMode(index < 0 ? mode : null);
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
  const disabled = monitorOnly || !connected || !capability || !deck?.track;
  const supported = (name: string) => !disabled && capabilities.includes(name);
  const quantize = (deck as (DeckState & { quantize?: boolean }) | undefined)?.quantize ?? false;
  const unavailable = (capabilityName: string, feature: string, controller = true, needsTrack = true) =>
    monitorOnly ? "Junction Liveは表示専用です" : !connected ? "未接続" : !capability ? `Deck ${id} 未実装` : needsTrack && !deck?.track ? "曲が未ロード"
      : !capabilities.includes(capabilityName) ? `${feature} 未実装` : !controller ? `${feature} コントローラー未接続` : undefined;
  const hotCueReason = unavailable("deck.hotcue", "HOT CUE", Boolean(onHotCue));
  const loopReason = unavailable("deck.loop", "LOOP", Boolean(onBeatLoop || onLoop));
  const jumpReason = unavailable("deck.beatjump", "BEAT JUMP", Boolean(onBeatJump));
  const fxReason = unavailable("mixer.fx", "PAD FX", Boolean(onFx));
  const quantizeReason = unavailable("deck.quantize", "QUANTIZE", Boolean(onQuantize), false);
  const gridReason = unavailable("deck.beatgrid", "GRID EDIT", true);
  const selectLoopBeats = (beats: number) => {
    const next = Math.max(.125, Math.min(32, beats));
    setLoopBeats(next);
    if (deck?.loopRegion?.enabled) onBeatLoop?.(next);
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

  const status = monitorOnly ? "JUNCTION LIVE" : !capability ? "N/A" : !connected ? "OFFLINE" : deck?.status === "error" ? "ERROR"
    : deck?.status === "playing" ? "PLAYING" : deck?.status === "loading" ? "LOADING"
    : deck?.track ? deck.status === "paused" ? "PAUSED" : "READY" : "EMPTY";
  const leading = !monitorOnly && deck?.syncLeader === id && deck.syncEnabled;
  const stateKind = leading ? "master" : deck?.status === "error" ? "error" : deck?.status === "playing" ? "playing" : "idle";

  return <article ref={deckDrop.setNodeRef} aria-label={`Deck ${id}`} data-deck={id} data-track-drop-deck={id} data-track-drop-label={`DECK ${id} へロード`} data-track-drop-active={deckDrop.isOver ? "true" : undefined} onClick={onActivate}
    className={cn("dj-deck", left ? "dj-deck--left" : "dj-deck--right", active && "dj-deck--active", !deck?.track && "dj-deck--empty", monitorOnly && "dj-deck--junction-monitor")}>
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
        <span className="dj-chip dj-chip--state" data-state={monitorOnly ? "junction" : stateKind}>{monitorOnly ? status : leading ? "MASTER" : deck?.syncEnabled ? `→ ${deck.syncLeader || "—"}` : status}</span>
        {/* rekordbox のデッキヘッダ：左に KEY SYNC とキー表示、右に BEAT SYNC / MASTER。 */}
        <div className="dj-sync-block" role="group" aria-label={`Deck ${id} SYNC`}>
          <button className="dj-chip dj-chip--keysync" disabled
            title="キーシンク：トラックのキーを変更し、デッキ間のキーを合わせます（この音声エンジン未対応）">KEY SYNC</button>
          <button className={cn("dj-chip dj-chip--beatsync", deck?.syncEnabled && "is-on")} disabled={!supported("deck.sync")} aria-pressed={Boolean(deck?.syncEnabled)}
            title={capabilities.includes("deck.sync") ? "テンポシンク：BPMを合わせます。拍位置の自動移動は行いません" : "この音声エンジンはシンク非対応"}
            onClick={() => onSync(!deck?.syncEnabled)}><span>BEAT</span><span>SYNC</span></button>
          <div className="dj-key-shift" title="キー：この音声エンジンはキー変更未対応のため表示のみ">
            <button type="button" disabled aria-label="キーを下げる">‹</button>
            <b>{trackKey || "—"}</b><i className="dj-num">±0</i>
            <button type="button" disabled aria-label="キーを上げる">›</button>
          </div>
          <button className={cn("dj-chip dj-chip--master", leading && "is-on")} disabled={!supported("deck.sync") || !onMaster} aria-pressed={leading}
            title="シンクマスター：ビート/BPMシンクのマスターに設定します" onClick={() => onMaster?.()}>MASTER</button>
        </div>
      </div>
    </div>
    <div className="dj-overview">
      <DeckWaveform assetId={deck?.track?.assetId} remoteWaveform={deck?.track?.waveform} monitorAssetId={monitorAssetId} trackId={trackId} positionMs={position} durationMs={duration} layout="horizontal" side={left ? "left" : "right"} color={left ? "cyan" : "fuchsia"} hotCues={deck?.hotCues} playing={deck?.status === "playing"} rate={deck?.rate ?? 1} bpm={deck?.track?.bpm} loopRegion={deck?.loopRegion} onSeek={disabled ? undefined : onSeekAbsolute} />
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
            {padMode === "sampler" ? <SamplerPads offset={left?8:0} enabled={Boolean(!monitorOnly && connected && capabilities.includes("sampler"))} cueAvailable={!monitorOnly && capabilities.includes("mixer.pfl")} /> : <PerformancePads page={padPage} cueColors={cueColors} onKey={(semitones, keyboard) => {
              setPadError(""); const trackId = deck?.track?.trackId, generation = djEngineClient.getDeckGeneration(id);
              void (async () => { await djEngineClient.send("deck.key.shift", {deck:id,trackId,semitones});
                if (keyboard && generation === djEngineClient.getDeckGeneration(id)) { const cue = deck?.hotCues[keyboardCue]; if (cue == null) throw new Error("KEYBOARDにはホットキューを設定してください"); await djEngineClient.seek(id,cue); if(generation === djEngineClient.getDeckGeneration(id)) await djEngineClient.play(id); }
              })().catch(e=>setPadError(String(e)));
            }} mode={padMode} hotCues={deck?.hotCues} formatTime={formatTime} describeCue={describeCue} selectedLoopBeats={loopBeats} fxMix={fxMix} activeFx={channel?.fx?.enabled ? channel.fx.effect : undefined}
              hotCueDisabledReason={hotCueReason} beatLoopDisabledReason={loopReason} beatJumpDisabledReason={jumpReason} padFxDisabledReason={fxReason}
              onHotCue={onHotCue} onBeatLoop={(beats) => { setLoopBeats(beats); (onBeatLoop ?? onLoop)(beats); }} onBeatJump={onBeatJump} onFx={onFx} />}
            {padMode === "keyboard" && <label>HOT CUE <select aria-label={`Deck ${id} KEYBOARDのキュー`} value={keyboardCue} onChange={e=>setKeyboardCue(Number(e.target.value))}>{Array.from({length:16},(_,i)=><option key={i} value={i} disabled={deck?.hotCues[i]==null}>{String.fromCharCode(65+i)}</option>)}</select></label>}
            {padError && <small role="alert">{padError}</small>}
            <details className="dj-pad-extras"><summary>KEY / SLIP / MEMORY</summary><DeckPerformanceControls id={id} deck={deck} enabled={Boolean(!monitorOnly && connected && deck?.track)} /></details>
            <div className="dj-pad-mode-row">
              <div className="dj-pad-pages" role="group" aria-label={`Deck ${id} ${padMode==="sampler"?"バンク":"ページ"}`}>
                {Array.from({length:lastPage+1},(_,page)=><button key={page} type="button" aria-pressed={displayedPage===page} onClick={()=>movePage(page-displayedPage)}>{padMode==="sampler"?"BANK":"PAGE"} {page+1}</button>)}
              </div>
              <PadModeSelect deckId={id} value={padMode} onChange={(mode) => { if (gridEditor) onGridClose?.(); setPadMode(mode); }} />
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
            <span>{gridEditor ? "GRID編集中 · 再生操作は利用可" : UNBACKED_PAD_MODES[padMode] ? "この音声エンジン未対応" : padMode === "hotcue" ? (hotCueReason ?? "SHIFT: CLEAR") : padMode === "padfx" ? (fxReason ?? "HOLD: ACTIVE") : (padMode === "beatloop" ? loopReason : jumpReason) ?? "READY"}</span>
          </div>
        </div>
      </div>
      <div className="dj-loop-controls">
        <AutoBeatLoop deckId={id} beats={loopBeats} mode={loopMode} loopActive={Boolean(deck?.loopRegion?.enabled)} hasLoop={Boolean(deck?.loopRegion)}
          disabledReason={loopReason}
          onMode={setLoopMode}
          onBeats={selectLoopBeats}
          onSetLoop={(beats) => (onBeatLoop ?? onLoop)(beats)}
          onLoopToggle={() => deck?.loopRegion && (onLoopEnable ? onLoopEnable(!deck.loopRegion.enabled) : onLoop(loopBeats))}
          onLoopIn={() => onLoopIn?.()}
          onLoopOut={() => onLoopOut?.()} />
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
        <button className="dj-chip" disabled title="スリップ：この音声エンジン未対応">SLIP</button>
        <button className={cn("dj-chip", quantize && "is-on")} disabled={Boolean(quantizeReason)}
          title={quantizeReason ?? "クオンタイズ：キュー・ループを拍位置に合わせて置きます（再生位置は動きません）"} aria-pressed={quantize}
          onClick={() => onQuantize?.(!quantize)}>Q</button>
        <button className={cn("dj-chip", deck?.keylock && "is-on")} disabled={!supported("deck.keylock")} aria-pressed={Boolean(deck?.keylock)}
          title="マスターテンポ：音程を変えずに再生速度を調整します" onClick={() => onKeylock(!deck?.keylock)}>MT</button>
      </div>
      </div>
      <TempoPlatter deckId={id} bpm={bpm} trackBpm={trackBpm} rate={rate} positionMs={position}
        playing={deck?.status === "playing"} disabled={!supported("deck.tempo")}
        range={tempoRange} onRange={(range) => { setTempoRange(range); localStorage.setItem(`plumdeck.tempoRange.${id}`, String(range)); window.dispatchEvent(new CustomEvent("plumdeck:controller-tempo-range", { detail: { deck: id, range } })); }} onTempo={onTempo} />
      <button className="dj-eject dj-eject--deck" disabled={monitorOnly ? false : disabled} onClick={onUnload} aria-label={`Eject deck ${id}`} title={monitorOnly ? "Junction Live表示を解除（実デッキへ戻る）" : "デッキから取り出す"}><Upload /></button>
    </div>
    {deck?.lastError && <div className="dj-deck-error" role="alert">{deck.lastError}</div>}
  </article>;
}
