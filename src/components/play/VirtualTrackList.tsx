import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { Disc3, Loader2, Trash2 } from "lucide-react";
import type { Track } from "@/types";
import type { DeckId } from "@/types/dj-engine";
import { DeckWaveform } from "./DeckWaveform";
import { artworkUrl, useTrackVisuals } from "./useTrackVisuals";
import { formatTime } from "./SoftwareDeck";
import { performanceMetadataService } from "@/services/performance-metadata";
import type { SortField, SortState } from "./track-sort";
import { formatKey, relationOf, trafficLevel, useKeyDisplay } from "@/lib/key-display";
import { usePlayActionDrop, usePlayTrackDrag } from "./PlayDragDrop";

const ROW_HEIGHT = 24;

export type BrowserTrack = Track & { setlist_track_id?: number; position?: number; browser_key?: string };

/** 並べ替えできる列とヘッダ表記。順序はそのまま表示順。 */
const COLUMNS: { label: string; field?: SortField; title?: string }[] = [
  { label: "#" }, { label: "Key", field: "key", title: "キー（Camelot 順）" }, { label: "Preview" }, { label: "Art" },
  { label: "BPM", field: "bpm" }, { label: "ΔBPM", title: "MASTER デッキの現在 BPM との差" }, { label: "トラックタイトル", field: "title" }, { label: "アーティスト", field: "artist" },
  { label: "Time", field: "duration", title: "再生時間" }, { label: "Genre", field: "genre" }, { label: "操作" },
];

export function VirtualTrackList({ resourceKey, tracks, total, hasMore, loading, error, activeDeck, selected, empty, sort, sortScope = "loaded", onSort, onSelect, onLoad, onLoadMore, onRetry, onRemove, cueOverrides, cueRevision = 0, onDropTrack, baseKey = null, masterBpm = null, playedIds }: {
  resourceKey: string;
  /** MASTER deck's current BPM (tempo applied), for the ΔBPM column. */
  masterBpm?: number | null;
  /** Tracks played in this Play session. */
  playedIds?: ReadonlySet<number>;
  baseKey?: number | null; tracks: BrowserTrack[]; total: number; hasMore: boolean; loading: boolean; error: string | null; activeDeck: DeckId; selected: number | null; empty: string;
  sort?: SortState;
  /** server: ライブラリ全体を並べ替え済み。loaded: 読み込み済みの行だけ。 */
  sortScope?: "server" | "loaded";
  onSort?: (field: SortField) => void;
  onSelect: (id: number) => void; onLoad: (track: Track) => void; onLoadMore: () => void; onRetry: () => void; onRemove?: (track: BrowserTrack) => void;
  /** 編集直後のトラックはこちらを優先して、取り直しを待たずに反映する。 */
  cueOverrides?: Record<number, (number | null)[]>;
  cueRevision?: number;
  /** 開いているプレイリストへの追加。レコメンド/検索からのドロップを受ける。 */
  onDropTrack?: (track: Track) => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [viewport, setViewport] = useState({ top: 0, height: 240 });
  useEffect(() => { const node = scrollRef.current; if (!node) return; const observer = new ResizeObserver(() => setViewport((old) => ({ ...old, height: node.clientHeight }))); observer.observe(node); return () => observer.disconnect(); }, []);
  useEffect(() => { if (scrollRef.current) scrollRef.current.scrollTop = 0; setViewport((old) => ({ ...old, top: 0 })); }, [resourceKey]);
  useEffect(() => { const node = scrollRef.current; if (hasMore && !loading && node && node.scrollHeight <= node.clientHeight + ROW_HEIGHT) onLoadMore(); }, [tracks.length, hasMore, loading, onLoadMore]);
  useEffect(() => {
    const node = scrollRef.current;
    const index = tracks.findIndex(track => track.id === selected);
    if (!node || index < 0) return;
    const top = index * ROW_HEIGHT;
    if (top < node.scrollTop) node.scrollTop = top;
    else if (top + ROW_HEIGHT > node.scrollTop + node.clientHeight) node.scrollTop = top + ROW_HEIGHT - node.clientHeight;
  }, [selected, tracks]);
  const [cuePoints, setCuePoints] = useState<Record<number, (number | null)[]>>({});
  const fetchedCues = useRef(new Set<number>());
  const currentCueRevision = useRef(cueRevision);
  if (currentCueRevision.current !== cueRevision) { currentCueRevision.current = cueRevision; fetchedCues.current.clear(); }
  useEffect(() => { setCuePoints({}); }, [cueRevision]);
  const start = Math.max(0, Math.floor(viewport.top / ROW_HEIGHT) - 8);
  const end = Math.min(tracks.length, Math.ceil((viewport.top + viewport.height) / ROW_HEIGHT) + 8);
  const visibleIds = useMemo(() => tracks.slice(start, end).map((track) => track.id).filter((id) => Number.isFinite(id)), [tracks, start, end]);
  const missingCueIds = visibleIds.filter((id) => !fetchedCues.current.has(id)).join(",");
  const listAlive = useRef(true);
  useEffect(() => { listAlive.current = true; return () => { listAlive.current = false; }; }, []);
  useEffect(() => {
    if (!missingCueIds) return;
    const ids = missingCueIds.split(",").map(Number);
    for (const id of ids) fetchedCues.current.add(id);
    void performanceMetadataService.cuePoints(ids)
      .then((points) => { if (listAlive.current && currentCueRevision.current === cueRevision) setCuePoints((old) => ({ ...old, ...points })); })
      .catch(() => { for (const id of ids) fetchedCues.current.delete(id); });
  }, [missingCueIds, cueRevision]);
  const removeSelected = () => {
    if (!onRemove || selected === null) return false;
    const track = tracks.find((item) => item.id === selected);
    if (!track) return false;
    onRemove(track);
    return true;
  };
  const playlistDrop = usePlayActionDrop(`play-track-list-${resourceKey}`, onDropTrack);

  return <div ref={playlistDrop.setNodeRef} className={`dj-virtual-tracks${playlistDrop.isOver ? " is-drop" : ""}`}>
    <div className="dj-vtrack-header">{COLUMNS.map((column) => {
      const active = column.field && sort?.field === column.field ? sort.direction : null;
      if (!column.field || !onSort) return <span key={column.label}>{column.label}</span>;
      return <span key={column.label} aria-sort={active === "asc" ? "ascending" : active === "desc" ? "descending" : "none"}>
        <button type="button" className={active ? "is-sorted" : undefined}
          title={`${column.title ?? column.label}で並べ替え${active === "asc" ? "（降順へ）" : active === "desc" ? "（既定順へ）" : ""}`}
          onClick={() => onSort(column.field as SortField)}>{column.label}<i aria-hidden>{active === "asc" ? "▲" : active === "desc" ? "▼" : ""}</i></button>
      </span>;
    })}</div>
    <div ref={scrollRef} className="dj-vtrack-scroll" tabIndex={0}
      onKeyDown={(event) => {
        if (!(event.metaKey || event.ctrlKey) || event.key !== "Backspace" && event.key !== "Delete") return;
        if (removeSelected()) event.preventDefault();
      }}
      onScroll={(event) => { const node = event.currentTarget; setViewport({ top: node.scrollTop, height: node.clientHeight }); if (node.scrollHeight - node.scrollTop - node.clientHeight < ROW_HEIGHT * 12) onLoadMore(); }}>
      <div style={{ height: tracks.length * ROW_HEIGHT, position: "relative" }}>{tracks.slice(start, end).map((track, relative) => <VirtualTrackRow baseKey={baseKey} masterBpm={masterBpm} played={Boolean(playedIds?.has(track.id))} key={track.browser_key ?? track.setlist_track_id ?? track.id} dragId={`play-track-${resourceKey}-${track.browser_key ?? track.setlist_track_id ?? track.id}`} track={track} index={start + relative} top={(start + relative) * ROW_HEIGHT} activeDeck={activeDeck} selected={selected === track.id} onSelect={() => { onSelect(track.id); scrollRef.current?.focus({ preventScroll: true }); }} onLoad={() => onLoad(track)} onRemove={onRemove ? () => onRemove(track) : undefined} cues={cueOverrides?.[track.id] ?? cuePoints[track.id]} />)}</div>
      {!tracks.length && !loading && !error && <div className="dj-library-message">{empty}</div>}
      {(loading || error || hasMore) && <div className="dj-page-state">{loading ? <><Loader2 className="animate-spin" />読み込み中…</> : error ? <><span>{error}</span><button onClick={onRetry}>再試行</button></> : <button onClick={onLoadMore}>さらに読み込む</button>}</div>}
    </div>
    <div className="dj-library-footer">
      <span>{sort && sortScope === "loaded" && hasMore
        ? `並べ替えは読み込み済みの ${tracks.length.toLocaleString()} 曲が対象。下までスクロールすると残りも読み込みます`
        : `ドラッグ：ドロップ先のデッキへ · ダブルクリック：DECK ${activeDeck} へ${onRemove ? " · Ctrl/⌘+Delete で選択曲をプレイリストから削除" : ""}${onDropTrack ? " · レコメンド/検索からドロップで追加" : ""}`}</span>
      <span className="dj-num">{total.toLocaleString()} tracks</span></div>
  </div>;
}

/** BPM difference to the MASTER deck, taking half / double time as the same tempo. */
export function deltaBpm(trackBpm: number | null | undefined, masterBpm: number | null | undefined): number | null {
  if (!trackBpm || !masterBpm || trackBpm <= 0 || masterBpm <= 0) return null;
  const candidates = [trackBpm, trackBpm * 2, trackBpm / 2];
  return candidates.reduce((best, bpm) => Math.abs(bpm - masterBpm) < Math.abs(best) ? bpm - masterBpm : best, trackBpm - masterBpm);
}

function VirtualTrackRow({ dragId, track, index, top, activeDeck, selected, onSelect, onLoad, onRemove, cues, baseKey = null, masterBpm = null, played = false }: { dragId: string; track: BrowserTrack; index: number; top: number; activeDeck: DeckId; selected: boolean; onSelect: () => void; onLoad: () => void; onRemove?: () => void; cues?: (number | null)[]; baseKey?: number | null; masterBpm?: number | null; played?: boolean }) {
  const delta = deltaBpm(track.bpm, masterBpm);
  const percent = delta !== null && masterBpm ? Math.abs(delta) / masterBpm * 100 : null;
  const keys = useKeyDisplay();
  const light = trafficLevel(relationOf(baseKey, track.key), keys.level2);
  const { data } = useTrackVisuals(track.id);
  const drag = usePlayTrackDrag(dragId, track);
  return <div ref={drag.setNodeRef} {...drag.attributes} {...drag.listeners} className={`dj-vtrack-row${selected ? " is-selected" : ""}${played ? " is-played" : ""}${drag.isDragging ? " is-dragging" : ""}`} title={played ? "このセッションで再生済み" : undefined} style={{ transform: `translateY(${top}px)` }}
    onClick={onSelect} onDoubleClick={() => track.filepath && onLoad()}>
    <span>{index + 1}</span><span style={keyCellStyle(light)} title={track.key || undefined}>{formatKey(track.key, keys.notation)}</span><span><DeckWaveform trackId={track.id} positionMs={0} durationMs={(track.duration || 0) * 1000} layout="horizontal" side="left" color="cyan" hotCues={cues} compact /></span><span>{data?.artwork ? <img src={artworkUrl(data.artwork)} alt="" /> : <Disc3 />}</span><span>{track.bpm?.toFixed(2) || "—"}</span><span className={`dj-delta-bpm${percent === null ? "" : percent <= 3 ? " is-near" : percent <= 6 ? " is-mid" : " is-far"}`} title={delta === null ? "MASTER デッキがありません" : `MASTER との差 ${delta >= 0 ? "+" : ""}${delta.toFixed(2)} BPM（${percent!.toFixed(1)}%）`}>{delta === null ? "—" : `${delta >= 0 ? "+" : ""}${delta.toFixed(1)}`}</span><span title={track.title || track.filepath}>{track.title || track.filepath || "—"}</span><span>{track.artist || "—"}</span><span>{formatTime((track.duration || 0) * 1000)}</span><span>{track.genre || "—"}</span><span className="dj-vtrack-actions"><button disabled={!track.filepath} title={`Deck ${activeDeck}へロード`} onClick={(event) => { event.stopPropagation(); onLoad(); }}>→{activeDeck}</button>{onRemove && <button title="プレイリストから削除" onClick={(event) => { event.stopPropagation(); onRemove(); }}><Trash2 /></button>}</span>
  </div>;
}

/** Traffic Light: green for same / ±1 / relative, yellow for diagonal / +2 (when enabled). */
export function keyCellStyle(level: 1 | 2 | null): CSSProperties | undefined {
  if (level === 1) return { background: "rgba(34, 197, 94, 0.28)", color: "rgb(187, 247, 208)", borderRadius: 3 };
  if (level === 2) return { background: "rgba(234, 179, 8, 0.24)", color: "rgb(254, 240, 138)", borderRadius: 3 };
  return undefined;
}
