import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { Disc3, Library, ListMusic, Loader2, Pause, Pin, PinOff, Play, Settings2 } from "lucide-react";
import { enqueueWindowOperation, useAlwaysOnTop } from "@/components/assist/useCompactWindow";
import { assistService, isDesktopShell } from "@/services/assist";
import { playService, type LocalPlaylist } from "@/services/play";
import { currentKey } from "@/services/dj-engine/musical-key";
import type { Track } from "@/types";
import type { DeckId, DeckState } from "@/types/dj-engine";
import { cn } from "@/lib/utils";
import { usePlayPreferences } from "./play-preferences";
import { usePagedResource } from "./usePagedResource";
import {
  COMPACT_AUDIO_SETTINGS_HEIGHT, COMPACT_AUDIO_SETTINGS_WIDTH, COMPACT_LIBRARY_EVENT, COMPACT_MENU_HIDE_MS,
  COMPACT_MENU_LOADED_MS, COMPACT_PLAY_QUERY, COMPACT_WAVE_MAX_HEIGHT, LIBRARY_EVENT, TEMPO_RANGE_EVENT, compactTiers, compactWavesHeight,
  knobLoadDeck, nextTempoRange, remainingText, stepCursor, type CompactTiers,
} from "./compact-play";
import { formatTempoPercent, rangeLabel, TEMPO_RANGES, type TempoRange } from "./tempo-math";
import "./play-compact.css";

function useNarrowWindow(): boolean {
  const [narrow, setNarrow] = useState(() => typeof window !== "undefined" && window.matchMedia(COMPACT_PLAY_QUERY).matches);
  useEffect(() => {
    const query = window.matchMedia(COMPACT_PLAY_QUERY);
    const update = () => setNarrow(query.matches);
    update();
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);
  return narrow;
}

/**
 * Whether Play shows the compact view. It turns on below COMPACT_PLAY_MAX_WIDTH.
 * Audio settings opened from the compact view grow the window until they fit;
 * the view stays compact meanwhile and the window returns to its previous
 * bounds when the settings close.
 */
export function usePlayCompactMode(audioSettingsOpen: boolean): boolean {
  const narrow = useNarrowWindow();
  const [hold, setHold] = useState(false);
  useEffect(() => { if (audioSettingsOpen && narrow) setHold(true); }, [audioSettingsOpen, narrow]);
  const releasing = useRef(false);
  useEffect(() => {
    if (!hold) return;
    // Closed: the cleanup below releases the hold once the old size is back.
    if (!audioSettingsOpen) { if (!releasing.current) setHold(false); return; }
    if (!isDesktopShell()) return () => setHold(false);
    let grown = false;
    releasing.current = true;
    void enqueueWindowOperation(async () => {
      const width = Math.max(window.innerWidth, COMPACT_AUDIO_SETTINGS_WIDTH);
      const height = Math.max(window.innerHeight, COMPACT_AUDIO_SETTINGS_HEIGHT);
      if (width <= window.innerWidth && height <= window.innerHeight) return;
      grown = (await assistService.enterCompactWindow(width, height)).applied;
    }).catch(() => undefined);
    return () => {
      void enqueueWindowOperation(async () => { if (grown) await assistService.exitCompactWindow(); })
        .catch(() => undefined)
        .finally(() => { releasing.current = false; setHold(false); });
    };
  }, [hold, audioSettingsOpen]);
  return narrow || hold;
}

/** What fits below the capped waveforms at the current window height (all off outside the compact view). */
export function useCompactTiers(active: boolean, decks: number): CompactTiers {
  const [height, setHeight] = useState(() => typeof window === "undefined" ? 0 : window.innerHeight);
  useEffect(() => {
    if (!active) return;
    const update = () => setHeight(window.innerHeight);
    update();
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, [active]);
  return active ? compactTiers(height, decks) : { pads: false, library: false };
}

const savedTempoRange = (id: DeckId): TempoRange => {
  const saved = Number(localStorage.getItem(`plumdeck.tempoRange.${id}`)) as TempoRange;
  return TEMPO_RANGES.includes(saved) ? saved : TEMPO_RANGES[2];
};

/** Each deck's tempo range, kept in step with the deck (which owns it) and the controller. */
function useTempoRanges(decks: readonly DeckId[]) {
  const [ranges, setRanges] = useState(() => Object.fromEntries(decks.map((id) => [id, savedTempoRange(id)])) as Partial<Record<DeckId, TempoRange>>);
  useEffect(() => {
    const reload = () => setRanges(Object.fromEntries(decks.map((id) => [id, savedTempoRange(id)])));
    reload();
    // The deck saves the range before these events reach us, from its own control or the controller.
    const later = () => setTimeout(reload, 0);
    window.addEventListener(LIBRARY_EVENT, later);
    window.addEventListener(TEMPO_RANGE_EVENT, later);
    return () => { window.removeEventListener(LIBRARY_EVENT, later); window.removeEventListener(TEMPO_RANGE_EVENT, later); };
  }, [decks.join()]);
  const cycle = (id: DeckId) => {
    const range = nextTempoRange(TEMPO_RANGES, ranges[id] ?? savedTempoRange(id));
    // The hidden deck stores it like a controller TEMPO RANGE press; the controller follows too.
    window.dispatchEvent(new CustomEvent(LIBRARY_EVENT, { detail: { control: "tempoRange", deck: id, value: range } }));
    window.dispatchEvent(new CustomEvent(TEMPO_RANGE_EVENT, { detail: { deck: id, range } }));
  };
  return { ranges, cycle };
}

type CompactAction = { control: string; deck?: DeckId; value: number; pressed?: boolean };
type MenuSource = { kind: "collection" } | { kind: "local"; id: number; name: string };
const COLLECTION: MenuSource = { kind: "collection" };

export function PlayCompactView({ decks, deckState, trackKey, pendingLoads, renderLane, activeDeck, onLoad, onOpenAudioSettings, browseMenu = true }: {
  decks: readonly DeckId[];
  deckState: (id: DeckId) => DeckState | undefined;
  trackKey: (id: DeckId) => string | null | undefined;
  pendingLoads: Partial<Record<DeckId, string>>;
  renderLane: (id: DeckId) => ReactNode;
  activeDeck: DeckId;
  onLoad: (deck: DeckId, track: Track) => void;
  onOpenAudioSettings: () => void;
  /** False while the library is on screen below: the BROWSE knob drives it instead. */
  browseMenu?: boolean;
}) {
  const tempo = useTempoRanges(decks);
  const pin = useAlwaysOnTop(true);
  const { keyNotation } = usePlayPreferences();
  const states = Object.fromEntries(decks.map((id) => [id, deckState(id)])) as Partial<Record<DeckId, DeckState | undefined>>;
  return <div className="dj-compact-view" style={{ maxHeight: compactWavesHeight(decks.length), "--compact-wave-max": `${COMPACT_WAVE_MAX_HEIGHT}px` } as CSSProperties}>
    <div className="dj-compact-bar">
      <span className="dj-compact-title"><Disc3 />PLAY</span>
      {pin.error && <span className="dj-compact-warning" title={pin.error}>最前面に固定できませんでした</span>}
      <div className="dj-compact-actions">
        {pin.supported && <button type="button" className={cn("dj-button", pin.alwaysOnTop && "is-on")} aria-pressed={pin.alwaysOnTop}
          title={pin.alwaysOnTop ? "最前面の固定を解除" : "ほかのアプリより手前に固定"} onClick={() => pin.setAlwaysOnTop(!pin.alwaysOnTop)}>
          {pin.alwaysOnTop ? <PinOff /> : <Pin />}<span>{pin.alwaysOnTop ? "固定中" : "最前面"}</span></button>}
        <button type="button" className="dj-button" title="オーディオ設定（設定が収まるまでウィンドウを広げます）" onClick={onOpenAudioSettings}><Settings2 /><span>オーディオ</span></button>
      </div>
    </div>
    <div className="dj-compact-decks" style={{ gridTemplateRows: `repeat(${decks.length}, minmax(0, 1fr))` }}>
      {decks.map((id) => {
        const deck = states[id];
        const track = deck?.track;
        const playing = deck?.status === "playing";
        const rate = deck?.rate ?? 1;
        const bpm = deck?.effectiveBpm ?? (track?.bpm ? track.bpm * rate : null);
        const key = currentKey(trackKey(id), Math.round(deck?.keyShift ?? 0), rate, Boolean(deck?.keylock), keyNotation);
        const remaining = track ? remainingText(deck?.positionMs ?? 0, track.durationMs) : "--:--";
        const nearEnd = Boolean(track && track.durationMs - (deck?.positionMs ?? 0) < 30_000 && playing);
        return <div key={id} className={cn("dj-compact-deck", id === activeDeck && "is-active")}
          style={{ "--deck-accent": id === "A" || id === "C" ? "var(--dj-blue)" : "var(--dj-orange)" } as CSSProperties}>
          <div className="dj-compact-status">
            <b className="dj-compact-letter">{id}</b>
            <i className={cn("dj-compact-state", playing && "is-playing")} aria-label={playing ? "再生中" : "停止中"}>{playing ? <Play /> : <Pause />}</i>
            <span className="dj-compact-track" title={track ? `${track.title ?? ""}${track.artist ? ` — ${track.artist}` : ""}` : undefined}>
              {pendingLoads[id] ? <>読み込み中… {pendingLoads[id]}</> : track ? <>{track.title || "タイトルなし"}{track.artist && <small>{track.artist}</small>}</> : <em>曲が入っていません</em>}
            </span>
            <span className={cn("dj-compact-time dj-num", nearEnd && "is-ending")}>{remaining}</span>
            <span className="dj-compact-bpm dj-num">{bpm ? bpm.toFixed(1) : "—"}<small>BPM</small></span>
            <span className={cn("dj-compact-pitch dj-num", Math.abs(rate - 1) >= 0.0001 && "is-shifted")} title="テンポの変化量">{formatTempoPercent(rate, tempo.ranges[id] ?? TEMPO_RANGES[2])}</span>
            <button type="button" className="dj-compact-range" title="テンポの変化幅（押すたびに ±6 → ±10 → ±16 → WIDE）"
              aria-label={`DECK ${id} のテンポ変化幅 ${rangeLabel(tempo.ranges[id] ?? TEMPO_RANGES[2])}`} onClick={() => tempo.cycle(id)}>{rangeLabel(tempo.ranges[id] ?? TEMPO_RANGES[2])}</button>
            <span className="dj-compact-key">{key.text ? `${key.approximate ? "≈" : ""}${key.text}` : "—"}</span>
          </div>
          {renderLane(id)}
        </div>;
      })}
    </div>
    {browseMenu && <CompactBrowseMenu decks={decks} states={states} activeDeck={activeDeck} onLoad={onLoad} />}
  </div>;
}

/** Overlay list driven by the DDJ BROWSE knob: turn to move, press (or LOAD) to load. */
function CompactBrowseMenu({ decks, states, activeDeck, onLoad }: {
  decks: readonly DeckId[];
  states: Partial<Record<DeckId, DeckState | undefined>>;
  activeDeck: DeckId;
  onLoad: (deck: DeckId, track: Track) => void;
}) {
  const [visible, setVisible] = useState(false);
  const [level, setLevel] = useState<"sources" | "tracks">("tracks");
  const [source, setSource] = useState<MenuSource>(COLLECTION);
  const [cursor, setCursorState] = useState(0);
  const [sourceCursor, setSourceCursorState] = useState(0);
  // MIDI arrives in bursts between renders: read the newest values from refs so
  // fast knob turns do not lose steps.
  const live = useRef({ visible: false, cursor: 0, sourceCursor: 0 });
  const setCursor = (value: number) => { live.current.cursor = value; setCursorState(value); };
  const setSourceCursor = (value: number) => { live.current.sourceCursor = value; setSourceCursorState(value); };
  const show = (value: boolean) => { live.current.visible = value; setVisible(value); };
  const [notice, setNotice] = useState<string | null>(null);
  const hideTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const playlists = usePagedResource({ key: "compact-playlists", fetchPage: (offset, limit) => playService.localPlaylists(limit, offset), itemKey: (item: LocalPlaylist) => item.id });
  const tracks = usePagedResource<Track>({
    key: `compact-tracks:${source.kind === "local" ? source.id : "collection"}`,
    fetchPage: (offset, limit) => source.kind === "local" ? playService.localPlaylistTracks(source.id, limit, offset) : playService.tracksPage({ offset, limit }),
    itemKey: (track) => (track as Track & { setlist_track_id?: number }).setlist_track_id ?? track.id,
  });
  const sources: MenuSource[] = [COLLECTION, ...playlists.items.map((item) => ({ kind: "local" as const, id: item.id, name: item.name }))];
  const knobDeck = knobLoadDeck(activeDeck, decks, states);

  const schedule = (ms = COMPACT_MENU_HIDE_MS) => {
    if (hideTimer.current) clearTimeout(hideTimer.current);
    hideTimer.current = setTimeout(() => { show(false); setNotice(null); }, ms);
  };
  useEffect(() => () => { if (hideTimer.current) clearTimeout(hideTimer.current); }, []);

  const load = (deck: DeckId | null, track: Track | undefined) => {
    if (!track) { setNotice("曲を選んでください"); schedule(); return; }
    if (!deck) { setNotice("すべてのデッキが再生中です。止めてからロードしてください"); schedule(); return; }
    if (!track.filepath) { setNotice("この曲はファイルが見つかりません"); schedule(); return; }
    onLoad(deck, track);
    setNotice(`DECK ${deck} にロードしました`);
    schedule(COMPACT_MENU_LOADED_MS);
  };

  const handle = useRef<(action: CompactAction) => void>(() => undefined);
  handle.current = (action) => {
    // Audio settings or another dialog has the focus; the knob must not act behind it.
    if (document.querySelector('[role="dialog"]')) return;
    const wasVisible = live.current.visible;
    show(true); setNotice(null); schedule();
    const rows = level === "sources" ? sources.length : tracks.items.length;
    if (action.control === "browse" || action.control === "previous" || action.control === "next") {
      // The first turn only opens the menu at the remembered row.
      if (!wasVisible) return;
      const resource = level === "sources" ? playlists : tracks;
      if (resource.error) { resource.retry(); return; }
      const delta = action.control === "previous" ? -1 : action.control === "next" ? 1 : action.value;
      if (level === "sources") {
        const next = stepCursor(live.current.sourceCursor, delta, rows);
        setSourceCursor(next);
        if (next >= rows - 3 && playlists.hasMore) playlists.loadMore();
      } else {
        const next = stepCursor(live.current.cursor, delta, rows);
        setCursor(next);
        if (next >= rows - 5 && tracks.hasMore) tracks.loadMore();
      }
      return;
    }
    if (action.control === "back") {
      if (level === "tracks") {
        setLevel("sources");
        setSourceCursor(Math.max(0, sources.findIndex((item) => item.kind === source.kind && (item.kind === "collection" || source.kind === "local" && item.id === source.id))));
      }
      return;
    }
    if (action.control === "browsePress" || action.control === "load") {
      if (!wasVisible) return;
      if (level === "sources") {
        const chosen = sources[live.current.sourceCursor];
        if (!chosen) return;
        setSource(chosen); setLevel("tracks"); setCursor(0);
        return;
      }
      load(action.control === "load" && action.deck ? action.deck : knobDeck, tracks.items[live.current.cursor]);
    }
  };
  useEffect(() => {
    const receive = (event: Event) => handle.current((event as CustomEvent<CompactAction>).detail);
    window.addEventListener(COMPACT_LIBRARY_EVENT, receive);
    return () => window.removeEventListener(COMPACT_LIBRARY_EVENT, receive);
  }, []);

  useLayoutEffect(() => {
    if (!visible) return;
    listRef.current?.querySelector<HTMLElement>("[data-cursor='true']")?.scrollIntoView({ block: "nearest" });
  }, [visible, level, cursor, sourceCursor]);

  if (!visible) return null;
  const sourceName = source.kind === "local" ? source.name : "Collection";
  const page = level === "sources" ? playlists : tracks;
  return <div className="dj-compact-menu" role="listbox" aria-label="ブラウズメニュー"
    onPointerMove={() => schedule()}>
    <div className="dj-compact-menu-head">
      {level === "sources" ? <><Library />プレイリストを選ぶ</> : <><ListMusic />{sourceName}<small>{tracks.total ? `${Math.min(cursor + 1, tracks.items.length)} / ${tracks.total.toLocaleString()}` : ""}</small></>}
    </div>
    <div ref={listRef} className="dj-compact-menu-list">
      {level === "sources"
        ? sources.map((item, index) => <button type="button" key={item.kind === "local" ? item.id : "collection"} data-cursor={index === sourceCursor} role="option" aria-selected={index === sourceCursor}
          className={cn("dj-compact-menu-row", index === sourceCursor && "is-cursor")}
          onClick={() => { setSourceCursor(index); setSource(item); setLevel("tracks"); setCursor(0); schedule(); }}>
          {item.kind === "collection" ? <Library /> : <ListMusic />}<span>{item.kind === "local" ? item.name : "Collection（すべての曲）"}</span>
        </button>)
        : tracks.items.map((track, index) => <button type="button" key={`${(track as Track & { setlist_track_id?: number }).setlist_track_id ?? track.id}:${index}`} data-cursor={index === cursor} role="option" aria-selected={index === cursor}
          className={cn("dj-compact-menu-row", index === cursor && "is-cursor")}
          onClick={() => { setCursor(index); schedule(); }} onDoubleClick={() => load(knobDeck, track)}>
          <span>{track.title || "タイトルなし"}<small>{track.artist || ""}</small></span>
          <b className="dj-num">{track.bpm ? track.bpm.toFixed(1) : "—"}</b>
        </button>)}
      {page.loading && <div className="dj-compact-menu-note"><Loader2 className="animate-spin" />読み込み中…</div>}
      {page.error && <div className="dj-compact-menu-note">読み込めませんでした。もう一度回すと再試行します</div>}
      {!page.loading && !page.error && level === "tracks" && !tracks.items.length && <div className="dj-compact-menu-note">曲がありません。BACK でプレイリストを選べます</div>}
    </div>
    <div className={cn("dj-compact-menu-foot", notice && "is-notice")}>
      {notice ?? (level === "sources" ? "回して選ぶ・押して開く" : knobDeck ? `押すと DECK ${knobDeck} にロード ・ LOAD ボタンでそのデッキへ ・ BACK でプレイリスト一覧` : "すべてのデッキが再生中です")}
    </div>
  </div>;
}
