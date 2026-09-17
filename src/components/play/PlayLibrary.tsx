import { memo, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { ChevronDown, ChevronRight, Copy, Disc3, Folder, History, Library, ListMusic, Loader2, Pencil, Plus, Radio, Search, Sparkles, Trash2 } from "lucide-react";
import { playService, type HistoryTrack, type LocalPlaylist, type LocalPlaylistTrack, type MirrorPlaylist, type MirrorSource, type RecordingEntry } from "@/services/play";
import type { Track } from "@/types";
import { DECK_IDS, type DeckId } from "@/types/dj-engine";
import { nativeDropElement } from "@/services/deck-drop";
import { djEngineClient } from "@/services/dj-engine/client";
import { usePlayerStore } from "@/stores/playerStore";
import { cn } from "@/lib/utils";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { artworkUrl, useTrackVisuals } from "./useTrackVisuals";
import { formatTime } from "./SoftwareDeck";
import { usePagedResource } from "./usePagedResource";
import { VirtualTrackList, type BrowserTrack } from "./VirtualTrackList";
import { nextSort, sortTracks, type SortState } from "./track-sort";
import { ResizeHandle } from "./ResizeHandle";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { workflowsService } from "@/services/workflows";
import "./play-library.css";
import { SamplerCollection } from "./SamplerCollection";
import { samplerLibrary, sampleDrag } from "@/services/sampler-library";
import { sampler } from "@/services/dj-engine/sampler";
import { usePlayActionDrop, usePlayTrackDrag } from "./PlayDragDrop";

type Source = "collection" | "sampler" | "local" | "mirror" | "history" | "recordings";
type MirrorPageState = { items: MirrorPlaylist[]; total: number; hasMore: boolean; loading: boolean; error?: string };
type MirrorFlatRow = { kind: "item"; item: MirrorPlaylist; depth: number } | { kind: "more" | "loading" | "error"; parent: string | null; depth: number };
type PlaylistDialogState = { kind: "create" } | { kind: "rename" | "delete"; item: LocalPlaylist };
const PAGE = 100;

function useDebounced(value: string, delay = 180) { const [result, setResult] = useState(value); useEffect(() => { const timer = window.setTimeout(() => setResult(value), delay); return () => window.clearTimeout(timer); }, [value, delay]); return result; }

export const PlayLibrary = memo(function PlayLibrary({ activeDeck, seedTrackId, onLoad, cueOverrides, cueRevision, cueImportControl }: { activeDeck: DeckId; seedTrackId: number | null; onLoad: (deck: DeckId, track: Track) => void; cueOverrides?: Record<number, (number | null)[]>; cueRevision?: number; cueImportControl?: import("react").ReactNode }) {
  const [source, setSource] = useState<Source>("collection");
  const samples = useSyncExternalStore(samplerLibrary.subscribe, samplerLibrary.snapshot);
  const [query, setQuery] = useState(""); const debouncedQuery = useDebounced(query.trim());
  // Collection はサーバ側でライブラリ全体を並べ替える。プレイリストや履歴は読み込み済みの行を並べ替える。
  const [sort, setSort] = useState<SortState>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [localPlaylistId, setLocalPlaylistId] = useState<number | null>(null);
  const [sources, setSources] = useState<MirrorSource[]>([]); const [mirrorSource, setMirrorSource] = useState<string | null>(null); const [mirrorPlaylist, setMirrorPlaylist] = useState<MirrorPlaylist | null>(null);
  const [mirrorPages, setMirrorPages] = useState<Record<string, MirrorPageState>>({}); const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [history, setHistory] = useState<HistoryTrack[]>([]); const [recordings, setRecordings] = useState<RecordingEntry[]>([]);
  const [rightOpen, setRightOpen] = useState(true); const [rightMode, setRightMode] = useState<"recommend" | "search">("recommend"); const [rightQuery, setRightQuery] = useState(""); const debouncedRightQuery = useDebounced(rightQuery.trim());
  const [notice, setNotice] = useState<string | null>(null);
  const [playlistDialog, setPlaylistDialog] = useState<PlaylistDialogState | null>(null);
  const [dialogName, setDialogName] = useState("");
  const [dialogPending, setDialogPending] = useState(false);
  const [dialogError, setDialogError] = useState<string | null>(null);
  const mirrorGeneration = useRef(0);
  const importTargetRef = useRef<{ kind: "collection" | "local_playlist"; id?: number } | null>({ kind: "collection" });
  const collection = usePagedResource({ key: `collection:${debouncedQuery}:${sort?.field ?? ""}:${sort?.direction ?? ""}`, enabled: source === "collection", fetchPage: (offset, limit) => playService.tracksPage({ q: debouncedQuery || undefined, offset, limit, sort: sort?.field, order: sort?.direction }), itemKey: (track: Track) => track.id });
  const playlists = usePagedResource({ key: "local-playlists", fetchPage: (offset, limit) => playService.localPlaylists(limit, offset), itemKey: (item: LocalPlaylist) => item.id });
  const localTracks = usePagedResource({ key: `local:${localPlaylistId ?? "none"}`, enabled: source === "local" && localPlaylistId !== null, fetchPage: (offset, limit) => playService.localPlaylistTracks(localPlaylistId!, limit, offset), itemKey: (track) => track.setlist_track_id });
  const mirrorTracks = usePagedResource({ key: `mirror:${mirrorSource}:${mirrorPlaylist?.external_id ?? "none"}`, enabled: source === "mirror" && Boolean(mirrorSource && mirrorPlaylist), fetchPage: (offset, limit) => playService.mirrorPlaylistTracksPage(mirrorSource!, mirrorPlaylist!.external_id, limit, offset), itemKey: (track) => `${track.position}:${track.local_track_id ?? track.source_filepath}` });
  const rightRecommend = usePagedResource({ key: `right-recommend:${seedTrackId ?? "none"}`, enabled: rightOpen && rightMode === "recommend" && Boolean(seedTrackId), pageSize: 50, fetchPage: (offset, limit) => playService.recommendationsPage(seedTrackId!, { offset, limit }), itemKey: (track: Track) => track.id });
  const rightSearch = usePagedResource({ key: `right-search:${debouncedRightQuery}`, enabled: rightOpen && rightMode === "search" && Boolean(debouncedRightQuery), pageSize: 50, fetchPage: (offset, limit) => playService.tracksPage({ q: debouncedRightQuery, offset, limit }), itemKey: (track: Track) => track.id });

  useEffect(() => { let live = true; void playService.sources().then((rows) => { if (live) { setSources(rows); setMirrorSource((old) => old ?? rows[0]?.id ?? null); } }).catch(() => undefined); return () => { live = false; }; }, []);
  useEffect(() => { let live = true; if (source === "history") void playService.history().then((rows) => { if (live) setHistory(rows); }); if (source === "recordings") void playService.recordings().then((rows) => { if (live) setRecordings(rows); }); return () => { live = false; }; }, [source]);
  useEffect(() => {
    importTargetRef.current = source === "collection" ? { kind: "collection" }
      : source === "local" && localPlaylistId ? { kind: "local_playlist", id: localPlaylistId }
      : null;
  }, [source, localPlaylistId]);
  useEffect(() => {
    const refresh = () => { collection.reload(); localTracks.reload(); playlists.reload(); };
    window.addEventListener("plumdeck:import-tracks-updated", refresh);
    return () => window.removeEventListener("plumdeck:import-tracks-updated", refresh);
  }, [collection.reload, localTracks.reload, playlists.reload]);
  useEffect(() => {
    let disposed = false;
    let cleanup: (() => void) | undefined;
    void Promise.resolve().then(() => getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      document.querySelectorAll("[data-native-drop-active]").forEach((node) => node.removeAttribute("data-native-drop-active"));
      if (payload.type === "leave") return;
      const dropNode = nativeDropElement(document, payload.position.x, payload.position.y, window.devicePixelRatio || 1);
      const draggedSample = sampleDrag.current();
      if (draggedSample) {
        const pad = dropNode?.closest<HTMLElement>("[data-sampler-slot]");
        if (payload.type === "drop" && pad?.dataset.samplerEnabled === "true" && sampleDrag.claim()) {
          void sampler.load(Number(pad.dataset.samplerSlot), draggedSample.path).catch(error => setNotice(String(error)));
        }
        if (payload.type === "drop") sampleDrag.end();
        return;
      }
      const sampleTarget = dropNode?.closest<HTMLElement>("[data-sampler-collection]");
      if (sampleTarget) {
        if (payload.type === "enter" || payload.type === "over") sampleTarget.setAttribute("data-native-drop-active", "true");
        const paths = payload.type === "drop" ? payload.paths : [];
        if (payload.type === "drop" && paths.length) {
          try {
            const result = samplerLibrary.add(paths);
            setNotice(`${result.accepted}音源を登録しました（解析なし）${result.skipped ? `・非対応${result.skipped}件をスキップ` : ""}`);
          } catch (error) { setNotice(String(error)); }
        }
        return;
      }
      const target = dropNode?.closest<HTMLElement>("[data-import-target-kind],[data-import-reject],.dj-library-center,.dj-local-playlist");
      if (payload.type === "enter" || payload.type === "over") {
        target?.setAttribute("data-native-drop-active", "true");
        return;
      }
      if (payload.type !== "drop") return;
      // An internal HTML track drag has no native file paths. It belongs to the
      // deck/playlist DOM handlers and must never produce an import rejection.
      if (payload.paths.length === 0) return;
      if (!target || target.dataset.importReject || target.classList.contains("dj-library-center") && !importTargetRef.current) {
        setNotice("この場所には取り込めません。Collectionまたはplumdeckプレイリストへドロップしてください。");
        return;
      }
      const captured = target.dataset.importTargetKind === "local_playlist"
        ? { kind: "local_playlist" as const, id: Number(target.dataset.importTargetId) }
        : target.classList.contains("dj-local-playlist")
          ? { kind: "local_playlist" as const, id: Number(target.dataset.importTargetId) }
          : importTargetRef.current ?? { kind: "collection" as const };
      if (captured.kind === "local_playlist" && !Number.isInteger(captured.id)) {
        setNotice("追加先プレイリストを特定できませんでした");
        return;
      }
      setNotice(`${payload.paths.length}件を受け付けました。バックグラウンドで解析します…`);
      void workflowsService.createImport(payload.paths, captured, "native_file_drop").then(() => {
        setNotice(`取込ジョブを開始しました（追加先: ${captured.kind === "collection" ? "Collection" : `Playlist #${captured.id}`}）`);
        window.dispatchEvent(new Event("plumdeck:imports-changed"));
      }).catch((error) => setNotice(error instanceof Error ? error.message : String(error)));
    })).then((unlisten) => { if (disposed) unlisten(); else cleanup = unlisten; }).catch(() => undefined);
    return () => {
      disposed = true;
      cleanup?.();
    };
  }, []);

  const mirrorKey = (parent: string | null, sourceId = mirrorSource) => `${sourceId ?? "none"}:${parent ?? "__root__"}`;
  const loadMirror = async (parent: string | null, append = false) => {
    if (!mirrorSource) return; const sourceId = mirrorSource; const generation = mirrorGeneration.current; const key = mirrorKey(parent, sourceId); const current = mirrorPages[key]; if (current?.loading || append && !current?.hasMore) return;
    setMirrorPages((old) => ({ ...old, [key]: { items: append ? old[key]?.items ?? [] : [], total: old[key]?.total ?? 0, hasMore: true, loading: true } }));
    try { const page = await playService.mirrorTreePage(sourceId, parent, PAGE, append ? current?.items.length ?? 0 : 0); if (mirrorGeneration.current !== generation) return; setMirrorPages((old) => ({ ...old, [key]: { items: append ? [...(old[key]?.items ?? []), ...page.items] : page.items, total: page.total, hasMore: page.has_more, loading: false } })); }
    catch (cause) { if (mirrorGeneration.current === generation) setMirrorPages((old) => ({ ...old, [key]: { ...(old[key] ?? { items: [], total: 0, hasMore: true }), loading: false, error: cause instanceof Error ? cause.message : String(cause) } })); }
  };
  useEffect(() => { mirrorGeneration.current++; setMirrorPages({}); setExpanded(new Set()); setMirrorPlaylist(null); if (mirrorSource) void loadMirror(null); }, [mirrorSource]);
  const switchSource = (next: Source) => { setSource(next); setSelected(null); if (next !== "collection") setQuery(""); };
  const selectedLocal = playlists.items.find((item) => item.id === localPlaylistId);
  const center = source === "collection" ? collection : source === "local" ? localTracks : mirrorTracks;
  const centerTracks: BrowserTrack[] = source === "history" ? history.map((row) => ({ ...row, id: row.track_id, browser_key: row.event_key }) as BrowserTrack) : source === "mirror" ? mirrorTracks.items.filter((row) => row.resolved && row.local_track_id).map((row) => ({ ...row, id: row.local_track_id!, browser_key: `${row.position}:${row.local_track_id}` }) as BrowserTrack) : source === "recordings" || source === "sampler" ? [] : center.items as BrowserTrack[];
  // Collection はサーバが並べ替えた順で返るので、ここで触らない。
  const sortedTracks = source === "collection" ? centerTracks : sortTracks(centerTracks, sort) as BrowserTrack[];
  const centerTotal = source === "sampler" ? samples.length : source === "history" ? history.length : source === "recordings" ? recordings.length : center.total;
  const title = source === "sampler" ? "Sampler" : source === "collection" ? "Collection" : source === "local" ? selectedLocal?.name ?? "plumdeck Playlist" : source === "mirror" ? mirrorPlaylist?.name ?? "Rekordbox Mirror" : source === "history" ? "プレイ履歴" : "録音";
  const mutate = async (task: () => Promise<unknown>, after: () => void, success?: string) => { try { await task(); after(); if (success) setNotice(success); } catch (cause) { setNotice(cause instanceof Error ? cause.message : String(cause)); } };
  const openPlaylistDialog = (next: PlaylistDialogState) => { setPlaylistDialog(next); setDialogName(next.kind === "rename" ? next.item.name : ""); setDialogError(null); };
  const submitPlaylistDialog = async () => {
    if (!playlistDialog || dialogPending) return;
    const name = dialogName.trim();
    if (playlistDialog.kind !== "delete" && !name) { setDialogError("プレイリスト名を入力してください"); return; }
    setDialogPending(true); setDialogError(null);
    try {
      if (playlistDialog.kind === "create") { await playService.createLocalPlaylist(name); setNotice("プレイリストを作成しました"); }
      else if (playlistDialog.kind === "rename") await playService.renameLocalPlaylist(playlistDialog.item.id, name);
      else {
        const deletedId = playlistDialog.item.id;
        await playService.deleteLocalPlaylist(deletedId);
        setLocalPlaylistId((current) => { if (current === deletedId) { setSource("collection"); return null; } return current; });
      }
      playlists.reload(); setPlaylistDialog(null);
    } catch (cause) { setDialogError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setDialogPending(false); }
  };
  const addTrack = (track: Track, playlistId: number) => {
    void (async () => {
      try {
        const created = await playService.addLocalPlaylistTrack(playlistId, track.id);
        if (localPlaylistId === playlistId && created?.setlist_track_id) {
          localTracks.insert({ ...track, setlist_track_id: created.setlist_track_id, position: localTracks.items.length } as LocalPlaylistTrack);
        }
        setNotice("プレイリストに追加しました");
        playlists.reload();
      } catch (cause) { setNotice(cause instanceof Error ? cause.message : String(cause)); }
    })();
  };
  const copyMirror = async () => { if (!mirrorSource || !mirrorPlaylist) return; try { const result = await playService.copyMirrorPlaylist(mirrorSource, mirrorPlaylist.external_id); playlists.reload(); const resolved = result.resolved ?? result.copied; setNotice(`${result.playlist.name}へ ${result.copied.toLocaleString()}曲をコピー（解決 ${resolved.toLocaleString()} / 未解決スキップ ${result.skipped_unresolved.toLocaleString()}）`); } catch (cause) { setNotice(cause instanceof Error ? cause.message : String(cause)); } };

  const flattenMirror = (parent: string | null, depth = 0): MirrorFlatRow[] => { const page = mirrorPages[mirrorKey(parent)]; const rows: MirrorFlatRow[] = []; for (const item of page?.items ?? []) { rows.push({ kind: "item", item, depth }); if (item.kind === "folder" && expanded.has(item.external_id)) rows.push(...flattenMirror(item.external_id, depth + 1)); } if (page?.loading) rows.push({ kind: "loading", parent, depth }); else if (page?.error) rows.push({ kind: "error", parent, depth }); else if (page?.hasMore) rows.push({ kind: "more", parent, depth }); return rows; };
  const chooseMirror = (item: MirrorPlaylist) => { if (item.kind === "folder") { setExpanded((old) => { const next = new Set(old); next.has(item.external_id) ? next.delete(item.external_id) : next.add(item.external_id); return next; }); if (!mirrorPages[mirrorKey(item.external_id)]) void loadMirror(item.external_id); } else { setMirrorPlaylist(item); switchSource("mirror"); } };
  const controllerLibrary = useRef<(action: { control: string; deck?: DeckId; value: number }) => void>(() => undefined);
  controllerLibrary.current = (action) => {
    if (document.querySelector('[role="dialog"]')) return;
    const index = sortedTracks.findIndex(track => track.id === selected);
    if (["browse", "previous", "next"].includes(action.control)) {
      const delta = action.control === "previous" ? -1 : action.control === "next" ? 1 : action.value;
      const next = Math.max(0, Math.min(sortedTracks.length - 1, (index < 0 ? delta > 0 ? -1 : 0 : index) + delta));
      if (sortedTracks[next]) setSelected(sortedTracks[next].id);
      if (next >= sortedTracks.length - 5 && center.hasMore) center.loadMore();
    } else if (action.control === "load" && action.deck) {
      const track = sortedTracks.find(row => row.id === selected);
      if (track) onLoad(action.deck, track); else setNotice("BROWSEノブで曲を選択してください");
    } else if (action.control === "back") switchSource("collection");
    else if (action.control === "browseView") setRightOpen(value => !value);
    else if (action.control === "related") { setRightOpen(true); setRightMode("recommend"); }
  };
  useEffect(() => {
    const receive = (event: Event) => controllerLibrary.current((event as CustomEvent).detail);
    window.addEventListener("plumdeck:controller-library", receive);
    return () => window.removeEventListener("plumdeck:controller-library", receive);
  }, []);
  const rightPage = rightMode === "recommend" ? rightRecommend : rightSearch;
  return <section className="dj-browser" aria-label="Play library">
    <aside className="dj-library-tree"><div className="dj-tree-heading"><Library /><strong>ブラウザ</strong></div><div className="dj-tree-scroll">
      <SamplerTreeButton selected={source === "sampler"} onSelect={() => switchSource("sampler")} onDrop={(track) => {
          try {
            if (!track.filepath) throw new Error("音源ファイルの場所が分からない曲は登録できません。");
            const result = samplerLibrary.add([track.filepath]);
            setNotice(`「${track.title || track.filepath.split(/[\\/]/).pop()}」をSamplerに登録しました（解析なし）`);
            if (result.skipped) setNotice("この音源はSamplerに登録済みです");
          } catch (error) { setNotice(error instanceof Error ? error.message : String(error)); }
        }} />
      <button data-import-target-kind="collection" className={cn("dj-tree-row", source === "collection" && "is-selected")} onClick={() => switchSource("collection")}><Library /><span>Collection</span><small>{collection.total || ""}</small></button>
      <div className="dj-tree-row dj-tree-section"><span>plumdeck PLAYLISTS · {playlists.total.toLocaleString()}</span><button title="新規プレイリスト" onClick={() => openPlaylistDialog({ kind: "create" })}><Plus /></button></div>
      <LocalPlaylistRows items={playlists.items} selectedId={source === "local" ? localPlaylistId : null} hasMore={playlists.hasMore} loading={playlists.loading} error={playlists.error} onLoadMore={playlists.loadMore} onRetry={playlists.retry} onSelect={(item) => { setLocalPlaylistId(item.id); switchSource("local"); }} onRename={(item) => openPlaylistDialog({ kind: "rename", item })} onDelete={(item) => openPlaylistDialog({ kind: "delete", item })} onDrop={addTrack} />
      <div data-import-reject="mirror" className="dj-tree-row dj-tree-section"><span>REKORDBOX MIRROR · READ ONLY</span></div>
      {sources.map((item) => <div key={item.id}><button className="dj-tree-row" onClick={() => { setMirrorSource(item.id); switchSource("mirror"); }}><Folder /><span>{item.name}</span><small>{item.playlist_count}</small></button>{mirrorSource === item.id && <MirrorTreeRows rows={flattenMirror(null)} expanded={expanded} selectedId={source === "mirror" ? mirrorPlaylist?.external_id : undefined} onChoose={chooseMirror} onLoad={(parent, append) => void loadMirror(parent, append)} />}</div>)}
      <div className="dj-tree-divider" /><button data-import-reject="history" className={cn("dj-tree-row", source === "history" && "is-selected")} onClick={() => switchSource("history")}><History /><span>プレイ履歴</span></button><button data-import-reject="recordings" className={cn("dj-tree-row", source === "recordings" && "is-selected")} onClick={() => switchSource("recordings")}><Radio /><span>録音</span></button>
    </div><div className="dj-tree-footer"><Disc3 /><strong>plumdeck</strong><span>PLAY</span></div></aside>
    <ResizeHandle variable="--d-tree" storageKey="plumdeck.width.tree" label="ブラウザ幅" min={140} max={420} />
    <div className="dj-library-center"><div className="dj-library-toolbar">{cueImportControl}<strong>{title}</strong><span className="dj-track-count">{`${centerTotal.toLocaleString()} 曲`}</span>{source === "collection" && <div className="dj-search"><Search /><input aria-label="Collectionを検索" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search tracks…" /></div>}{source === "mirror" && mirrorPlaylist && <button className="dj-button" onClick={() => void copyMirror()}><Copy />plumdeckへコピー</button>}<button className={cn("dj-button", rightOpen && "is-on")} onClick={() => setRightOpen((value) => !value)}><Sparkles /></button></div>
      {notice && <div className="dj-library-notice" role="status"><span>{notice}</span><button onClick={() => setNotice(null)}>×</button></div>}
      {source === "sampler" ? <SamplerCollection /> : source === "recordings" ? <RecordingRows rows={recordings} /> : <VirtualTrackList resourceKey={`${source}:${debouncedQuery}:${localPlaylistId}:${mirrorSource}:${mirrorPlaylist?.external_id}`} tracks={sortedTracks} total={centerTotal} sort={sort} sortScope={source === "collection" ? "server" : "loaded"} onSort={(field) => setSort((current) => nextSort(current, field))} hasMore={source === "history" ? false : center.hasMore} loading={source === "history" ? false : center.loading} error={source === "history" ? null : center.error} activeDeck={activeDeck} selected={selected} empty={source === "mirror" && !mirrorPlaylist ? "左のRekordboxツリーからプレイリストを選択" : source === "local" && !localPlaylistId ? "左のplumdeckプレイリストを選択" : "曲がありません"} onSelect={setSelected} onLoad={(track) => onLoad(activeDeck, track)} onLoadMore={center.loadMore} onRetry={center.retry} cueOverrides={cueOverrides} cueRevision={cueRevision} onDropTrack={source === "local" && localPlaylistId ? (track) => addTrack(track, localPlaylistId) : undefined} onRemove={source === "local" && localPlaylistId ? (track) => track.setlist_track_id && void mutate(() => playService.removeLocalPlaylistTrack(localPlaylistId, track.setlist_track_id!), () => { localTracks.reload(); playlists.reload(); }) : undefined} />}
    </div>
    {rightOpen && <ResizeHandle variable="--d-recommend" storageKey="plumdeck.width.recommend" label="レコメンド幅" min={160} max={480} invert />}
    {rightOpen && <aside className="dj-recommend"><div className="dj-right-tabs"><button className={rightMode === "recommend" ? "is-on" : ""} onClick={() => setRightMode("recommend")}><Sparkles />RECOMMEND</button><button className={rightMode === "search" ? "is-on" : ""} onClick={() => setRightMode("search")}><Search />SEARCH</button></div>{rightMode === "search" && <div className="dj-right-search"><Search /><input aria-label="右パネル検索" value={rightQuery} onChange={(event) => setRightQuery(event.target.value)} placeholder="Title, artist…" /></div>}<div className="dj-recommend-subtitle"><span>{rightMode === "recommend" ? "NEXT TRACK" : "SEARCH RESULTS"}</span><b>{rightPage.total.toLocaleString()}</b></div><RecommendationList resourceKey={`${rightMode}:${seedTrackId}:${debouncedRightQuery}`} tracks={rightPage.items} loading={rightPage.loading} error={rightPage.error} hasMore={rightPage.hasMore} empty={rightMode === "recommend" ? seedTrackId ? "この曲の候補はありません" : "デッキに曲をロードしてください" : debouncedRightQuery ? "一致する曲はありません" : "検索語を入力してください"} onLoadMore={rightPage.loadMore} onRetry={rightPage.retry} onLoad={(track) => onLoad(activeDeck, track)} /></aside>}
    <PlaylistMutationDialog state={playlistDialog} name={dialogName} pending={dialogPending} error={dialogError} onName={setDialogName} onClose={() => { if (!dialogPending) setPlaylistDialog(null); }} onSubmit={() => void submitPlaylistDialog()} />
  </section>;
});

function RecommendationList({ resourceKey, tracks, loading, error, hasMore, empty, onLoadMore, onRetry, onLoad }: { resourceKey: string; tracks: Track[]; loading: boolean; error: string | null; hasMore: boolean; empty: string; onLoadMore: () => void; onRetry: () => void; onLoad: (track: Track) => void }) { const ref = useRef<HTMLDivElement>(null); const [view, setView] = useState({ top: 0, height: 300 }); const row = 44; const start = Math.max(0, Math.floor(view.top / row) - 5); const end = Math.min(tracks.length, Math.ceil((view.top + view.height) / row) + 5); useEffect(() => { if (ref.current) ref.current.scrollTop = 0; setView((old) => ({ ...old, top: 0 })); }, [resourceKey]); useEffect(() => { const node = ref.current; if (!node) return; const observer = new ResizeObserver(() => setView((old) => ({ ...old, height: node.clientHeight }))); observer.observe(node); return () => observer.disconnect(); }, []); useEffect(() => { const node = ref.current; if (hasMore && !loading && node && node.scrollHeight <= node.clientHeight + row) onLoadMore(); }, [tracks.length, hasMore, loading, onLoadMore]); return <div ref={ref} className="dj-recommend-scroll" onScroll={(event) => { const node = event.currentTarget; setView({ top: node.scrollTop, height: node.clientHeight }); if (node.scrollHeight - node.scrollTop - node.clientHeight < 160) onLoadMore(); }}><div className="dj-recommend-virtual" style={{ height: tracks.length * row }}>{tracks.slice(start, end).map((track, index) => <RecommendationRow key={track.id} dragId={`play-recommend-${resourceKey}-${track.id}`} track={track} top={(start + index) * row} onLoad={() => onLoad(track)} />)}</div>{!tracks.length && !loading && !error && <div className="dj-library-message">{empty}</div>}<div className="dj-page-state">{loading ? <Loader2 className="animate-spin" /> : error ? <button onClick={onRetry}>再試行: {error}</button> : hasMore ? <button onClick={onLoadMore}>さらに表示</button> : null}</div></div>; }
function RecommendationRow({ dragId, track, top, onLoad }: { dragId: string; track: Track; top: number; onLoad: () => void }) { const { data } = useTrackVisuals(track.id); const drag = usePlayTrackDrag(dragId, track); return <div ref={drag.setNodeRef} {...drag.attributes} {...drag.listeners} className={cn("dj-recommend-row", drag.isDragging && "is-dragging")} style={{ transform: `translateY(${top}px)` }} onDoubleClick={() => track.filepath && onLoad()}><div className="dj-recommend-cover">{data?.artwork ? <img src={artworkUrl(data.artwork)} alt="" /> : <Disc3 />}</div><div className="dj-recommend-track"><strong>{track.title || "—"}</strong><span>{track.artist || "—"}</span><small>{track.bpm?.toFixed(2) || "—"} BPM <b>{track.key || "—"}</b></small></div><button disabled={!track.filepath} onClick={onLoad}>↗</button></div>; }

function SamplerTreeButton({ selected, onSelect, onDrop }: { selected: boolean; onSelect: () => void; onDrop: (track: Track) => void }) {
  const drop = usePlayActionDrop("play-sampler-collection", onDrop);
  return <button ref={drop.setNodeRef} data-sampler-collection data-native-drop-active={drop.isOver ? "true" : undefined} className={cn("dj-tree-row", selected && "is-selected")} onClick={onSelect} title="曲や音源ファイルをドロップしてサンプラー用に登録"><Disc3 /><span>Sampler</span></button>;
}
function RecordingRows({ rows }: { rows: RecordingEntry[] }) {
  const [openId, setOpenId] = useState<number | null>(null);
  const [timeline, setTimeline] = useState<import("@/services/workflows").TimelineSegment[]>([]);
  const audio = useRef<HTMLAudioElement>(null);
  const open = (id: number) => { setOpenId(id); setTimeline([]); void workflowsService.timeline(id).then(setTimeline); };
  const isolatePreview = async (element: HTMLAudioElement) => {
    if (element.dataset.previewIsolated === "true") { delete element.dataset.previewIsolated; return; }
    element.pause();
    usePlayerStore.getState().pause();
    const decks = djEngineClient.getState().snapshot?.decks;
    await Promise.allSettled(DECK_IDS.filter((id) => decks?.[id].status === "playing").map((id) => djEngineClient.pause(id)));
    element.dataset.previewIsolated = "true";
    await element.play();
  };
  return <div className="dj-recordings">{rows.map((row) => <div key={row.id} className="!block !h-auto !min-h-9 py-1"><button className="flex w-full items-center gap-2 text-left" onClick={() => openId === row.id ? setOpenId(null) : open(row.id)}><span className="flex-1" title={row.filepath}>{row.title || row.filepath.split(/[\\/]/).pop() || row.filepath}</span><time>{formatTime(row.duration_ms)}</time><b>{row.status.toUpperCase()}</b></button>{openId === row.id && <div className="mt-2 space-y-2 border-t border-white/10 pt-2"><audio ref={audio} controls preload="metadata" className="h-8 w-full" src={playService.recordingAudioUrl(row.id)} onPlay={(event) => void isolatePreview(event.currentTarget)} /><div className="max-h-36 overflow-auto">{timeline.map((segment, index) => <button key={segment.id ?? index} className="flex w-full gap-2 rounded px-2 py-1 text-left hover:bg-white/10" onClick={() => { if (audio.current) { audio.current.currentTime = segment.start_ms / 1000; void isolatePreview(audio.current); } }}><time>{formatTime(segment.start_ms)}</time><span>{segment.artist_snapshot} — {segment.title_snapshot}</span><small>{segment.source}</small></button>)}{!timeline.length && <span className="text-xs text-slate-500">曲目未登録</span>}</div></div>}</div>)}{!rows.length && <div className="dj-library-message">録音はまだありません</div>}</div>;
}

function LocalPlaylistRows({ items, selectedId, hasMore, loading, error, onLoadMore, onRetry, onSelect, onRename, onDelete, onDrop }: { items: LocalPlaylist[]; selectedId: number | null; hasMore: boolean; loading: boolean; error: string | null; onLoadMore: () => void; onRetry: () => void; onSelect: (item: LocalPlaylist) => void; onRename: (item: LocalPlaylist) => void; onDelete: (item: LocalPlaylist) => void; onDrop: (track: Track, playlistId: number) => void }) {
  const ref = useRef<HTMLDivElement>(null); const [top, setTop] = useState(0); const height = 21; const viewport = 150; const start = Math.max(0, Math.floor(top / height) - 3); const end = Math.min(items.length, Math.ceil((top + viewport) / height) + 3);
  useEffect(() => { const node = ref.current; if (hasMore && !loading && node && node.scrollHeight <= node.clientHeight + height) onLoadMore(); }, [items.length, hasMore, loading, onLoadMore]);
  useEffect(() => { ref.current?.querySelectorAll<HTMLElement>(".dj-local-playlist").forEach((node, index) => { const item = items.slice(start, end)[index]; if (item) { node.dataset.importTargetKind = "local_playlist"; node.dataset.importTargetId = String(item.id); } }); }, [items, start, end]);
  return <div ref={ref} className="dj-local-playlists-viewport" onScroll={(event) => { const node = event.currentTarget; setTop(node.scrollTop); if (node.scrollHeight - node.scrollTop - node.clientHeight < height * 5) onLoadMore(); }}><div style={{ height: items.length * height, position: "relative" }}>{items.slice(start, end).map((item, index) => <LocalPlaylistRow key={item.id} item={item} selected={selectedId === item.id} top={(start + index) * height} onSelect={() => onSelect(item)} onRename={() => onRename(item)} onDelete={() => onDelete(item)} onDrop={(track) => onDrop(track, item.id)} />)}</div>{loading && <div className="dj-tree-loading"><Loader2 className="animate-spin" /></div>}{error && <button className="dj-tree-more" onClick={onRetry}>再試行</button>}</div>;
}

function LocalPlaylistRow({ item, selected, top, onSelect, onRename, onDelete, onDrop }: { item: LocalPlaylist; selected: boolean; top: number; onSelect: () => void; onRename: () => void; onDelete: () => void; onDrop: (track: Track) => void }) {
  const drop = usePlayActionDrop(`play-local-playlist-${item.id}`, onDrop);
  return <div ref={drop.setNodeRef} className="dj-local-playlist" data-native-drop-active={drop.isOver ? "true" : undefined} style={{ position: "absolute", insetInline: 0, top }}><button className={cn("dj-tree-row", selected && "is-selected")} onClick={onSelect}><ListMusic /><span>{item.name}</span><small>{item.track_count}</small></button><span className="dj-playlist-actions"><button title="名前変更" onClick={onRename}><Pencil /></button><button title="削除" onClick={onDelete}><Trash2 /></button></span></div>;
}

function MirrorTreeRows({ rows, expanded, selectedId, onChoose, onLoad }: { rows: MirrorFlatRow[]; expanded: Set<string>; selectedId?: string; onChoose: (item: MirrorPlaylist) => void; onLoad: (parent: string | null, append: boolean) => void }) {
  const ref = useRef<HTMLDivElement>(null); const [top, setTop] = useState(0); const rowHeight = 21; const viewport = 180; const start = Math.max(0, Math.floor(top / rowHeight) - 3); const end = Math.min(rows.length, Math.ceil((top + viewport) / rowHeight) + 3);
  return <div ref={ref} className="dj-mirror-tree-viewport" style={{ height: Math.min(viewport, Math.max(rowHeight, rows.length * rowHeight)) }} onScroll={(event) => { const node = event.currentTarget; setTop(node.scrollTop); if (node.scrollHeight - node.scrollTop - node.clientHeight < rowHeight * 2) { const more = [...rows].reverse().find((row) => row.kind === "more"); if (more?.kind === "more") onLoad(more.parent, true); } }}><div style={{ height: rows.length * rowHeight, position: "relative" }}>{rows.slice(start, end).map((row, index) => { const y = (start + index) * rowHeight; if (row.kind === "item") { const open = row.item.kind === "folder" && expanded.has(row.item.external_id); return <button key={`${row.item.source_id}:${row.item.external_id}`} className={cn("dj-tree-row", selectedId === row.item.external_id && "is-selected")} style={{ position: "absolute", top: y, paddingLeft: 14 + row.depth * 12 }} onClick={() => onChoose(row.item)}>{row.item.kind === "folder" ? open ? <ChevronDown /> : <ChevronRight /> : <span className="dj-tree-indent" />}{row.item.kind === "folder" ? <Folder /> : <ListMusic />}<span>{row.item.name}</span><small>{row.item.track_count || ""}{row.item.unmapped_count ? "*" : ""}</small></button>; } return <button key={`${row.kind}:${row.parent}:${y}`} className="dj-tree-more" style={{ position: "absolute", top: y }} disabled={row.kind === "loading"} onClick={() => onLoad(row.parent, row.kind === "more")}>{row.kind === "loading" ? "読み込み中…" : row.kind === "error" ? "再試行" : "さらに表示"}</button>; })}</div></div>;
}

function PlaylistMutationDialog({ state, name, pending, error, onName, onClose, onSubmit }: { state: PlaylistDialogState | null; name: string; pending: boolean; error: string | null; onName: (value: string) => void; onClose: () => void; onSubmit: () => void }) {
  const deleting = state?.kind === "delete";
  return <Dialog open={Boolean(state)} onOpenChange={(open) => { if (!open) onClose(); }}><DialogContent className="dj-playlist-dialog" onEscapeKeyDown={(event) => { if (pending) event.preventDefault(); }} onPointerDownOutside={(event) => { if (pending) event.preventDefault(); }}>
    <DialogHeader><DialogTitle>{state?.kind === "create" ? "plumdeckプレイリストを作成" : state?.kind === "rename" ? "プレイリスト名を変更" : "プレイリストを削除"}</DialogTitle><DialogDescription>{deleting && state ? <>「{state.item.name}」から {state.item.track_count.toLocaleString()}曲のメンバー登録を削除します。音楽ファイル本体は変更されません。</> : "Play画面で管理するローカルplumdeckプレイリストです。Rekordbox Mirrorには影響しません。"}</DialogDescription></DialogHeader>
    {!deleting && <label className="dj-playlist-dialog__field"><span>プレイリスト名</span><input autoFocus aria-label="プレイリスト名" value={name} maxLength={200} disabled={pending} onChange={(event) => onName(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && name.trim()) { event.preventDefault(); onSubmit(); } }} /></label>}
    {error && <div className="dj-playlist-dialog__error" role="alert">{error}</div>}
    <DialogFooter className="dj-playlist-dialog__actions"><button type="button" disabled={pending} onClick={onClose}>取消</button><button type="button" className={deleting ? "is-destructive" : "is-primary"} disabled={pending || !deleting && !name.trim()} onClick={onSubmit}>{pending ? "処理中…" : deleting ? "削除" : state?.kind === "rename" ? "保存" : "作成"}</button></DialogFooter>
  </DialogContent></Dialog>;
}
