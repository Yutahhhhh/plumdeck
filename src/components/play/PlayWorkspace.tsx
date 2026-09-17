import { ensureAudioReady, OUTPUT_ROUTING_KEY, parseOutputRouting, type OutputRouting } from "@/services/dj-engine/audio-ready";
import { localTrackId } from '@/services/junction/asset-resolver';
import { deckRealtimeStore } from '@/services/dj-engine/deck-realtime-store';
import { junctionLeaseKey } from '@/services/junction/state';
import { junctionState } from '@/services/junction/state';
import { useJunctionTracks } from '@/hooks/useJunctionTracks';
import { useJunction } from '@/hooks/useJunction';
import { junctionCommand } from '@/services/junction/client';
import { JunctionInputDeck } from "./JunctionInputDeck";
import { JunctionTurnBar } from "./JunctionTurnBar";
import { TAIL_LOCK_TEXT } from "@/services/junction/turn-state";
import { BeatFxPanel } from "./BeatFxPanel";
import { memoryAction } from "@/services/dj-engine/memory-cues";
import { jogWeight } from "@/services/dj-engine/jog-weight";
import { sampler } from "@/services/dj-engine/sampler";
import "./sampler.css";
import { useDdj1000 } from "@/hooks/useDdj1000";
import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { AlertTriangle, Circle, Columns3, Disc3, Keyboard, Loader2, Mic, Power, Rows3, Settings2, Square, Volume2 } from "lucide-react";
import { useDjEngine } from "@/hooks/useDjEngine";
import { playService , type RecordingEntry } from "@/services/play";
import { settingsService } from "@/services/settings";
import { performanceMetadataService } from "@/services/performance-metadata";
import { workflowsService } from "@/services/workflows";
import { cuePositions, persistCue, persistLoop } from "@/services/performance-cue-metadata";
import type { Track } from "@/types";
import { DECK_IDS, type DeckId, type MicrophoneSettings, type RecordingState, type TrackDescriptor } from "@/types/dj-engine";
import { savedMicrophoneSettings, saveMicrophoneSettings } from "@/services/dj-engine/audio-settings";
import { usePlayerStore } from "@/stores/playerStore";
import { cn } from "@/lib/utils";
import { PlayLibrary } from "./PlayLibrary";
import { DeckWaveform, type WaveformLayout } from "./DeckWaveform";

import { Fader, SoftwareDeck, formatTime } from "./SoftwareDeck";
import { MixerPanel } from "./MixerPanel";
import { PanelToolbar, PANEL_DEFAULTS, type PanelId, type PanelVisibility } from "./PanelToolbar";
import { FxPanel, FX_UNIT_DEFAULTS, type FxUnitState } from "./FxPanel";
import "./play-workspace.css";
import "./play-density.css";
import { AudioSettings } from "./AudioSettings";
import { RecordingSaveDialog } from "./RecordingSaveDialog";
import { RekordboxCueImportButton } from "./RekordboxCueImportButton";
import { BeatGridEditor } from "./BeatGridEditor";
import type { PerformanceBeatGrid, PerformanceMetadata } from "@/types/performance-metadata";
import { PlayDragDropProvider, usePlayDeckDrop } from "./PlayDragDrop";
import { ApiError } from "@/services/api-client";
import { KeyedTaskQueue } from "@/services/dj-engine/keyed-task-queue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
type GridEditSession = { deck: DeckId; trackId: number; metadata: PerformanceMetadata; initialGrid: PerformanceBeatGrid };
/** 助走の上限。これ以上は曲頭を見失うので、つまみ出せる長さを切る。 */

type HistoryRuntime = { key: string; trackId: number; loadedAt: string; startedAt?: string; playedMs: number; playingSince?: number };

function DeckWaveformLane({ id, expandedPair, children }: { id: DeckId; expandedPair: 'AB' | 'CD' | null; children: ReactNode }) {
  const drop = usePlayDeckDrop(`play-deck-waveform-${id}`, id);
  return <div ref={drop.setNodeRef}
    data-deck={id} data-track-drop-deck={id} data-track-drop-label={`DECK ${id} へロード`} data-track-drop-active={drop.isOver ? "true" : undefined}
    className={cn("dj-lane", expandedPair&&(expandedPair.includes(id)?"dj-lane--expanded":"dj-lane--summary"))}
    style={{ "--deck-accent": id === "A" || id === "C" ? "var(--dj-blue)" : "var(--dj-orange)" } as CSSProperties}>
    {children}
  </div>;
}

function restoredHistory(): [DeckId, HistoryRuntime][] {
  try {
    const value = JSON.parse(sessionStorage.getItem("plumdeck.playHistoryRuntime") ?? "[]");
    return Array.isArray(value) ? value : [];
  } catch {
    sessionStorage.removeItem("plumdeck.playHistoryRuntime");
    return [];
  }
}

export function PlayWorkspace() {
  const { status, state, error, busy, client, start, stop, connect } = useDjEngine();
  useEffect(() => { const timer = setInterval(() => void sampler.poll(), 250); return () => { clearInterval(timer); }; }, []);
  const [deckCount, setDeckCount] = useState<2 | 4>(() => localStorage.getItem("plumdeck.deckCount") === "4" ? 4 : 2);
  const [expandedPair,setExpandedPair]=useState<'AB'|'CD'|null>(null);
  const [waveContrast,setWaveContrast]=useState(()=>Number(localStorage.getItem('plumdeck.waveContrast'))||1);
  const [waveMonochrome,setWaveMonochrome]=useState(()=>localStorage.getItem('plumdeck.waveMonochrome')==='true');
  const [waveDelay,setWaveDelay]=useState(()=>Math.max(-50,Math.min(500,Number(localStorage.getItem('plumdeck.waveDelay'))||0)));
  useEffect(()=>{deckRealtimeStore.setPresentationDelay(waveDelay);},[waveDelay]);
  useEffect(()=>{const escape=(event:KeyboardEvent)=>{if(event.key==='Escape')setExpandedPair(null);};window.addEventListener('keydown',escape);return()=>window.removeEventListener('keydown',escape);},[]);
  const [activeDeck, setActiveDeck] = useState<DeckId>("A");
  const [junctionMonitorDeck, setJunctionMonitorDeck] = useState<DeckId | null>(null);
  const junction = useJunctionTracks();
  const junctionSession = useJunction();
  const junctionInput = junctionSession?.active ? junctionSession.junctionInput : undefined;
  // OUTGOING: the decks still sounding on the next DJ's J accept loops only.
  const outgoing = junctionSession?.active && junctionSession.turn?.signal === "outgoing";
  const tailLock = (deck: DeckId): string | undefined => outgoing && junctionSession?.turn?.tailDecks.includes(deck) ? TAIL_LOCK_TEXT : undefined;
  const updateJunctionMonitorDeck = useCallback((deck: DeckId | null) => {
    setJunctionMonitorDeck(deck);
    void invoke<DeckId | null>('junction_live_monitor_set', {deck}).catch(() => undefined);
  }, []);
  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    void listen<{deck?: unknown}>('junction://live-monitor', (event) => {
      const deck = event.payload?.deck;
      if (deck === null || deck === undefined) setJunctionMonitorDeck(null);
      else if (typeof deck === 'string' && DECK_IDS.includes(deck as DeckId)) setJunctionMonitorDeck(deck as DeckId);
    }).then((stop) => {
      if (disposed) {
        stop();
        return;
      }
      unlisten = stop;
      // Subscribe before reading so an MCP assignment cannot land between the
      // initial getter and event-listener registration.
      void invoke<DeckId | null>('junction_live_monitor_deck').then((deck) => {
        if (!disposed && (deck === null || DECK_IDS.includes(deck))) setJunctionMonitorDeck(deck);
      }).catch(() => undefined);
    }).catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, []);
  useEffect(() => {
    if (junction.visible || !junctionMonitorDeck) return;
    // Junction state is polled. Give a just-arrived MCP attach one poll cycle
    // before treating a non-visible snapshot as authoritative.
    const timer = window.setTimeout(() => updateJunctionMonitorDeck(null), 1_500);
    return () => window.clearTimeout(timer);
  }, [junction.visible, junctionMonitorDeck, updateJunctionMonitorDeck]);
  const [outputDevice, setOutputDevice] = useState(() => localStorage.getItem("plumdeck.djOutputDevice") ?? "");
  const [recordingDir, setRecordingDir] = useState<string>("");
  const [recordingFormat, setRecordingFormat] = useState<string>("");
  useEffect(() => {
    let live = true;
    void settingsService.getAll()
      .then((values) => { if (!live) return; setRecordingDir(values.recording_directory ?? ""); setRecordingFormat(values.recording_format ?? ""); })
      .catch(() => undefined);
    return () => { live = false; };
  }, []);

  const [commandError, setCommandError] = useState<string | null>(null);
  const [shortcuts, setShortcuts] = useState(false);
  const [audioSettingsOpen, setAudioSettingsOpen] = useState(false);
  const [savingRecording, setSavingRecording] = useState<RecordingEntry | null>(null);
  const [gridEdit, setGridEdit] = useState<GridEditSession | null>(null);
  const [cueRevision, setCueRevision] = useState(0);
  const [trackMetadata, setTrackMetadata] = useState<Record<number, PerformanceMetadata>>({});
  // エンジンはキーを持たないので、ロード時のライブラリ値をヘッダ表示用に控えておく。
  const [trackKeys, setTrackKeys] = useState<Record<number, string>>({});
  const [gridPreview, setGridPreview] = useState<PerformanceBeatGrid | null>(null);
  const [gridShift, setGridShift] = useState<{ sequence: number; deltaMs: number } | null>(null);
  const gridOpenRequest = useRef(0);
  const gridSaving = useRef(false);
  const gridOperations = useRef(new KeyedTaskQueue());
  const deckLoadOperations = useRef(new KeyedTaskQueue());
  const deckLoadRequests = useRef<Record<DeckId, number>>({ A: 0, B: 0, C: 0, D: 0 });
  // rekordbox の CUE は「一時停止中に押すとキュー設定・再生中に押すとキューへ復帰」。
  // エンジンはキューポイントを持たないので UI 側で覚える。曲を外したら 0 に戻る。
  const cuePoints = useRef<Record<DeckId, number>>({ A: 0, B: 0, C: 0, D: 0 });
  // 曲頭より前から始めたいとき用の無音の助走。Mixxx は負の再生位置を持てないので、
  // 助走ぶんだけ UI 側で待ってから 0 秒地点を再生する。
  const manualLoopIn = useRef<Record<DeckId, number | null>>({ A: null, B: null, C: null, D: null });
  const [panels, setPanels] = useState<PanelVisibility>(() => {
    try { return { ...PANEL_DEFAULTS, ...JSON.parse(localStorage.getItem("plumdeck.panels") ?? "{}") as Partial<PanelVisibility> }; }
    catch { return PANEL_DEFAULTS; }
  });
  const [fxUnits, setFxUnits] = useState<[FxUnitState, FxUnitState]>(FX_UNIT_DEFAULTS);
  const [compactDecks, setCompactDecks] = useState(() => localStorage.getItem("plumdeck.compactDecks") === "true");
  const [waveformLayout, setWaveformLayout] = useState<WaveformLayout>(() => localStorage.getItem("plumdeck.waveformLayout") === "vertical" ? "vertical" : "horizontal");
  const snapshot = state.snapshot;
  const loadedMetadataIds = [...new Set(DECK_IDS.map(id => snapshot?.decks[id]?.track?.trackId).filter(Boolean))].join(",");
  const playSession = useRef(sessionStorage.getItem("plumdeck.playSession") ?? `play-${crypto.randomUUID()}`);
  const historyKeys = useRef(new Map<DeckId, HistoryRuntime>(restoredHistory()));
  const recordingKey = useRef<string | null>(sessionStorage.getItem("plumdeck.recordingKey"));
  const previousRecording = useRef(snapshot?.recording);
  const snapshotRef = useRef(snapshot);
  const persistenceQueue = useRef<Promise<unknown>>(Promise.resolve());
  const exiting = useRef(false);
  const visibleDecks = useMemo(() => DECK_IDS.slice(0, deckCount), [deckCount]);
  const connected = Boolean(status?.running && client.getSessionId() && snapshot && !state.sessionInvalidated);
  const autoConnectAttempted = useRef(false);
  const microphoneRestoredSession = useRef<string | null>(null);
  const applyingAudio = useRef(false);
  useEffect(() => {
    const session = client.getSessionId();
    if (!connected || !session || !snapshot?.audio.applied || !snapshot.audio.microphone?.available
      || applyingAudio.current || microphoneRestoredSession.current === session) return;
    microphoneRestoredSession.current = session;
    // A reconnect may attach to an already configured engine; preserve its live routing.
    if (snapshot.audio.microphone.deviceId) return;
    const saved = savedMicrophoneSettings();
    if (saved?.deviceId && !junctionState.active()) void client.setMicrophone(saved).catch(cause => setCommandError(`マイク設定を復元できませんでした: ${cause instanceof Error ? cause.message : String(cause)}`));
  }, [client, connected, snapshot?.sessionId, snapshot?.audio.applied, snapshot?.audio.microphone?.available]);
  // 保存先は起動時の環境変数だけでなく、繋がっている間も送り直す。エンジンは
  // 録音のたびに読み直すので、設定を変えたら次の録音から効く。
  useEffect(() => {
    if (!connected || !recordingDir.trim()) return;
    void client.setRecordingDirectory(recordingDir.trim()).catch(() => undefined);
  }, [client, connected, recordingDir]);

  useEffect(() => {
    if (!connected || !recordingFormat.trim()) return;
    void client.setRecordingFormat(recordingFormat.trim())
      .catch((cause) => setCommandError(cause instanceof Error ? cause.message : String(cause)));
  }, [client, connected, recordingFormat]);

  useEffect(() => {
    let live = true;
    for (const id of loadedMetadataIds.split(",").map(Number).filter(id => Number.isSafeInteger(id) && id > 0)) {
      void performanceMetadataService.get(id).then(metadata => {
        if (live) setTrackMetadata(old => old[id]?.revision > metadata.revision ? old : { ...old, [id]: metadata });
      }).catch(() => undefined);
    }
    return () => { live = false; };
  }, [loadedMetadataIds]);

  useEffect(() => {
    if (!status || busy || autoConnectAttempted.current || connected) return;
    autoConnectAttempted.current = true;
    void (status.running ? connect() : status.installed ? start(outputDevice || undefined, recordingDir || undefined) : Promise.resolve());
  }, [busy, connect, connected, outputDevice, start, status]);

  useEffect(() => { sessionStorage.setItem("plumdeck.playSession", playSession.current); }, []);

  const saveRuntime = useCallback(() => {
    if (exiting.current) return;
    sessionStorage.setItem("plumdeck.playHistoryRuntime", JSON.stringify([...historyKeys.current]));
    if (recordingKey.current) sessionStorage.setItem("plumdeck.recordingKey", recordingKey.current);
    else sessionStorage.removeItem("plumdeck.recordingKey");
  }, []);

  useEffect(() => {
    if (!status?.running || snapshot?.audio.applied) return;
    const timer = window.setInterval(() => { void client.refreshSnapshot().catch(() => undefined); }, 500);
    const timeout = window.setTimeout(() => window.clearInterval(timer), 10_000);
    return () => { window.clearInterval(timer); window.clearTimeout(timeout); };
  }, [client, snapshot?.audio.applied, status?.running]);

  useEffect(() => { snapshotRef.current = snapshot; }, [snapshot]);

  const persist = useCallback((task: () => Promise<unknown>) => {
    persistenceQueue.current = persistenceQueue.current.then(task, task);
    return persistenceQueue.current;
  }, []);

  useEffect(() => {
    void persist(() => playService.startSession(playSession.current, deckCount)).catch((e) => setCommandError(String(e)));
  }, [deckCount, persist]);

  const run = useCallback(async (task: () => Promise<unknown>) => {
    setCommandError(null);
    try { await task(); } catch (e) { setCommandError(e instanceof Error ? e.message : String(e)); }
  }, []);

  const finalizeHistory = useCallback((deck: DeckId, reason: string) => {
    const item = historyKeys.current.get(deck);
    if (!item) return Promise.resolve();
    historyKeys.current.delete(deck);
    const endedAt = new Date().toISOString();
    const playedMs = item.playedMs + (item.playingSince ? Math.max(0, Date.now() - item.playingSince) : 0);
    saveRuntime();
    return persist(() => playService.upsertHistory({ event_key: item.key, session_id: playSession.current, deck, track_id: item.trackId, loaded_at: item.loadedAt, ended_at: endedAt, completed: true, played_ms: playedMs, reason }));
  }, [persist, saveRuntime]);

  const finalizeRecording = useCallback(async () => {
    const recording = snapshotRef.current?.recording;
    if (!recording?.active || !recording.path || !recording.startedAt) return;
    const key = recordingKey.current ?? `${playSession.current}:${recording.startedAt}`;
    const stopped = await client.stopRecording() as RecordingState;
    await persist(async () => {
      const saved = await playService.upsertRecording({ recording_key: key, session_id: playSession.current, filepath: stopped.path ?? recording.path!, started_at: stopped.startedAt ?? recording.startedAt!, ended_at: new Date().toISOString(), duration_ms: stopped.sampleRateHz && stopped.frameCount !== undefined ? Math.floor(stopped.frameCount * 1000 / stopped.sampleRateHz) : Math.max(recording.elapsedMs, stopped.elapsedMs), status: stopped.error ? "failed" : "completed", error: stopped.error, sample_rate_hz: stopped.sampleRateHz || null, frame_count: stopped.frameCount ?? null, timeline_quality: stopped.timelineQuality ?? "not_recorded", timeline_dropped_events: stopped.timelineDroppedEvents ?? 0 });
      if (stopped.sampleRateHz && stopped.frameCount !== undefined && stopped.timeline) {
        await workflowsService.saveEngineTimeline(saved.id, { sample_rate_hz: stopped.sampleRateHz, frame_count: stopped.frameCount, dropped_events: stopped.timelineDroppedEvents ?? 0, segments: stopped.timeline });
      }
    });
    recordingKey.current = null;
    saveRuntime();
  }, [client, persist, saveRuntime]);

  useEffect(() => {
    exiting.current = false;
    return () => {
    exiting.current = true;
    if (junctionState.active()) return;
    sessionStorage.removeItem("plumdeck.playSession");
    sessionStorage.removeItem("plumdeck.playHistoryRuntime");
    sessionStorage.removeItem("plumdeck.recordingKey");
    void (async () => {
      await finalizeRecording().catch(() => undefined);
      await Promise.allSettled(DECK_IDS.map((deck) => finalizeHistory(deck, "mode_exit")));
      await persistenceQueue.current.catch(() => undefined);
      await playService.endSession(playSession.current).catch(() => undefined);
    })();
    };
  }, [finalizeHistory, finalizeRecording]);

  const loadTrack = useCallback((deck: DeckId, track: Track) => {
    const junction = junctionState.get();
    if (junction?.active && junction.turn?.signal === "outgoing" && junction.turn.tailDecks.includes(deck)) {
      setCommandError(TAIL_LOCK_TEXT);
      return;
    }
    if (junction?.active && !junction.turn && junction.localPeerId !== junction.performerPeerId && !junction.localPrep) {
      setCommandError("別のDJがプレイ中です。Junctionの手元試聴で準備し、引き継ぎ後にデッキへロードしてください。");
      return;
    }
    const loadRequest = ++deckLoadRequests.current[deck];
    if (track.key) setTrackKeys(old => old[track.id] === track.key ? old : { ...old, [track.id]: track.key });
    cuePoints.current[deck] = 0;
    manualLoopIn.current[deck] = null;
    const descriptor: TrackDescriptor = { musicalKey: track.key || undefined, localTrackId: track.id, trackId: String(track.id), path: track.filepath, durationMs: track.duration * 1000, title: track.title || undefined, artist: track.artist || undefined, bpm: track.bpm || undefined };
    void run(() => deckLoadOperations.current.run(DECK_IDS.indexOf(deck), () => gridOperations.current.run(track.id, async () => {
      if (loadRequest !== deckLoadRequests.current[deck]) return;
      if (!client.getSessionId()) {
        if (!status?.running) await start(outputDevice || undefined, recordingDir || undefined);
        if (!client.getSessionId()) await connect();
      }
      try {
        const audio = await ensureAudioReady(client, outputDevice || undefined, recordingDir || undefined);
        const saved = parseOutputRouting(localStorage.getItem(OUTPUT_ROUTING_KEY));
        if (saved && saved.deviceId === audio.deviceId && (JSON.stringify(saved.routing.masterChannels) !== JSON.stringify(audio.masterChannels) || JSON.stringify(saved.routing.pflChannels) !== JSON.stringify(audio.pflChannels))) {
          await client.setOutputRouting(saved.routing);
        }
      } catch (cause) {
        setAudioSettingsOpen(true);
        throw cause;
      }
      try {
        const metadata = await performanceMetadataService.get(track.id);
        setTrackMetadata(old => ({ ...old, [track.id]: metadata }));
        descriptor.hotCues = cuePositions(metadata, descriptor.durationMs);
        descriptor.bpm = metadata.beat_grid?.bpm ?? descriptor.bpm;
        descriptor.beatgridOffsetMs = metadata.beat_grid?.first_beat_ms;
        descriptor.beatsPerBar = metadata.beat_grid?.beats_per_bar;
        descriptor.beatTimesMs = metadata.beat_grid?.beat_times_ms ?? undefined;
        descriptor.beatNumbers = metadata.beat_grid?.beat_numbers ?? undefined;
        if (metadata.grid_warning) setCommandError(`グリッド注意: ${metadata.grid_warning}`);
      } catch {
        throw new Error("保存済みグリッドを確認できないため、ロードを中止しました。接続を確認して再試行してください。");
      }
      await finalizeHistory(deck, "replaced");
      if (loadRequest !== deckLoadRequests.current[deck]) return;
      const loadLease = junctionLeaseKey();
      await client.load(deck, descriptor);
      const loadGeneration = client.getDeckGeneration(deck);
      const deadline = performance.now() + 15000;
      while (true) {
        if (loadRequest !== deckLoadRequests.current[deck]) return;
        if (loadLease !== junctionLeaseKey() || loadGeneration !== client.getDeckGeneration(deck)) return;
        const state = (await client.refreshSnapshot()).decks[deck];
        if (state.status === "error") throw new Error(state.lastError ?? "曲のロードに失敗しました");
        if (state.track?.trackId === descriptor.trackId && state.status !== "loading") break;
        if (performance.now() >= deadline) { if (loadRequest === deckLoadRequests.current[deck] && loadLease === junctionLeaseKey() && loadGeneration === client.getDeckGeneration(deck)) await client.unload(deck); throw new Error("曲のロードがタイムアウトしました。再試行してください。"); }
        await new Promise(resolve => window.setTimeout(resolve, 50));
      }
      const loadedAt = new Date().toISOString();
      const key = `${playSession.current}:${deck}:${track.id}:${Date.now()}`;
      historyKeys.current.set(deck, { key, trackId: track.id, loadedAt, playedMs: 0 });
      saveRuntime();
      await persist(() => playService.upsertHistory({ event_key: key, session_id: playSession.current, deck, track_id: track.id, loaded_at: loadedAt }));
      setActiveDeck(deck);
    })));
  }, [client, connect, finalizeHistory, outputDevice, persist, run, saveRuntime, start, status?.running]);

  /** Bind the remote performer's presentation to a deck display only. The real
   * deck and the Junction Program output stay completely untouched. */
  const monitorJunction = useCallback((deck: DeckId) => {
    if (!junction.current) {
      setCommandError("Junction Liveの再生中トラックを受信してから再試行してください。");
      return;
    }
    setCommandError(null);
    updateJunctionMonitorDeck(deck);
    setActiveDeck(deck);
  }, [junction.current, updateJunctionMonitorDeck]);

  const editHotCue = (deck: DeckId, slot: number, clear: boolean) => run(async () => {
    const trackId = client.getState().snapshot?.decks[deck].track?.trackId;
    if (!trackId) return;
    const session = client.getSessionId();
    const loads = { ...deckLoadRequests.current };
    const stillLoaded = (id: DeckId) => loads[id] === deckLoadRequests.current[id] && client.getSessionId() === session && client.getState().snapshot?.decks[id].track?.trackId === trackId;
    const current = client.getState().snapshot!.decks[deck];
    // 呼び出しはメタデータに触れないので、保存待ちの列に並ばせない。
    // ここを直列化していると、直前の保存の HTTP 往復ぶんだけ反応が遅れる。
    if (!clear && current.hotCues[slot] != null) {
      await client.jumpToHotCue(deck, slot);
      if (current.status !== "playing") await client.play(deck);
      return;
    }
    // 登録・削除はエンジンを先に動かす。保存の往復を待ってから鳴らすと、
    // 押してから反応するまでに往復が二重に乗る。
    const result = await (clear ? client.clearHotCue(deck, slot) : client.setHotCue(deck, slot));
    if (client.getState().snapshot?.decks[deck].track?.assetId || localTrackId(client.getState().snapshot?.decks[deck].track) === null) return;
    await gridOperations.current.run(Number(trackId), async () => {
      if (!stillLoaded(deck)) return;
      const baseline = await performanceMetadataService.get(Number(trackId));
      if (!stillLoaded(deck)) return;
      const previous = baseline.cue_points.find(cue => cue.slot === slot)?.position_ms ?? null;
      let position: number | null | undefined;
      try {
        const positions = (result as { hotCues?: (number | null)[] })?.hotCues;
        position = clear ? null : positions?.[slot];
        if (position === undefined || !clear && (typeof position !== "number" || !Number.isFinite(position))) throw new Error("エンジンの登録位置を確認できませんでした");
        const saved = await persistCue(performanceMetadataService, Number(trackId), baseline, slot, position);
        setTrackMetadata(old => ({ ...old, [Number(trackId)]: saved }));
        setGridEdit(old => old?.trackId === Number(trackId) ? { ...old, metadata: saved } : old);
      } catch (cause) {
        // A conflicting editor may already have saved a newer cue; restore the
        // current durable value, never overwrite it with our stale baseline.
        const latest = await performanceMetadataService.get(Number(trackId)).catch(() => null);
        const restored = latest ? latest.cue_points.find(cue => cue.slot === slot)?.position_ms ?? null : previous;
        if (latest) setGridEdit(old => old?.trackId === Number(trackId) ? { ...old, metadata: latest } : old);
        if (stillLoaded(deck)) await (restored === null ? client.clearHotCue(deck, slot) : client.setHotCue(deck, slot, restored, false)).catch(() => { throw new Error("ホットキューの保存と復元に失敗しました。曲を再ロードしてください。"); });
        throw cause;
      }
      for (const other of DECK_IDS) {
        if (other !== deck && stillLoaded(other)) {
          await (position === null ? client.clearHotCue(other, slot) : client.setHotCue(other, slot, position, false))
            .catch(() => { throw new Error(`ホットキューは保存済みですが、DECK ${other}への反映に失敗しました。曲を再ロードしてください。`); });
        }
      }
      if (stillLoaded(deck)) await client.refreshSnapshot();
    });
  });

  const importRekordboxCues = async () => {
    const result = await performanceMetadataService.importAllRekordboxCues();
    setTrackMetadata({});
    setCueRevision(value => value + 1);
    const ids = [...new Set(DECK_IDS.map(id => (localTrackId(client.getState().snapshot?.decks[id].track) ?? -1)).filter(id => Number.isSafeInteger(id) && id > 0))];
    const failures: DeckId[] = [];
    for (const trackId of ids) await gridOperations.current.run(trackId, async () => {
      const session = client.getSessionId();
      if (!session) return;
      const loads = { ...deckLoadRequests.current };
      const stillLoaded = (id: DeckId) => client.getSessionId() === session && loads[id] === deckLoadRequests.current[id]
        && client.getState().snapshot?.decks[id].track?.trackId === String(trackId);
      const saved = await performanceMetadataService.get(trackId);
      setTrackMetadata(old => ({ ...old, [trackId]: saved }));
      setGridEdit(old => old?.trackId === trackId ? { ...old, metadata: saved } : old);
      for (const id of DECK_IDS) {
        if (!stillLoaded(id)) continue;
        const duration = client.getState().snapshot!.decks[id].track!.durationMs;
        if (saved.cue_points.some(cue => cue.position_ms >= duration)) { failures.push(id); continue; }
        const positions = cuePositions(saved, duration);
        try {
          for (let slot = 0; slot < 8; slot++) {
            if (!stillLoaded(id)) break;
            const position = positions[slot];
            await (position == null ? client.clearHotCue(id, slot) : client.setHotCue(id, slot, position, false));
          }
        } catch { failures.push(id); }
      }
    }).catch(() => {
      for (const id of DECK_IDS) if (client.getState().snapshot?.decks[id].track?.trackId === String(trackId)) failures.push(id);
    });
    if (client.getSessionId()) await client.refreshSnapshot().catch(() => {
      setCommandError("CUE一括保存は完了しましたが、音声エンジンの状態を更新できませんでした。再接続してください。");
    });
    if (failures.length) setCommandError(`CUE一括保存は完了しましたが、DECK ${failures.join(" / ")}への反映に失敗しました。曲を再ロードしてください。`);
    return result;
  };

  const saveCurrentLoop = (deck: DeckId) => run(async () => {
    const state = client.getState().snapshot?.decks[deck];
    if (!state?.track || !state.loopRegion) return;
    const trackId = localTrackId(state.track);
    if (trackId === null) return;
    const region = { ...state.loopRegion };
    await gridOperations.current.run(trackId, async () => {
      const baseline = await performanceMetadataService.get(trackId);
      const saved = await persistLoop(performanceMetadataService, trackId, baseline, {
        id: crypto.randomUUID(), start_ms: region.startMs, end_ms: region.endMs,
        label: `Loop ${baseline.loops.length + 1}`,
      });
      setTrackMetadata(old => ({ ...old, [trackId]: saved }));
      setGridEdit(old => old?.trackId === trackId ? { ...old, metadata: saved } : old);
    });
  });

  const recallLoop = (deck: DeckId, loopId: string) => run(async () => {
    const trackId = client.getState().snapshot?.decks[deck].track?.trackId;
    if (!trackId) return;
    const localId = localTrackId(client.getState().snapshot?.decks[deck].track);
    if (localId === null) return;
    const loop = trackMetadata[localId]?.loops.find(item => item.id === loopId);
    if (!loop) return;
    const load = deckLoadRequests.current[deck];
    const session = client.getSessionId(), lease = junctionLeaseKey();
    await client.setLoop(deck, loop.start_ms, loop.end_ms);
    if (lease !== junctionLeaseKey() || load !== deckLoadRequests.current[deck] || session !== client.getSessionId() || client.getState().snapshot?.decks[deck].track?.trackId !== trackId) return;
    await client.enableLoop(deck, true);
  });

  useEffect(() => {
    if (!snapshot) return;
    for (const deck of DECK_IDS) {
      const item = historyKeys.current.get(deck);
      const deckState = snapshot.decks[deck];
      if (!item) continue;
      if (!deckState?.track || deckState.track.trackId !== String(item.trackId)) {
        void finalizeHistory(deck, "ejected");
        continue;
      }
      if (deckState.status === "playing" && !item.startedAt) {
        item.startedAt = new Date().toISOString();
        item.playingSince = Date.now();
        saveRuntime();
        void persist(() => playService.upsertHistory({ event_key: item.key, session_id: playSession.current, deck, track_id: item.trackId, loaded_at: item.loadedAt, first_played_at: item.startedAt }));
      } else if (deckState.status === "playing" && !item.playingSince) {
        item.playingSince = Date.now();
        saveRuntime();
      } else if (deckState.status !== "playing" && item.playingSince) {
        item.playedMs += Math.max(0, Date.now() - item.playingSince);
        item.playingSince = undefined;
        saveRuntime();
      }
      if (item.startedAt && deckState.track.durationMs > 0 && deckState.positionMs >= deckState.track.durationMs - 250 && deckState.status !== "playing") {
        void finalizeHistory(deck, "completed");
      }
    }
  }, [finalizeHistory, persist, saveRuntime, snapshot]);

  useEffect(() => {
    const recording = snapshot?.recording;
    const previous = previousRecording.current;
    previousRecording.current = recording;
    if (!recording) return;
    if (recording.active && !previous?.active && recording.path && recording.startedAt) {
      recordingKey.current = `${playSession.current}:${recording.startedAt}`;
      saveRuntime();
      void persist(() => playService.upsertRecording({ recording_key: recordingKey.current!, session_id: playSession.current, filepath: recording.path!, started_at: recording.startedAt!, duration_ms: recording.elapsedMs, status: "recording" }));
    }
    if (!recording.active && previous?.active && recording.path && previous.startedAt) {
      const key = recordingKey.current ?? `${playSession.current}:${previous.startedAt}`;
      const failed = Boolean(recording.error);
      void persist(async () => {
        const saved = await playService.upsertRecording({ recording_key: key, session_id: playSession.current, filepath: recording.path!, started_at: previous.startedAt!, ended_at: new Date().toISOString(), duration_ms: recording.elapsedMs, status: failed ? "failed" : "completed", error: recording.error, sample_rate_hz: recording.sampleRateHz, frame_count: recording.frameCount, timeline_quality: recording.timelineQuality ?? "not_recorded", timeline_dropped_events: recording.timelineDroppedEvents ?? 0 });
        if (recording.sampleRateHz && recording.frameCount !== undefined && recording.timeline) await workflowsService.saveEngineTimeline(saved.id, { sample_rate_hz: recording.sampleRateHz, frame_count: recording.frameCount, dropped_events: recording.timelineDroppedEvents ?? 0, segments: recording.timeline });
      })
        .then(async () => {
          // 保存した行を引き当ててから、聴いて名前を付けてもらう。
          if (failed || exiting.current) return;
          const rows = await playService.recordings().catch(() => [] as RecordingEntry[]);
          const saved = rows.find((row) => row.recording_key === key);
          if (saved) setSavingRecording(saved);
        })
        .catch(() => undefined);
      recordingKey.current = null;
      saveRuntime();
    }
  }, [persist, saveRuntime, snapshot?.recording]);

  const changeDeckCount = useCallback((count: 2 | 4) => {
    if (count === deckCount) return;
    if (count === 2 && !junctionState.active()) {
      if (activeDeck === "C" || activeDeck === "D") setActiveDeck("A");
      for (const deck of ["C", "D"] as const) {
        void run(async () => {
          const operations: Promise<unknown>[] = [];
          if (snapshotRef.current?.decks[deck]?.status === "playing") operations.push(client.pause(deck));
          if (snapshotRef.current?.decks[deck]?.track) operations.push(client.unload(deck));
          await Promise.allSettled(operations);
          await finalizeHistory(deck, "deck_hidden");
        });
      }
    }
    setDeckCount(count);
    localStorage.setItem("plumdeck.deckCount", String(count));
  }, [activeDeck, client, deckCount, finalizeHistory, run]);

  const stopEngine = useCallback(() => {
    void run(async () => {
      if (junctionState.active()) throw new Error("先にJunctionセッションを終了してください。");
      try { await finalizeRecording(); } finally { await stop(); }
      await Promise.allSettled(DECK_IDS.map((deck) => finalizeHistory(deck, "engine_stop")));
    });
  }, [finalizeHistory, finalizeRecording, run, stop]);

  const togglePlay = useCallback((deck: DeckId) => {
    const current = snapshot?.decks[deck];
    if (!current?.track) return;
    void run(() => current.status === "playing" ? client.pause(deck) : client.play(deck));
  }, [client, run, snapshot]);

  const seekRelative = useCallback((deck: DeckId, delta: number) => {
    const current = snapshot?.decks[deck];
    if (!current?.track) return;
    const durationMs = current.track.durationMs;
    void run(() => client.seek(deck, Math.max(-60_000, Math.min(durationMs, current.positionMs + delta)), durationMs));
  }, [client, run, snapshot]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (event.defaultPrevented || document.querySelector('[role="dialog"]') || target?.closest("input,textarea,select,button,[role=slider],[contenteditable=true]")) return;
      const select = ({ "1": "A", "2": "B", "3": "C", "4": "D" } as Record<string, DeckId>)[event.key];
      if (select && visibleDecks.includes(select)) { setActiveDeck(select); return; }
      if (event.repeat && event.code === "Space") return;
      if (event.code === "Space") { event.preventDefault(); togglePlay(activeDeck); }
      if (event.key.toLowerCase() === "q") seekRelative(activeDeck, -5_000);
      if (event.key.toLowerCase() === "e") seekRelative(activeDeck, 5_000);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [activeDeck, seekRelative, togglePlay, visibleDecks]);

  const applyAudioOutput = async (device: string, microphone?: MicrophoneSettings, routing?: OutputRouting) => {
    if (junctionState.active()) throw new Error("Junction中の配信先はセッション設定から変更してください。");
    applyingAudio.current = true;
    try {
      if (device !== outputDevice || !client.getState().snapshot?.audio.applied || (routing && (JSON.stringify(routing.masterChannels) !== JSON.stringify(client.getState().snapshot?.audio.masterChannels) || JSON.stringify(routing.pflChannels) !== JSON.stringify(client.getState().snapshot?.audio.pflChannels)))) {
        if (snapshotRef.current?.recording?.active) await finalizeRecording();
        await client.stop();
        await Promise.allSettled(DECK_IDS.map(deck => finalizeHistory(deck, "audio_output_changed")));
        const started = await client.start(device || undefined, recordingDir || undefined);
        if (!started.running) throw new Error(started.detail || "音声エンジンを起動できませんでした");
        await client.connect();
        for (let attempt = 0; attempt < 40; attempt++) {
          const current = await client.refreshSnapshot();
          if (current.audio.applied) break;
          await new Promise(resolve => window.setTimeout(resolve, 100));
        }
        if (!client.getState().snapshot?.audio.applied) throw new Error(client.getState().snapshot?.audio.reason || "出力デバイスを開けませんでした");
        setOutputDevice(device);
        localStorage.setItem("plumdeck.djOutputDevice", device);
        if (recordingFormat.trim()) await client.setRecordingFormat(recordingFormat.trim());
      }
      if (routing) {
        const applied = await client.setOutputRouting(routing);
        localStorage.setItem(OUTPUT_ROUTING_KEY, JSON.stringify({ deviceId: applied.deviceId, routing }));
      }
      microphoneRestoredSession.current = client.getSessionId();
      if (microphone) {
        const applied = await client.setMicrophone(microphone);
        if (applied.microphone) saveMicrophoneSettings(applied.microphone);
      }
    } finally { applyingAudio.current = false; }
  };

  const prepareRecordingPreview = async () => {
    if (junctionState.active()) throw new Error("Junction中は録音プレビューを利用できません。手元の試聴をご利用ください。");
    usePlayerStore.getState().pause();
    if (!client.getSessionId()) return;
    const current = client.getState().snapshot;
    if (current?.recording?.active) throw new Error("新しい録音を停止してからプレビューしてください。");
    await Promise.all(DECK_IDS.filter(id => current?.decks[id].status === "playing").map(id => client.pause(id)));
  };

  const closeGridEditor = useCallback(() => {
    if (gridSaving.current) return;
    gridOpenRequest.current++;
    setGridEdit(null);
    setGridPreview(null);
  }, []);

  useEffect(() => {
    if (gridEdit && snapshot?.decks[gridEdit.deck]?.track?.trackId !== String(gridEdit.trackId)) {
      gridOpenRequest.current++;
      setGridEdit(null);
      setGridPreview(null);
    }
  }, [gridEdit, snapshot]);

  const openGridEditor = (deck: DeckId) => {
    if (gridSaving.current) return;
    if (gridEdit?.deck === deck) { closeGridEditor(); return; }
    const track = client.getState().snapshot?.decks[deck]?.track;
    if (!track || track.assetId || !/^\d+$/.test(track.trackId)) return;
    const request = ++gridOpenRequest.current;
    void run(async () => {
      const metadata = await performanceMetadataService.get(Number(track.trackId));
      if (request !== gridOpenRequest.current || client.getState().snapshot?.decks[deck]?.track?.trackId !== track.trackId) return;
      setGridPreview(null);
      setGridEdit({ deck, trackId: Number(track.trackId), metadata, initialGrid: metadata.beat_grid ?? {
        bpm: track.bpm && track.bpm >= 20 && track.bpm <= 300 ? track.bpm : 120,
        first_beat_ms: track.beatgridOffsetMs ?? 0,
        beats_per_bar: track.beatsPerBar ?? 4,
      } });
    });
  };

  const saveGrid = async (grid: PerformanceBeatGrid) => {
    const editing = gridEdit;
    if (!editing || gridSaving.current) throw new Error("編集対象が変わりました。グリッド編集を開き直してください。");
    gridSaving.current = true;
    try {
      await gridOperations.current.run(editing.trackId, async () => {
      const loaded = DECK_IDS.map(id => client.getState().snapshot?.decks[id]?.track).filter(track => track?.trackId === String(editing.trackId));
      const lastBeat = grid.beat_times_ms?.[grid.beat_times_ms.length - 1] ?? grid.first_beat_ms;
      if (loaded.some(track => track && lastBeat >= track.durationMs)) throw new Error("グリッドが実際の音声の終端を超えています。再解析または曲の対応関係を確認してください。保存はしていません。");
      const payload = { deck: editing.deck, trackId: String(editing.trackId), bpm: grid.bpm, firstBeatMs: grid.first_beat_ms, beatsPerBar: grid.beats_per_bar, beatTimesMs: grid.beat_times_ms ?? undefined, beatNumbers: grid.beat_numbers ?? undefined };
      if (new TextEncoder().encode(JSON.stringify(payload)).byteLength > 1024 * 1024) throw new Error("拍データが音声エンジンの転送上限を超えています。保存はしていません。");
      // PUT is complete replacement: keep cues, loops and the revision captured
      // when editing started. Never silently overwrite a concurrent edit.
      let saved: PerformanceMetadata;
      try {
        saved = await performanceMetadataService.replace(editing.trackId, {
          revision: editing.metadata.revision,
          cue_points: editing.metadata.cue_points,
          loops: editing.metadata.loops,
          beat_grid: grid,
        });
      } catch (cause) {
        if (cause instanceof ApiError && cause.status === 409) throw new Error("他の画面で変更されています。閉じて開き直し、最新のグリッドを確認してください。");
        throw cause;
      }
      setGridEdit((current) => current?.trackId === editing.trackId ? { ...current, metadata: saved } : current);
      const targets = DECK_IDS.filter((id) => client.getState().snapshot?.decks[id]?.track?.trackId === String(editing.trackId));
      const results = await Promise.allSettled(targets.map((id) => client.setBeatgrid(id, String(editing.trackId), grid.bpm, grid.first_beat_ms, grid.beats_per_bar, grid.beat_times_ms, grid.beat_numbers)));
      const failed = results.flatMap((result, index) => result.status === "rejected" ? [targets[index]] : []);
      if (failed.length) throw new Error(`plumdeckへの保存は完了しましたが、デッキ ${failed.join(" / ")} への反映に失敗しました。再保存で適用を再試行できます。`);
      await client.refreshSnapshot();
      });
    } catch (cause) {
      setCommandError(cause instanceof Error ? cause.message : String(cause));
      throw cause;
    } finally { gridSaving.current = false; }
  };

  const displayedDeck = (id: DeckId) => {
    const deck = snapshot?.decks[id];
    const remote = junctionMonitorDeck === id ? junction.current : null;
    if (deck && remote) {
      const positionMs = Math.max(0, Math.min(remote.duration * 1000, remote.positionMs));
      return {
        ...deck,
        status: remote.playing ? "playing" as const : "paused" as const,
        positionMs,
        positionFrames: Math.round(positionMs * 44.1),
        rate: remote.rate,
        keylock: false,
        syncEnabled: false,
        syncLeader: null,
        effectiveBpm: remote.bpm ? remote.bpm * remote.rate : null,
        scratching: false,
        hotCues: Array.from({ length: 16 }, () => null),
        loopRegion: null,
        lastError: remote.state === "failed" ? remote.detail ?? "Junction Liveの音源を受信できませんでした" : null,
        track: {
          assetId: remote.assetId,
          localTrackId: null,
          trackId: `junction-live:${remote.assetId}`,
          path: remote.filepath,
          title: remote.title,
          artist: remote.artist || null,
          durationMs: remote.duration * 1000,
          bpm: remote.bpm,
          sampleRateHz: 44_100,
          channels: 2,
        },
      };
    }
    if (!deck?.track || !gridPreview || gridEdit?.trackId !== Number(deck.track.trackId)) return deck;
    return { ...deck, track: { ...deck.track, bpm: gridPreview.bpm, beatgridOffsetMs: gridPreview.first_beat_ms, beatsPerBar: gridPreview.beats_per_bar, beatTimesMs: gridPreview.beat_times_ms ?? undefined, beatNumbers: gridPreview.beat_numbers ?? undefined } };
  };

  const seed = localTrackId(snapshot?.decks[activeDeck]?.track) ?? -1;
  const recordingSupported = Boolean(snapshot?.engine.capabilities.includes("recording"));
  const capabilities = snapshot?.engine.capabilities ?? [];
  const canMix = connected && (capabilities.includes("mixer.basic") || capabilities.includes("mixer.gain"));
  const seekAbsolute = useCallback((deck: DeckId, ms: number) => {
    const current = client.getState().snapshot?.decks[deck];
    if (current?.track) void run(() => client.seek(deck, ms, current.track!.durationMs));
  }, [client, run]);
  const scratch = useCallback((deck: DeckId, command: { phase: "begin" | "move" | "end"; positionMs: number; gestureId: string; capturedAt?: number; keepalive?: boolean }) => {
    if (command.phase === "begin") setCommandError(null);
    return client.scratch(deck, command.phase, command.positionMs, command.gestureId, command.capturedAt, command.keepalive);
  }, [client]);
  const waveform = (id: DeckId, layout: WaveformLayout) => {
    const deck = displayedDeck(id);
    const monitorAssetId = junctionMonitorDeck === id && junction.current?.state === "ready" ? junction.current.assetId : undefined;
    return <DeckWaveform key={id} label={id} assetId={deck?.track?.assetId} remoteWaveform={deck?.track?.waveform} trackId={localTrackId(deck?.track)}
      monitorAssetId={monitorAssetId}
      positionMs={deck?.positionMs ?? 0} durationMs={deck?.track?.durationMs ?? 0} layout={layout} side={id === "A" || id === "C" ? "left" : "right"}
      color={id === "A" || id === "C" ? "cyan" : "fuchsia"} mode={expandedPair&&!expandedPair.includes(id)?"overview":"scroll"} playing={deck?.status === "playing"} rate={deck?.rate ?? 1} bpm={deck?.track?.bpm} beatgridOffsetMs={deck?.track?.beatgridOffsetMs} beatsPerBar={deck?.track?.beatsPerBar}
      beatTimesMs={deck?.track?.beatTimesMs} beatNumbers={deck?.track?.beatNumbers}
      gridAvailable={Boolean(deck?.track?.beatgridOffsetMs !== undefined || deck?.track?.beatTimesMs?.length)}
      onGridShift={!monitorAssetId && gridEdit?.deck === id && !gridSaving.current ? (deltaMs) => setGridShift(old => ({ sequence: (old?.sequence ?? 0) + 1, deltaMs })) : undefined}
      hotCues={deck?.hotCues} scratching={deck?.scratching} loopRegion={deck?.loopRegion}
      onSeek={connected && !monitorAssetId && !tailLock(id) ? (ms) => seekAbsolute(id, ms) : undefined}
      onScratch={connected && !monitorAssetId && !tailLock(id) ? (command) => scratch(id, command) : undefined}
      onBackspin={connected && !monitorAssetId && !tailLock(id) ? (release) => client.backspin(id, release.gestureId, release.positionMs, release.velocity, jogWeight(id),
        (cause) => setCommandError(cause instanceof Error ? cause.message : String(cause))) : undefined}
      onScratchGrab={connected && !monitorAssetId && !tailLock(id) ? () => client.grabBackspin(id) : undefined}
      onScratchError={(cause) => setCommandError(cause instanceof Error ? cause.message : String(cause))} />;
  };
  // Lanes accept the same drag payload as the decks, so a row can be dropped on
  // whichever waveform the eye is already on.
  const lane = (id: DeckId, layout: WaveformLayout) => <DeckWaveformLane key={id} id={id} expandedPair={expandedPair}>
    {waveform(id, layout)}
    <button className="dj-wave-expand" aria-label={`デッキ ${id} のペアを拡大`} aria-pressed={!!expandedPair?.includes(id)} onClick={()=>setExpandedPair(current=>current?.includes(id)?null:(id==='A'||id==='B'?'AB':'CD'))}>拡大</button>
  </DeckWaveformLane>;
  // シンク先は「いま master になっているデッキ」。無ければ相方のデッキ。
  // 一覧のプレビュー波形に出すホットキュー。編集したトラックはこちらが最新。
  const listCueOverrides = useMemo(
    () => Object.fromEntries(Object.entries(trackMetadata).map(([id, metadata]) => {
      const positions: (number | null)[] = Array.from({ length: 16 }, () => null);
      for (const cue of metadata.cue_points) if (cue.slot >= 0 && cue.slot < 16) positions[cue.slot] = cue.position_ms;
      return [Number(id), positions];
    })),
    [trackMetadata],
  );

  const togglePanel = (panel: PanelId) => setPanels((current) => {
    const next = { ...current, [panel]: !current[panel] };
    localStorage.setItem("plumdeck.panels", JSON.stringify(next));
    return next;
  });

  // FX ユニットは 1 チャンネル 1 エフェクト。担当チャンネルを移したら前の
  // チャンネルを必ず切ってから掛け直す（掛かりっぱなしを残さない）。
  const changeFxUnit = (index: 0 | 1, unit: FxUnitState) => {
    const previous = fxUnits[index];
    setFxUnits((current) => { const next = [...current] as [FxUnitState, FxUnitState]; next[index] = unit; return next; });
    void run(async () => {
      if (previous.channel !== unit.channel && previous.enabled) await client.setFx(previous.channel, previous.effect, false, previous.mix);
      await client.setFx(unit.channel, unit.effect, unit.enabled, unit.mix);
    });
  };

  const activeLeaderFor = (id: DeckId): DeckId | undefined => {
    const decks = snapshot?.engine.decks ?? [];
    const leader = decks.find((deck) => deck !== id && snapshot?.decks[deck]?.syncLeader === deck && snapshot.decks[deck]?.syncEnabled);
    return leader ?? decks.find((deck) => deck !== id);
  };

  const cueColorsFor = (id:DeckId) => Array.from({length:16},(_,slot)=>trackMetadata[(localTrackId(snapshot?.decks[id]?.track) ?? -1)]?.cue_points.find(cue=>cue.slot===slot)?.color??null);
  const softwareDeck = (id: DeckId) => <SoftwareDeck key={id} id={id} deck={displayedDeck(id)} active={activeDeck === id} connected={connected}
    tailLock={tailLock(id)}
    monitorOnly={junctionMonitorDeck === id && Boolean(junction.current)} monitorAssetId={junctionMonitorDeck === id && junction.current?.state === "ready" ? junction.current.assetId : undefined}
    cueColors={junctionMonitorDeck === id ? undefined : cueColorsFor(id)}
    onGridEdit={() => openGridEditor(id)}
    onGridClose={closeGridEditor}
    gridEditor={junctionMonitorDeck !== id && gridEdit?.deck === id && snapshot?.decks[id]?.track?.trackId === String(gridEdit.trackId) ? <BeatGridEditor key={`${id}:${gridEdit.trackId}`} trackId={gridEdit.trackId} durationMs={snapshot.decks[id].track!.durationMs} positionMs={snapshot.decks[id].positionMs} initialGrid={gridEdit.initialGrid} hasGrid={Boolean(gridEdit.metadata.beat_grid)} shiftRequest={gridShift} playing={snapshot.decks[id].status === "playing"} onTogglePlay={() => togglePlay(id)} onAnalyze={(force) => performanceMetadataService.analyzeGrid(gridEdit.trackId, force)} onRekordbox={() => performanceMetadataService.rekordboxGrid(gridEdit.trackId)} disabled={!connected} onPreview={setGridPreview} onSave={saveGrid} onClose={closeGridEditor} /> : undefined}
    capability={Boolean(snapshot?.engine.decks.includes(id))} capabilities={capabilities}
    channel={snapshot?.mixer.channels[id]}
    onBeatJump={(beats) => void run(() => client.beatJump(id, beats))}
    onBeatLoop={(beats) => void run(() => client.beatLoop(id, beats))}
    onLoopEnable={(enabled) => void run(() => client.enableLoop(id, enabled))}
    onQuantize={(enabled) => void run(() => client.setQuantize(id, enabled))}
    onFx={(effect, enabled, mix, depth) => void run(() => client.setFx(id, effect, enabled, mix, depth))}
    onSaveLoop={() => void saveCurrentLoop(id)}
    savedLoops={trackMetadata[(localTrackId(snapshot?.decks[id]?.track) ?? -1)]?.loops}
    trackKey={junctionMonitorDeck === id ? junction.current?.key : trackKeys[(localTrackId(snapshot?.decks[id]?.track) ?? -1)]}
    onRecallLoop={(loopId) => void recallLoop(id, loopId)}
    onActivate={() => setActiveDeck(id)} onToggle={() => togglePlay(id)}
    onCue={() => void run(async () => {
      const deck = client.getState().snapshot?.decks[id];
      if (!deck?.track) return;
      if (deck.status === "playing") { await client.pause(id); await client.seek(id, cuePoints.current[id], deck.track.durationMs); return; }
      cuePoints.current[id] = deck.positionMs;
    })}
    onSeek={(delta) => seekRelative(id, delta)} onSeekAbsolute={(ms) => seekAbsolute(id, ms)}
    onTempo={(rate) => void run(() => client.setTempo(id, rate))}
    onKeylock={(enabled) => void run(() => client.setKeylock(id, enabled))} onSync={(enabled) => void run(() => client.setSync(id, enabled, activeLeaderFor(id)))}
    onMaster={() => void run(async () => {
      // エンジンにマスター指定の op は無いので、他デッキの sync をこのデッキ基準で張り直す。
      const decks = client.getState().snapshot?.engine.decks ?? [];
      for (const other of decks) {
        if (other === id) continue;
        if (client.getState().snapshot?.decks[other]?.syncEnabled) await client.setSync(other, true, id);
      }
      await client.setSync(id, false);
    })}
    onUnload={() => junctionMonitorDeck === id ? updateJunctionMonitorDeck(null) : void run(async () => { ++deckLoadRequests.current[id]; cuePoints.current[id] = 0; manualLoopIn.current[id] = null; await client.unload(id); await finalizeHistory(id, "ejected"); })}
    onHotCue={(index, clear) => void editHotCue(id, index, clear)}
    onLoopIn={() => void run(async () => {
      const deck = client.getState().snapshot?.decks[id];
      if (deck?.track) manualLoopIn.current[id] = deck.positionMs;
    })}
    onLoopOut={() => void run(async () => {
      const deck = client.getState().snapshot?.decks[id];
      const start = manualLoopIn.current[id];
      if (!deck?.track || start === null || deck.positionMs <= start) return;
      manualLoopIn.current[id] = null;
      await client.setLoop(id, start, deck.positionMs);
      await client.enableLoop(id, true);
    })}
    onLoop={(beats) => void run(() => client.getState().snapshot?.decks[id]?.loopRegion?.enabled ? client.enableLoop(id, false) : client.beatLoop(id, beats))} />;

  const midi = useDdj1000({
    activate: (deck) => { setActiveDeck(deck); if (deck === "C" || deck === "D") changeDeckCount(4); },
    library: (action) => { window.dispatchEvent(new CustomEvent("plumdeck:controller-library", { detail: action })); },
    hotcue: editHotCue,
    memory: memoryAction,
    cuePoints: cuePoints.current,
    cueColors: {A:cueColorsFor("A"),B:cueColorsFor("B"),C:cueColorsFor("C"),D:cueColorsFor("D")},
    error: setCommandError,
  });

  const loadLocalTrack = (deck: DeckId, track: Track) => {
    if (junctionMonitorDeck === deck) updateJunctionMonitorDeck(null);
    loadTrack(deck, track);
  };

  return <PlayDragDropProvider onLoad={loadLocalTrack} onMonitorJunction={monitorJunction}><main style={{'--wave-contrast':waveContrast,'--wave-grayscale':waveMonochrome?1:0} as CSSProperties} className={cn("dj-workspace", expandedPair&&"dj-workspace--wave-expanded", compactDecks && "dj-workspace--compact", deckCount === 4 && "dj-workspace--four", waveformLayout === "vertical" && "dj-workspace--vertical")}>
    <header className="dj-global-bar">
      <div className="dj-performance-label"><Disc3 /><strong>PERFORMANCE</strong></div>
      <PanelToolbar visible={panels} onToggle={togglePanel} />
      <div className="dj-segmented" aria-label="デッキ表示サイズ">{([false, true] as const).map((compact) => <button key={String(compact)} title={compact ? "コンパクト表示（ブラウザを広く）" : "通常表示"} aria-pressed={compactDecks === compact} className={compactDecks === compact ? "is-on" : ""} onClick={() => { setCompactDecks(compact); localStorage.setItem("plumdeck.compactDecks", String(compact)); }}>{compact ? "コンパクト" : "通常"}</button>)}</div>
      <div className="dj-segmented" aria-label="Deck count">{([2, 4] as const).map((count) => <button key={count} title={`${count} デッキ`} aria-pressed={deckCount === count} className={deckCount === count ? "is-on" : ""} onClick={() => changeDeckCount(count)}>{count}</button>)}</div>
      <details className="dj-wave-settings"><summary>表示設定</summary><div>
        <label>波形コントラスト <input type="range" min="1" max="2" step="0.1" value={waveContrast} onChange={event=>{setWaveContrast(Number(event.target.value));localStorage.setItem('plumdeck.waveContrast',event.target.value);}} /></label>
        <label><input type="checkbox" checked={waveMonochrome} onChange={event=>{setWaveMonochrome(event.target.checked);localStorage.setItem('plumdeck.waveMonochrome',String(event.target.checked));}} />波形を単色で表示</label>
        <label title="音より波形が先行する場合は増やします。">波形の表示遅延（ms） <input type="number" min="-50" max="500" step="1" value={waveDelay} onChange={event=>{const value=Math.max(-50,Math.min(500,Number(event.target.value)||0));setWaveDelay(value);localStorage.setItem('plumdeck.waveDelay',String(value));}} /></label>
      </div></details>
      <div className="dj-segmented" aria-label="波形レイアウト">{(["horizontal", "vertical"] as const).map((layout) => <button key={layout} title={layout === "horizontal" ? "横波形（デッキ上部に重ねて表示）" : "縦波形（デッキ中央に並べて表示）"} aria-label={layout === "horizontal" ? "横波形" : "縦波形（デッキ中央）"} aria-pressed={waveformLayout === layout} className={waveformLayout === layout ? "is-on" : ""} onClick={() => { setWaveformLayout(layout); localStorage.setItem("plumdeck.waveformLayout", layout); }}>{layout === "horizontal" ? <Rows3 /> : <Columns3 />}</button>)}</div>
      <div className="dj-engine-status"><i className={connected && snapshot?.audio.applied ? "is-connected" : ""} /><span>{connected ? snapshot?.engine.simulated ? "SIMULATOR · 音声出力なし" : snapshot?.audio.applied ? "AUDIO CONNECTED" : "音声出力を確認中" : busy ? "音声エンジンを起動中…" : "AUDIO OFFLINE"}</span></div>
      <div className="dj-global-actions">
        <details className="dj-midi-settings">
          <summary className="dj-button" title={midi.status.error ?? "DDJ-1000 コントローラー設定"}>
            <span style={{ color: midi.status.connected && midi.enabled ? "#e6bd5e" : "#9ba6b3" }}>●</span>
            {midi.enabled ? midi.status.connected ? midi.status.received > 0 ? "DDJ-1000 MIDI受信あり" : "DDJ-1000 MIDI入力待ち" : "DDJ-1000 待機中" : "MIDI OFF"}
          </summary>
          <div className="dj-midi-popover">
            <label><input type="checkbox" checked={midi.enabled} onChange={e => midi.setEnabled(e.target.checked)} /> DDJ-1000を自動接続</label>
            <label title="JUNCTION MASTERを受けている間だけ、このチャンネルのLEVEL・EQ・CUE・クロスフェーダー割り当てがJUNCTION MASTERを操作します。デッキ側の操作は変わりません。">JUNCTION MASTERのチャンネル <select value={midi.junctionChannel} onChange={e => midi.setJunctionChannel(e.target.value === "C" || e.target.value === "D" ? e.target.value : "")}><option value="">割り当てない</option><option value="C">CH3</option><option value="D">CH4</option></select></label>
            <label title="小さい値ほど同じジョグ回転での移動量が小さくなります。">ジョグ移動量 <input type="range" min="0.005" max="0.25" step="0.005" value={midi.sensitivity} onChange={e => midi.setSensitivity(Number(e.target.value))} /> {midi.sensitivity.toFixed(3)} ms/count <button onClick={()=>midi.setSensitivity(0.1)}>標準に戻す</button></label>
            <span>受信 {midi.status.received} / 送信 {midi.status.sent}</span>
            <span>ジョグ画面: {midi.status.display?.authenticated ? `接続応答あり · HID送信 ${midi.status.display.reportsSent}` : "接続応答待ち"}</span>
            {midi.status.display?.error && <span role="alert">{midi.status.display.error}</span>}
            <span>MIDIポートの接続状態です。送信数は本体での点灯・表示を保証しません。</span>
            <button className="dj-button" disabled={!midi.enabled || !midi.status.connected} onClick={midi.testLights}>本体のCUEランプを4秒間点滅</button>
            <button className="dj-button" disabled={!midi.enabled} onClick={midi.reconnect}>MIDIを再接続</button>
            <span>反応しない場合：本体のUSB A / B切替をケーブルの接続先に合わせ、rekordboxを終了してください。</span>
            <span>ジョグ画面が NO AUDIO DRIVER の場合は、メーカーのドライバー認識状態と、下の音声設定を確認してください。</span>
            <details><summary>汎用MIDIモードの設定手順</summary><p>USBを抜き、本体を電源OFF。左デッキのSHIFT＋PLAY/PAUSEを押しながら電源ON。左のSLIP REVERSEを点灯させ、電源OFFで保存。再度電源を入れてUSBを接続します。rekordboxへ戻す際は同じ手順でSLIP REVERSEを消灯してください。</p></details>
            {midi.status.error && <span role="alert">{midi.status.error}</span>}
            <span>PLAY / CUE・ジョグ・テンポ・EQ・フェーダー・HOT CUE・LOOP・PAD FXに対応</span>
            <span>サンプラー・キーシフト・SLIP / REVERSEは未対応</span>
            <button className="dj-button" onClick={() => setAudioSettingsOpen(true)}>DDJ-1000の音声出力を設定</button>
          </div>
        </details>
        <button className="dj-button" title="オーディオ設定" onClick={() => setAudioSettingsOpen(true)}><Settings2 />オーディオ</button>
        {!status?.running && <><input aria-label="音声出力デバイス" className="dj-device-input" value={outputDevice} onChange={(event) => { setOutputDevice(event.target.value); localStorage.setItem("plumdeck.djOutputDevice", event.target.value); }} placeholder="標準オーディオ出力" /><button className="dj-button" disabled={busy || !status?.installed} onClick={() => void start(outputDevice || undefined, recordingDir || undefined)}>{busy ? <Loader2 className="animate-spin" /> : <Power />}起動</button></>}
        {status?.running && !connected && <button className="dj-button" onClick={() => void connect()}>接続</button>}
        <button className={cn("dj-button", shortcuts && "is-on")} aria-label="キーボード操作" title="キーボード操作" onClick={() => setShortcuts((value) => !value)}><Keyboard /></button>
        <button className={cn("dj-button dj-record", snapshot?.recording?.active && "is-recording")} disabled={!connected || !recordingSupported || snapshot?.recording?.stopping} title={recordingSupported ? "録音開始 / 停止" : "録音は現在の音声ホストで利用できません"} onClick={() => void run(() => snapshot?.recording?.active ? finalizeRecording() : client.startRecording())}>{snapshot?.recording?.active ? <Square /> : <Circle />}<span>{snapshot?.recording?.active ? formatTime(snapshot.recording.elapsedMs) : "REC"}</span></button>
        {status?.running && <button className="dj-button" aria-label="音声エンジン停止" title="音声エンジン停止" onClick={stopEngine}><Power /></button>}
      </div>
    </header>
    {(error || commandError || snapshot?.recording?.error) && <div role="alert" className="dj-error"><AlertTriangle />{commandError || error || snapshot?.recording?.error}</div>}
    {shortcuts && <div className="dj-shortcuts"><b>1–4</b> デッキ選択　<b>Space</b> 再生 / 一時停止　<b>Q / E</b> 5秒戻る / 進む　<b>ドラッグ</b> 曲を波形またはデッキへ　<b>波形ドラッグ</b> スクラブ</div>}
    {panels.fx && <FxPanel decks={visibleDecks} units={fxUnits} onChange={changeFxUnit}
      disabledReason={!connected ? "未接続" : !capabilities.includes("mixer.fx") ? "この音声エンジンはエフェクト非対応" : undefined} />}
    {panels.fx && <BeatFxPanel state={snapshot?.mixer.beatFx} enabled={Boolean(snapshot?.engine.capabilities.includes("mixer.beatfx"))} />}
    {panels.recording && <div className="dj-panel-stub" role="note">
      録音は上部ツールバーの REC ボタンから開始/停止できます。専用パネルは未実装です。
    </div>}
    <section className="dj-performance" aria-label="Software DJ controller">
      {waveformLayout === "horizontal" && <div className="dj-scrolling-waves">{visibleDecks.map((id) => lane(id, "horizontal"))}</div>}
      {junctionSession?.active && <JunctionTurnBar snapshot={junctionSession} />}
      <div className="dj-deck-pairs">{junctionInput?.peerId && <div className="dj-deck-pair dj-deck-pair--junction">
        <JunctionInputDeck state={junctionInput}
          performerName={junctionSession?.participants.find((participant) => participant.peerId === junctionInput.peerId)?.djName}
          mixingTail={junctionSession?.turn?.signal === "onair"}
          onSet={(settings) => junctionCommand('input.set', { ...settings })}
          onRelease={() => junctionCommand('turn.release')}
          onMatchTempo={snapshot?.decks[activeDeck]?.track?.bpm && !tailLock(activeDeck) ? async (bpm) => {
            const trackBpm = client.getState().snapshot?.decks[activeDeck]?.track?.bpm;
            if (!trackBpm) throw new Error(`DECK ${activeDeck}のBPMが分かりません`);
            await client.setTempo(activeDeck, bpm / trackBpm);
          } : undefined} />
      </div>}{Array.from({ length: deckCount / 2 }, (_, pair) => {
        const left = DECK_IDS[pair * 2]; const right = DECK_IDS[pair * 2 + 1];
        return <div className="dj-deck-pair" key={left}>
          {softwareDeck(left)}
          {waveformLayout === "vertical" && <div className="dj-center-waves" aria-label="中央縦波形">{lane(left, "vertical")}{lane(right, "vertical")}</div>}
          {panels.mixer && <MixerPanel decks={[left, right]}
            deckState={(deck) => snapshot?.decks[deck]}
            channelState={(deck) => snapshot?.mixer.channels[deck]}
            handlers={(deck) => ({
              onTrim: (gain) => void run(() => client.setTrim(deck, gain)),
              onEq: (band, gain) => void run(() => client.setEq(deck, band, gain)),
              onFilter: (value) => void run(() => client.setFilter(deck, value)),
              onGain: (gain) => void run(() => client.setChannelGain(deck, gain)),
              onPfl: (enabled) => void run(() => client.setPfl(deck, enabled)),
              onTempo: (rate) => void run(() => client.setTempo(deck, rate)),
            })}
            reason={(deck, capability, feature, controller) =>
              capability !== "mixer.pfl" && tailLock(deck) ? tailLock(deck)
              : !connected ? "未接続"
                : !snapshot?.engine.decks.includes(deck) ? `Deck ${deck} 未実装`
                  : !(capabilities.includes(capability) || capabilities.includes("mixer.basic")) ? `${feature} 未実装`
                    : !controller ? `${feature} コントローラー未接続` : undefined} />}
          {softwareDeck(right)}
        </div>;
      })}</div>
    </section>
    <div className="dj-master-strip">
      <span className="dj-master-title"><Volume2 />MASTER</span>
      <Fader label="マスターゲイン" min={0} max={1} value={snapshot?.mixer.masterGain ?? 0} disabled={!canMix || Boolean(outgoing)} onChange={(value) => void run(() => client.setMasterGain(value))} />
      <span className="dj-master-value">{Math.round((snapshot?.mixer.masterGain ?? 0) * 100)}%</span>
      {snapshot?.audio.microphone?.available && <button type="button" className={cn("dj-button", snapshot.audio.microphone.enabled && "is-active")}
        aria-label="マイク出力を切り替え" aria-pressed={snapshot.audio.microphone.enabled}
        disabled={!connected || !snapshot.audio.microphone.deviceId}
        title={snapshot.audio.microphone.deviceId ? "マイクをMasterへ出力 / ミュート" : "オーディオ設定でマイクを選択してください"}
        onClick={() => void run(async () => { const applied = await client.setMicrophone({ enabled: !client.getState().snapshot?.audio.microphone?.enabled }); if (applied.microphone) saveMicrophoneSettings(applied.microphone); })}>
        <Mic />MIC {snapshot.audio.microphone.enabled ? "ON" : "OFF"}</button>}
      <div className="dj-crossfader"><span>A{deckCount === 4 ? "/C" : ""}</span><Fader label="クロスフェーダー" min={-1} max={1} value={snapshot?.mixer.crossfader ?? 0} disabled={Boolean(outgoing) || !connected || !capabilities.includes("mixer.basic") && !capabilities.includes("mixer.crossfader")} onChange={(value) => void run(() => client.setCrossfader(value))} /><span>B{deckCount === 4 ? "/D" : ""}</span></div>
      <span className={cn("dj-master-note", snapshot?.recording?.active && "is-recording")}>{snapshot?.recording?.active ? `● REC ${formatTime(snapshot.recording.elapsedMs)}` : `DECK ${activeDeck} SELECTED`}</span>
    </div>
    <PlayLibrary activeDeck={activeDeck} seedTrackId={Number.isFinite(seed) && seed > 0 ? seed : null} onLoad={loadLocalTrack} cueOverrides={listCueOverrides} cueRevision={cueRevision} cueImportControl={<RekordboxCueImportButton onImport={importRekordboxCues} />} />
    {audioSettingsOpen && <AudioSettings onClose={() => setAudioSettingsOpen(false)} onApply={applyAudioOutput} />}
    <RecordingSaveDialog recording={savingRecording} onClose={() => setSavingRecording(null)}
      onBeforePreview={prepareRecordingPreview}
      onSettled={() => setSavingRecording(null)} />
  </main></PlayDragDropProvider>;
}
