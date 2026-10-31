import { useEffect, useRef, useState } from "react";
import type { PerformanceBeatGrid } from "@/types/performance-metadata";
import { bpmAt, nearestBeat, scaleGrid, setDownbeat, setTempoFrom, shiftGrid, subdivideGrid } from "./beat-grid-math";
import "./beat-grid-editor.css";

export type BeatGrid = PerformanceBeatGrid;
export type BeatGridEditorProps = {
  trackId: number; durationMs: number; positionMs: number; initialGrid: BeatGrid;
  disabled?: boolean; playing?: boolean; hasGrid?: boolean;
  /** One drag step; several can be batched into one render, so the editor
   * applies `totalMs` minus what it already consumed, never just `deltaMs`. */
  shiftRequest?: { sequence: number; deltaMs: number; totalMs: number } | null;
  onPreview: (grid: BeatGrid | null) => void; onSave: (grid: BeatGrid) => Promise<void>; onClose: () => void;
  onAnalyze?: (force: boolean) => Promise<BeatGrid>; onRekordbox?: () => Promise<BeatGrid>;
  onTogglePlay?: () => void;
};

export function BeatGridEditor(props: BeatGridEditorProps) {
  const { durationMs, positionMs, initialGrid, disabled, playing, onPreview, onSave, onClose } = props;
  const [draft, setDraft] = useState(initialGrid);
  const draftRef = useRef(draft);
  const [bpmText, setBpmText] = useState(String(draft.bpm));
  const [anchor, setAnchor] = useState(() => nearestBeat(initialGrid, positionMs));
  const [tempoScope, setTempoScope] = useState<"all" | "from">("all");
  const editingBpm = tempoScope === "from" ? bpmAt(draft, anchor) : draft.bpm;
  useEffect(() => { setBpmText(String(Math.round(editingBpm * 100) / 100)); }, [editingBpm, tempoScope, anchor]);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [changed, setChanged] = useState(false);
  const [gridLocked, setGridLocked] = useState(false);
  const taps = useRef<number[]>([]);
  const [history, setHistory] = useState<{ grid: BeatGrid; anchor: number }[]>([]);
  const [future, setFuture] = useState<{ grid: BeatGrid; anchor: number }[]>([]);
  const alive = useRef(true);
  const consumedShift = useRef(props.shiftRequest?.sequence ?? 0);
  const consumedTotal = useRef(props.shiftRequest?.totalMs ?? 0);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  const locked = Boolean(disabled || busy || gridLocked);
  const publish = (grid: BeatGrid, record = true) => {
    const previous = draftRef.current;
    if (record) { setHistory(old => [...old.slice(-49), { grid: previous, anchor }]); setFuture([]); }
    draftRef.current = grid; setDraft(grid);
    setChanged(true); setError(""); onPreview(grid);
  };
  const apply = (operation: () => BeatGrid) => {
    if (locked) return;
    try { publish(operation()); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  };
  useEffect(() => {
    const request = props.shiftRequest;
    if (!request || request.sequence === consumedShift.current) return;
    consumedShift.current = request.sequence;
    const deltaMs = request.totalMs - consumedTotal.current;
    consumedTotal.current = request.totalMs;
    if (locked) return;
    if (request.deltaMs === 0 && deltaMs === 0) { const previous = draftRef.current; setHistory(old => [...old.slice(-49), { grid: previous, anchor }]); setFuture([]); return; }
    if (deltaMs === 0) return;
    try { const next = shiftGrid(draftRef.current, deltaMs, durationMs); publish(next, false); setAnchor(a => nearestBeat(next, a + deltaMs)); }
    catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  }, [props.shiftRequest]);

  const fetchCandidate = async (kind: "rekordbox" | "analysis" | "reanalysis") => {
    if (locked) return;
    setBusy(kind === "rekordbox" ? "rekordboxを読込中…" : "拍位置を解析中…"); setError("");
    try {
      const grid = kind === "rekordbox" ? await props.onRekordbox!() : await props.onAnalyze!(kind === "reanalysis");
      if (alive.current) { publish(grid); setAnchor(nearestBeat(grid, positionMs)); }
    } catch (e) { if (alive.current) setError(e instanceof Error ? e.message : String(e)); }
    finally { if (alive.current) setBusy(""); }
  };
  const changeBpm = (bpm: number) => apply(() => tempoScope === "from"
    ? setTempoFrom(draft, bpm, anchor, durationMs) : scaleGrid(draft, bpm, anchor, durationMs));
  // rekordbox の「拍の間隔を狭める/広げる」。間隔を狭める＝BPMを上げる。
  const nudgeInterval = (deltaBpm: number) => changeBpm(Math.round((editingBpm + deltaBpm) * 100) / 100);
  const tapTempo = () => {
    if (locked) return;
    const now = performance.now();
    const recent = [...taps.current, now].filter((t) => now - t < 3000);
    taps.current = recent;
    if (recent.length < 2) return;
    const spans = recent.slice(1).map((t, i) => t - recent[i]);
    const average = spans.reduce((sum, span) => sum + span, 0) / spans.length;
    const bpm = Math.round((60_000 / average) * 100) / 100;
    if (bpm >= 20 && bpm <= 300) changeBpm(bpm);
  };
  const moveGrid = (delta: number) => apply(() => { const next = shiftGrid(draft, delta, durationMs); setAnchor(a => nearestBeat(next, a + delta)); return next; });
  const cancel = () => { if (!locked) { onPreview(null); onClose(); } };
  const save = async () => {
    if (locked) return;
    if (!bpmText || !Number.isFinite(Number(bpmText)) || Math.abs(Number(bpmText) - editingBpm) > .006) { setError("BPM入力を確定してください"); return; }
    setBusy("適用中…"); setError("");
    try { await onSave(draft); if (alive.current) { onPreview(null); onClose(); } }
    catch (e) { if (alive.current) setError(e instanceof Error ? e.message : String(e)); }
    finally { if (alive.current) setBusy(""); }
  };
  const source = draft.source === "rekordbox" ? "rekordbox" : draft.source === "analysis" ? "plumdeck解析・小節頭は要確認" : draft.source === "manual" || props.hasGrid ? "手修正" : "未解析・仮グリッド";
  return <div className="dj-grid-editor" role="group" aria-label="ビートグリッド編集">
    <div className="dj-grid-editor__head"><strong>GRID EDIT</strong><span className="dj-grid-editor__source">{source}</span><span className="dj-grid-editor__unsaved">{changed ? "未適用" : ""}</span>
      <button disabled={Boolean(disabled || busy)} aria-label="グリッド編集を閉じる" onClick={cancel}>×</button>
    </div>
    <div className="dj-grid-editor__row">
      <button disabled={locked || !props.onRekordbox} onClick={() => void fetchCandidate("rekordbox")}>rekordboxから読込</button>
      <button disabled={locked || !props.onAnalyze} onClick={() => void fetchCandidate("analysis")}>グリッド解析</button>
      <button disabled={locked || !props.onAnalyze} title="音声から拍位置を再検出。適用するまで保存データは変更しません" onClick={() => void fetchCandidate("reanalysis")}>再解析</button>
      <span className="dj-grid-editor__hint">{draft.beat_times_ms?.length ? `${draft.beat_times_ms.length}拍・実時刻` : "一定テンポ"}</span>
    </div>

    {/* 1段目: 拍位置。rekordbox の「拍位置を全体に左/右にずらす」と同じ並び。 */}
    <div className="dj-grid-editor__row">
      <span className="dj-grid-editor__bars" title="基準拍の位置">{(anchor / 1000).toFixed(3)}s</span>
      <button className="dj-grid-editor__downbeat" disabled={locked} title="現在の再生位置に小節の先頭拍を置く"
        onClick={() => apply(() => { const delta = positionMs - nearestBeat(draft, positionMs); const next = setDownbeat(draft, positionMs, durationMs); setAnchor(a => nearestBeat(next, a + delta)); return next; })}>▮</button>
      <button disabled={locked} title="拍位置を全体に左にずらす（粗く）" onClick={() => moveGrid(-10)}>◀◀◀|||</button>
      <button disabled={locked} title="拍位置を全体に左にずらす（細かく）" onClick={() => moveGrid(-1)}>◀|||</button>
      <button disabled={locked} title="拍位置を全体に右にずらす（細かく）" onClick={() => moveGrid(1)}>|||▶</button>
      <button disabled={locked} title="拍位置を全体に右にずらす（粗く）" onClick={() => moveGrid(10)}>|||▶▶▶</button>
      <button disabled={locked || !history.length} aria-label="元に戻す" title="元に戻す" onClick={() => { const previous = history[history.length - 1]; setHistory(history.slice(0, -1)); setFuture([...future, { grid: draft, anchor }]); publish(previous.grid, false); setAnchor(previous.anchor); }}>↶</button>
      <button disabled={locked || !future.length} aria-label="やり直す" title="やり直す" onClick={() => { const next = future[future.length - 1]; setFuture(future.slice(0, -1)); setHistory([...history, { grid: draft, anchor }]); publish(next.grid, false); setAnchor(next.anchor); }}>↷</button>
    </div>

    {/* 2段目: 拍の間隔＝BPM。狭めると BPM が上がる。 */}
    <div className="dj-grid-editor__row">
      <select className="dj-grid-editor__scope" aria-label="BPMの変更範囲" disabled={locked} value={tempoScope} onChange={e => setTempoScope(e.target.value as "all" | "from")}>
        <option value="all">曲全体</option><option value="from">変更地点から先</option>
      </select>
      {tempoScope === "from" && <button disabled={locked} title="現在位置をテンポの変更地点にする。BPMを入力するまでグリッドは変更しません" onClick={() => setAnchor(Math.max(0, Math.min(positionMs, durationMs - 1)))}>現在位置を変更地点に</button>}
    </div>
    <div className="dj-grid-editor__row">
      <label htmlFor={`bpm-${props.trackId}`}>BPM</label>
      <input id={`bpm-${props.trackId}`} className="dj-grid-editor__number" type="number" min={20} max={300} step={0.01} value={bpmText} disabled={locked}
        onChange={e => { setBpmText(e.target.value); const bpm = Number(e.target.value); if (e.target.value && bpm >= 20 && bpm <= 300) changeBpm(bpm); }} />
      <button disabled={locked} title="拍の間隔を狭める（粗く）" onClick={() => nudgeInterval(.1)}>▶▶|||◀◀</button>
      <button disabled={locked} title="拍の間隔を狭める（細かく）" onClick={() => nudgeInterval(.01)}>▶|||◀</button>
      <button disabled={locked} title="拍の間隔を広げる（細かく）" onClick={() => nudgeInterval(-.01)}>◀|||▶</button>
      <button disabled={locked} title="拍の間隔を広げる（粗く）" onClick={() => nudgeInterval(-.1)}>◀◀|||▶▶</button>
      <button className={gridLocked ? "is-on" : ""} disabled={Boolean(disabled || busy)} aria-pressed={gridLocked}
        title="グリッド編集を有効/無効にする" onClick={() => setGridLocked(value => !value)}>{gridLocked ? "🔒" : "🔓"}</button>
    </div>

    {/* 3段目: テンポの取り直しと適用。 */}
    <div className="dj-grid-editor__row">
      <button disabled={!props.onTogglePlay} onClick={props.onTogglePlay}>{playing ? "Ⅱ" : "▶"}</button>
      <button disabled={locked} title="タップした間隔でBPMを設定する" onClick={tapTempo}>TAP</button>
      <button disabled={locked || editingBpm * 2 > 300} title="選択範囲のテンポを倍にする" onClick={() => tempoScope === "from" ? changeBpm(editingBpm * 2) : apply(() => subdivideGrid(draft, 2, anchor))}>||| × 2</button>
      <button disabled={locked || editingBpm / 2 < 20} title="選択範囲のテンポを半分にする" onClick={() => tempoScope === "from" ? changeBpm(editingBpm / 2) : apply(() => subdivideGrid(draft, .5, anchor))}>||| × ½</button>
      <button disabled title="メトロノーム：この音声エンジン未対応">♪</button>
      <select aria-label="拍子" disabled={locked} value={draft.beats_per_bar} onChange={e => apply(() => ({ ...draft, beats_per_bar: Number(e.target.value), beat_numbers: draft.beat_times_ms?.map((_, i) => i % Number(e.target.value) + 1), source: "manual", confidence: null }))}>{[2,3,4,5,6,7,8,12,16].map(n => <option key={n} value={n}>{n}拍</option>)}</select>
      <span className="dj-grid-editor__hint" title="ドラッグでグリッド全体を移動、Shiftで1/10の微調整、Altで音をスクラッチ。保存・適用までエンジンのグリッドは変更しません">ドラッグ＝グリッド · Shift＝微調整</span>
      <button className="dj-grid-editor__cancel" disabled={Boolean(disabled || busy)} onClick={cancel}>取消</button>
      <button className="dj-grid-editor__save" disabled={Boolean(disabled || busy) || !changed} onClick={() => void save()}>保存・適用</button>
    </div>
    {(busy || error) && <div className="dj-grid-editor__status" role={error ? "alert" : "status"}>{error || busy}</div>}
  </div>;
}
