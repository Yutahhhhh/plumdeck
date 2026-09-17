import { deckRealtimeStore } from "./deck-realtime-store";
import { routePerformanceCommand } from '../performance-command-router';
import { junctionLeaseKey, junctionState } from '../junction/state';
/**
 * ネイティブ DJ エンジン（Phase 0 シミュレータ）のクライアントアダプタ。
 *
 * 既存の `<audio>` プレビュー再生（MusicPlayer / playerStore）とは別系統。
 * こちらを触っても既存の再生は影響を受けない。
 *
 * エンジンが未インストール・未起動でも例外にせず、`status()` が
 * `installed:false` / `running:false` を返して素直に劣化する。
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { LatestCommandQueue } from "./latest-command-queue";
import { ScratchCommandQueue } from "./scratch-command-queue";
import { Backspin } from "./backspin";
import { microphoneCommandParams } from "./audio-settings";

import {
  DJ_ENGINE_OPS,
  type DeckId,
  type AudioConfig,
  type MicrophoneSettings,
  type RecordingState,
  type EngineClientState,
  type EngineConnection,
  type EngineError,
  type EngineErrorCode,
  type EngineEventMessage,
  type EngineReply,
  type EngineSnapshot,
  type EngineStatus,
  type EqBand,
  type PadEffect,
  type ScratchCommand,
  type ScratchPhase,
  type TrackDescriptor,
} from "@/types/dj-engine";
import {
  applySnapshotWithEvents,
  buildBeatgridParams,
  buildBeatStepParams,
  buildFilterParams,
  buildTrimParams,
  buildFxParams,
  buildChannelGainParams,
  buildCrossfaderParams,
  buildEqParams,
  buildHotcueParams,
  buildLoadParams,
  buildLoopParams,
  buildMasterGainParams,
  buildMetersParams,
  buildSeekParams,
  buildScratchParams,
  buildTempoParams,
  createInitialClientState,
  describeEngineError,
  isEngineEventMessage,
  isEngineSnapshot,
  reduceEngineEvent,
} from "./protocol";

const EVENT_CHANNEL = "dj-engine://event";
const STATUS_CHANNEL = "dj-engine://status";

/** エンジンが返した構造化エラー。 */
export class DjEngineCommandError extends Error {
  readonly code: EngineErrorCode;
  readonly retryable: boolean;
  readonly details: unknown;

  constructor(error: EngineError) {
    super(describeEngineError(error));
    this.name = "DjEngineCommandError";
    this.code = error.code;
    this.retryable = error.retryable;
    this.details = error.details;
  }
}

const UNAVAILABLE_STATUS: EngineStatus = {
  installed: false,
  running: false,
  binaryPath: null,
  engineId: null,
  sessionId: null,
  protocol: null,
  simulated: true,
  implementation: null,
  lastError: null,
  detail:
    "Tauri 環境ではないため、ネイティブエンジンには接続できません（ブラウザ単体では利用不可）",
};

/** ブラウザ単体で開いた場合に Tauri IPC を呼ばないための判定。 */
function isTauriAvailable(): boolean {
  if (typeof window === "undefined") return false;
  return "__TAURI_INTERNALS__" in window || "__TAURI__" in window;
}

type StateListener = (state: EngineClientState) => void;
type StatusListener = (status: EngineStatus) => void;
type ContinuousControl = { op: string; params: Record<string, unknown>; session: string | null; deck?: DeckId; generation?: number; leaseKey: string };

export class DjEngineClient {
  private clientState: EngineClientState = createInitialClientState();
  private sessionId: string | null = null;
  private clockProbing = false;
  private clockTimer: ReturnType<typeof setInterval> | null = null;
  private seekQueues = new Map<DeckId, LatestCommandQueue<{ params: Record<string, unknown>; session: string | null; generation: number; trackId: string | undefined; leaseKey: string }>>();
  private seekGeneration: Record<DeckId, number> = { A: 0, B: 0, C: 0, D: 0 };
  private controlQueues = new Map<string, LatestCommandQueue<ContinuousControl>>();
  private scratchQueues = new Map<DeckId, ScratchCommandQueue>();
  private backspins = new Map<DeckId, { spin: Backspin; gestureId: string }>();
  private scratchGestures = new Map<string, { deck: DeckId; session: string | null; generation: number; trackId: string | undefined; positionMs: number; leaseKey: string }>();
  private stateListeners = new Set<StateListener>();
  private statusListeners = new Set<StatusListener>();
  private unsubscribers: UnlistenFn[] = [];
  private attaching: Promise<void> | null = null;
  /** Events arriving while a snapshot command is in flight are replayed afterward. */
  private snapshotEventBuffer: EngineEventMessage[] | null = null;
  private snapshotOperations: Promise<unknown> = Promise.resolve();
  private refreshing: Promise<EngineSnapshot> | null = null;

  private serializeSnapshot<T>(task: () => Promise<T>): Promise<T> {
    const result = this.snapshotOperations.then(task, task);
    this.snapshotOperations = result.catch(() => undefined);
    return result;
  }

  getState(): EngineClientState {
    return this.clientState;
  }

  getDeckGeneration(deck: DeckId): number { return this.seekGeneration[deck]; }

  getSessionId(): string | null {
    return this.sessionId;
  }

  subscribe(listener: StateListener): () => void {
    this.stateListeners.add(listener);
    return () => {
      this.stateListeners.delete(listener);
    };
  }

  subscribeStatus(listener: StatusListener): () => void {
    this.statusListeners.add(listener);
    return () => {
      this.statusListeners.delete(listener);
    };
  }

  // ---------------------------------------------------------------- ライフサイクル

  async status(): Promise<EngineStatus> {
    if (!isTauriAvailable()) return UNAVAILABLE_STATUS;
    try {
      return await invoke<EngineStatus>("dj_engine_status");
    } catch (error) {
      return { ...UNAVAILABLE_STATUS, detail: toMessage(error) };
    }
  }

  /** 明示的なオプトイン起動。既定では誰も自動起動しない。 */
  async start(outputDevice?: string, recordingDir?: string): Promise<EngineStatus> {
    if (!isTauriAvailable()) return UNAVAILABLE_STATUS;
    try {
      const status = await invoke<EngineStatus>("dj_engine_start", {
        outputDevice: outputDevice?.trim() || null,
        recordingDir: recordingDir?.trim() || null,
      });
      this.emitStatus(status);
      return status;
    } catch (error) {
      // start() can fail because the optional binary is absent. Re-read native
      // status instead of incorrectly claiming that it is installed.
      const current = await this.status();
      const status: EngineStatus = {
        ...current,
        lastError: current.lastError ?? toMessage(error),
        detail: current.detail ?? toMessage(error),
      };
      this.emitStatus(status);
      return status;
    }
  }

  async stop(): Promise<EngineStatus> {
    if (!isTauriAvailable()) return UNAVAILABLE_STATUS;
    const status = await invoke<EngineStatus>("dj_engine_stop");
    this.sessionId = null;
    this.clearScratchState();
    this.clientState = createInitialClientState();
    this.emitState();
    this.emitStatus(status);
    return status;
  }

  /**
   * セッションを張り直してスナップショットを取得する。
   * webview の再読み込み後はこれを呼ぶだけで状態が復元でき、再生は止まらない。
   */
  connect(): Promise<EngineConnection> {
    return this.serializeSnapshot(() => this.connectSnapshot());
  }

  private async connectSnapshot(): Promise<EngineConnection> {
    if (!isTauriAvailable()) {
      throw new Error(UNAVAILABLE_STATUS.detail ?? "エンジンを利用できません");
    }
    await this.attach();
    this.beginSnapshotBuffer();
    try {
      const connection = await invoke<EngineConnection>("dj_engine_connect");
      if (!isEngineSnapshot(connection.snapshot)) {
        throw new Error("エンジンから不正な状態スナップショットを受信しました");
      }
      this.sessionId = connection.sessionId;
      deckRealtimeStore.reset(connection.snapshot.engineId);
      if (this.clockTimer) clearInterval(this.clockTimer);
      const probe = async () => {
        const session = this.sessionId;
        const t0 = performance.now();
        try {
          const data = await this.send("engine.clock.probe") as {engineEpoch:string; receivedNativeUs:number; sentNativeUs:number};
          if (session === this.sessionId && data.engineEpoch === connection.snapshot.engineId)
            deckRealtimeStore.mapping.probe(t0, data.receivedNativeUs, data.sentNativeUs, performance.now());
        } catch { /* Legacy hosts keep the existing position path. */ }
      };
      if (connection.snapshot.engine.capabilities.includes("deck.clock.v2")) {
        void (async () => { for (let i=0;i<5;i++) await probe(); })();
        this.clockTimer = setInterval(() => void probe(), 30_000);
      }
      // Resolve native lifetime before any React subscriber can restore saved controls.
      const junction = await invoke<EngineReply>("junction_command", {sessionId: this.sessionId, op: "snapshot", params: {}});
      if (junction.ok && junction.data && typeof (junction.data as {active?: unknown}).active === "boolean") junctionState.set(junction.data as import("../../types/junction").JunctionSnapshot);
      this.clearScratchState();
      this.clientState = this.applyBufferedSnapshot(
        createInitialClientState(),
        connection.snapshot
      );
      this.emitState();
      return connection;
    } finally {
      this.snapshotEventBuffer = null;
    }
  }

  /** イベント購読を解除する。エンジンは止めない。 */
  async detach(): Promise<void> {
    if (this.clockTimer) clearInterval(this.clockTimer);
    this.clockTimer = null; deckRealtimeStore.reset();
    const unsubscribers = this.unsubscribers;
    this.unsubscribers = [];
    this.attaching = null;
    for (const unsubscribe of unsubscribers) {
      try {
        unsubscribe();
      } catch {
        // 解除失敗は無視してよい
      }
    }
  }

  /** スナップショットを取り直す。イベント欠落を検知したときに使う。 */
  refreshSnapshot(): Promise<EngineSnapshot> {
    if (this.refreshing) return this.refreshing;
    const result = this.serializeSnapshot(() => this.fetchSnapshot());
    this.refreshing = result;
    void result.finally(() => { if (this.refreshing === result) this.refreshing = null; }).catch(() => undefined);
    return result;
  }

  private async fetchSnapshot(): Promise<EngineSnapshot> {
    const session = this.sessionId;
    this.beginSnapshotBuffer();
    try {
      const snapshot = await this.send(DJ_ENGINE_OPS.snapshot, {});
      if (session !== this.sessionId) throw new Error("音声エンジンの接続が変更されました");
      if (!isEngineSnapshot(snapshot)) {
        throw new Error("エンジンから不正な状態スナップショットを受信しました");
      }
      this.clientState = this.applyBufferedSnapshot(this.clientState, snapshot);
      this.emitState();
      return snapshot;
    } finally {
      this.snapshotEventBuffer = null;
    }
  }

  // ---------------------------------------------------------------- 送信

  async send(op: string, params: Record<string, unknown> = {}): Promise<unknown> {
    if (!isTauriAvailable()) {
      throw new Error(UNAVAILABLE_STATUS.detail ?? "エンジンを利用できません");
    }
    const sessionId = this.sessionId;
    if (!sessionId) {
      throw new Error("エンジンに接続していません。先に connect() を呼んでください");
    }
    const reply = await invoke<EngineReply>("dj_engine_send", {
      sessionId,
      op,
      params: routePerformanceCommand(op, params),
    });
    if (reply.ok) {
      return reply.data;
    }
    const error: EngineError = reply.error ?? {
      code: "internal",
      message: "エンジンから理由不明のエラーが返りました",
      retryable: false,
    };
    if (sessionId === this.sessionId && (error.code === "session_mismatch" || error.code === "session_required")) {
      this.clientState = { ...this.clientState, sessionInvalidated: true };
      this.emitState();
    }
    throw new DjEngineCommandError(error);
  }

  // ---------------------------------------------------------------- 便利メソッド

  snapshot(): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.snapshot, {});
  }

  ping(): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.ping, {});
  }

  async load(deck: DeckId, track: TrackDescriptor): Promise<unknown> {
    const leaseKey = junctionLeaseKey();
    const params = routePerformanceCommand(DJ_ENGINE_OPS.deckLoad, buildLoadParams(deck, track));
    await this.releaseScratch(deck);
    if (leaseKey !== junctionLeaseKey()) throw new Error("演奏担当が変わりました。操作を再試行してください。");
    ++this.seekGeneration[deck]; this.seekQueues.get(deck)?.clear();
    this.scratchQueues.get(deck)?.clear();
    return this.send(DJ_ENGINE_OPS.deckLoad, params);
  }

  async unload(deck: DeckId): Promise<unknown> {
    const leaseKey = junctionLeaseKey();
    const params = routePerformanceCommand(DJ_ENGINE_OPS.deckUnload, {deck});
    await this.releaseScratch(deck);
    if (leaseKey !== junctionLeaseKey()) throw new Error("演奏担当が変わりました。操作を再試行してください。");
    ++this.seekGeneration[deck]; this.seekQueues.get(deck)?.clear();
    this.scratchQueues.get(deck)?.clear();
    return this.send(DJ_ENGINE_OPS.deckUnload, params);
  }

  play(deck: DeckId): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.deckPlay, { deck });
  }

  pause(deck: DeckId): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.deckPause, { deck });
  }

  pitchbend(deck: DeckId, amount: number): Promise<unknown> {
    return this.continuous(`${deck}:pitchbend`, "deck.pitchbend", this.withTrack(deck, { deck, amount }), deck);
  }

  seek(deck: DeckId, positionMs: number, durationMs?: number): Promise<unknown> {
    const settling = this.settleBackspin(deck);
    if (settling) return settling.then(() => this.seek(deck, positionMs, durationMs));
    const params = routePerformanceCommand(DJ_ENGINE_OPS.deckSeek, buildSeekParams(deck, positionMs, durationMs));
    let queue = this.seekQueues.get(deck);
    if (!queue) {
      queue = new LatestCommandQueue(async (target) => {
        if (target.leaseKey !== junctionLeaseKey() || target.session !== this.sessionId || target.generation !== this.seekGeneration[deck]
          || target.trackId !== this.clientState.snapshot?.decks[deck]?.track?.trackId) return;
        return this.send(DJ_ENGINE_OPS.deckSeek, target.params);
      });
      this.seekQueues.set(deck, queue);
    }
    return queue.enqueue({ params, leaseKey: junctionLeaseKey(), session: this.sessionId, generation: this.seekGeneration[deck], trackId: this.clientState.snapshot?.decks[deck]?.track?.trackId });
  }

  scratch(deck: DeckId, phase: ScratchPhase, positionMs: number, gestureId: string, capturedAt = performance.now(), keepalive = false): Promise<unknown> {
    // 別の手が掴んだら、惰性中のバックスピンを先に着地させる（同じキューで begin より前に end が届く）。
    if (phase === "begin") void this.settleBackspin(deck);
    const command = buildScratchParams(deck, phase, positionMs, gestureId);
    const mapped = deckRealtimeStore.mapping.at(capturedAt);
    if (mapped && this.clientState.snapshot?.engine.capabilities.includes("deck.clock.v2")) {
      command.capturedNativeUs = mapped.nativeUs;
      command.keepalive = keepalive;
    }
    let context = this.scratchGestures.get(gestureId);
    if (phase === "begin") {
      context = {
        deck,
        session: this.sessionId,
        generation: this.seekGeneration[deck],
        trackId: this.clientState.snapshot?.decks[deck]?.track?.trackId,
        positionMs, leaseKey: junctionLeaseKey(),
      };
      this.scratchGestures.set(gestureId, context);
    } else if (!context || context.deck !== deck) {
      return Promise.resolve();
    } else {
      context.positionMs = positionMs;
    }

    let queue = this.scratchQueues.get(deck);
    if (!queue) {
      queue = new ScratchCommandQueue(async (queued: ScratchCommand) => {
        const guard = this.scratchGestures.get(queued.gestureId);
        if (!guard || guard.leaseKey !== junctionLeaseKey() || guard.session !== this.sessionId || guard.generation !== this.seekGeneration[deck]
          || guard.trackId !== this.clientState.snapshot?.decks[deck]?.track?.trackId) return;
        return this.send(DJ_ENGINE_OPS.deckScratch, { ...queued });
      });
      this.scratchQueues.set(deck, queue);
    }
    const result = queue.enqueue(command);
    if (phase === "begin") void result.catch(() => this.scratchGestures.delete(gestureId));
    if (phase === "end") void result.then(
      () => this.scratchGestures.delete(gestureId),
      () => this.scratchGestures.delete(gestureId),
    );
    return result;
  }

  /**
   * 手を離した瞬間にバックスピンと判定されたジェスチャーを、指の代わりに惰性で回す。
   * 十分に減速したら end を送り、エンジンが即座に通常再生（停止中なら停止）へ着地させる。
   */
  backspin(deck: DeckId, gestureId: string, positionMs: number, velocity: number, weight: number, onError?: (error: unknown) => void): void {
    void this.settleBackspin(deck);
    let reported = false;
    const report = (error: unknown) => { if (!reported) { reported = true; onError?.(error); } };
    const spin = new Backspin({
      velocity, weight, positionMs,
      move: (position, at) => { void this.scratch(deck, "move", position, gestureId, at).catch(report); },
      land: (position) => {
        if (this.backspins.get(deck)?.gestureId === gestureId) this.backspins.delete(deck);
        const landed = this.scratch(deck, "end", position, gestureId);
        landed.catch(report);
        return landed;
      },
    });
    if (spin.active) this.backspins.set(deck, { spin, gestureId });
  }

  /** 惰性で回っているデッキを掴み直す。同じジェスチャーを指で続けるための ID と変位を返す。 */
  grabBackspin(deck: DeckId): { gestureId: string; positionMs: number } | null {
    const current = this.backspins.get(deck);
    if (!current) return null;
    this.backspins.delete(deck);
    const positionMs = current.spin.grab();
    if (!this.scratchGestures.has(current.gestureId)) return null;
    return { gestureId: current.gestureId, positionMs };
  }

  setTempo(deck: DeckId, rate: number): Promise<unknown> {
    return this.continuous(`${deck}:tempo`, DJ_ENGINE_OPS.deckTempoSet, buildTempoParams(deck, rate), deck);
  }

  setBeatgrid(deck: DeckId, trackId: string, bpm: number, firstBeatMs: number, beatsPerBar = 4, beatTimesMs?: number[] | null, beatNumbers?: number[] | null): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.deckBeatgridSet, buildBeatgridParams(deck, trackId, bpm, firstBeatMs, beatsPerBar, beatTimesMs, beatNumbers));
  }

  setKeylock(deck: DeckId, enabled: boolean): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.deckKeylockSet, { deck, enabled });
  }

  setSync(deck: DeckId, enabled: boolean, leader?: DeckId): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.deckSyncSet, { deck, enabled, leader });
  }

  setHotCue(deck: DeckId, index: number, positionMs?: number, quantize?: boolean): Promise<unknown> {
    return this.send(
      DJ_ENGINE_OPS.deckHotcueSet,
      this.withTrack(deck, { ...buildHotcueParams(deck, index, positionMs), ...(quantize === undefined ? {} : { quantize }) })
    );
  }

  jumpToHotCue(deck: DeckId, index: number): Promise<unknown> {
    const settling = this.settleBackspin(deck);
    if (settling) return settling.then(() => this.jumpToHotCue(deck, index));
    return this.send(DJ_ENGINE_OPS.deckHotcueJump, this.withTrack(deck, buildHotcueParams(deck, index)));
  }

  clearHotCue(deck: DeckId, index: number): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.deckHotcueClear, this.withTrack(deck, buildHotcueParams(deck, index)));
  }

  setLoop(deck: DeckId, startMs: number, endMs: number): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.deckLoopSet, this.withTrack(deck, buildLoopParams(deck, startMs, endMs)));
  }

  enableLoop(deck: DeckId, enabled: boolean): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.deckLoopEnable, this.withTrack(deck, { deck, enabled }));
  }

  beatJump(deck: DeckId, beats: number): Promise<unknown> {
    const settling = this.settleBackspin(deck);
    if (settling) return settling.then(() => this.beatJump(deck, beats));
    return this.send(DJ_ENGINE_OPS.deckBeatJump, this.withTrack(deck, buildBeatStepParams(deck, beats)));
  }

  beatLoop(deck: DeckId, beats: number): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.deckBeatLoop, this.withTrack(deck, buildBeatStepParams(deck, beats, true)));
  }

  setQuantize(deck: DeckId, enabled: boolean): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.deckQuantizeSet, this.withTrack(deck, { deck, enabled }));
  }

  setFilter(deck: DeckId, value: number): Promise<unknown> {
    return this.continuous(`${deck}:filter`, DJ_ENGINE_OPS.mixerFilterSet, buildFilterParams(deck, value));
  }

  setTrim(deck: DeckId, gain: number): Promise<unknown> {
    return this.continuous(`${deck}:trim`, DJ_ENGINE_OPS.mixerTrimSet, buildTrimParams(deck, gain));
  }

  /**
   * オン/オフは潰してはいけないので latest-wins キューを通さない。`continuous`
   * は待機中の値を上書きするため、押下の `true` が離した `false` に置き換わり、
   * エフェクトが一瞬しか掛からない（＝掛かっていないように聞こえる）。
   */
  async setFx(deck: DeckId, effect: PadEffect, enabled: boolean, mix: number, depth?: number): Promise<unknown> {
    const params = routePerformanceCommand(DJ_ENGINE_OPS.mixerFxSet, this.withTrack(deck, buildFxParams(deck, effect, enabled, mix, depth)));
    try {
      return await this.send(DJ_ENGINE_OPS.mixerFxSet, params);
    } catch (cause) {
      // A missing reply does not prove the audio processor stayed off.
      if (enabled) await this.send(DJ_ENGINE_OPS.mixerFxSet, { ...params, enabled: false }).catch(() => undefined);
      throw cause;
    }
  }

  setChannelGain(deck: DeckId, gain: number): Promise<unknown> {
    return this.continuous(`${deck}:gain`, DJ_ENGINE_OPS.mixerChannelGain, buildChannelGainParams(deck, gain));
  }

  setEq(deck: DeckId, band: EqBand, gain: number): Promise<unknown> {
    return this.continuous(`${deck}:eq:${band}`, DJ_ENGINE_OPS.mixerChannelEq, buildEqParams(deck, band, gain));
  }

  setPfl(deck: DeckId, enabled: boolean): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.mixerChannelPfl, { deck, enabled });
  }

  setCrossfader(position: number): Promise<unknown> {
    return this.continuous("crossfader", DJ_ENGINE_OPS.mixerCrossfader, buildCrossfaderParams(position));
  }

  setMasterGain(gain: number): Promise<unknown> {
    return this.continuous("master", DJ_ENGINE_OPS.mixerMasterGain, buildMasterGainParams(gain));
  }

  private withTrack(deck: DeckId, params: Record<string, unknown>): Record<string, unknown> {
    return { ...params, trackId: this.clientState.snapshot?.decks[deck]?.track?.trackId };
  }

  private continuous(key: string, op: string, params: Record<string, unknown>, deck?: DeckId): Promise<unknown> {
    params = routePerformanceCommand(op, params);
    let queue = this.controlQueues.get(key);
    if (!queue) {
      queue = new LatestCommandQueue(async target => {
        if (target.leaseKey !== junctionLeaseKey() || target.session !== this.sessionId || target.deck !== undefined && target.generation !== this.seekGeneration[target.deck]) return;
        return await this.send(target.op, target.params);
      });
      this.controlQueues.set(key, queue);
    }
    return queue.enqueue({ op, params, leaseKey: junctionLeaseKey(), session: this.sessionId, deck, generation: deck === undefined ? undefined : this.seekGeneration[deck] });
  }

  startRecording(): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.recordingStart, {});
  }

  async stopRecording(): Promise<RecordingState> {
    const session = this.sessionId;
    let recording = await this.send(DJ_ENGINE_OPS.recordingStop, {}) as RecordingState;
    const recordingPath = recording.path;
    const deadline = Date.now() + 15_000;
    while (recording.active || recording.stopping) {
      if (this.sessionId !== session) throw new Error("録音の保存完了を待つ間に音声エンジンが切り替わりました。");
      if (Date.now() >= deadline) throw new Error("録音ファイルの書き込み完了を待っています。少し待ってから再試行してください。");
      await new Promise(resolve => setTimeout(resolve, 100));
      const current = (await this.refreshSnapshot()).recording;
      if (!current) throw new Error("録音の保存状態を確認できませんでした。");
      if (recordingPath && current.path !== recordingPath) throw new Error("別の録音が開始されたため、元の録音の保存状態を確認できませんでした。");
      recording = current;
    }
    if (this.sessionId !== session) throw new Error("録音の保存完了を待つ間に音声エンジンが切り替わりました。");
    return recording;
  }

  /**
   * 録音の保存先を差し替える。エンジンは録音のたびに読み直すので、
   * プロセスを立て直さなくても次の録音から新しい場所になる。
   */
  setRecordingDirectory(directory: string): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.recordingDirectorySet, { directory });
  }

  /** 保存形式。書き出せない形式はエンジンが拒否する。 */
  setRecordingFormat(format: string): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.recordingFormatSet, { format });
  }

  listAudioDevices(): Promise<unknown> {
    return this.send(DJ_ENGINE_OPS.audioDevicesList, {});
  }

  async setOutputRouting(outputRouting: import("./audio-ready").OutputRouting): Promise<AudioConfig> {
    const config = await this.send(DJ_ENGINE_OPS.audioConfigSet, { outputRouting }) as AudioConfig;
    await this.refreshSnapshot();
    return config;
  }

  async setMicrophone(microphone: Partial<MicrophoneSettings>): Promise<AudioConfig> {
    const config = await this.send(DJ_ENGINE_OPS.audioConfigSet, microphoneCommandParams(microphone)) as AudioConfig;
    await this.refreshSnapshot();
    return config;
  }

  subscribeMeters(enabled: boolean, intervalMs?: number): Promise<unknown> {
    return this.send(
      DJ_ENGINE_OPS.metersSubscribe,
      buildMetersParams(enabled, intervalMs)
    );
  }

  // ---------------------------------------------------------------- 内部

  private attach(): Promise<void> {
    if (this.unsubscribers.length > 0) return Promise.resolve();
    if (this.attaching) return this.attaching;

    this.attaching = (async () => {
      const unsubscribeEvent = await listen<unknown>(EVENT_CHANNEL, (message) => {
        const payload = message.payload;
        if (!isEngineEventMessage(payload)) return;
        if (payload.event === "deck.clock.v2") {
          deckRealtimeStore.ingest(payload.data, payload.engineId);
          if (!deckRealtimeStore.mapping.at(performance.now()) && !this.clockProbing && payload.engineId === this.clientState.snapshot?.engineId) {
            this.clockProbing = true;
            if(!this.clientState.snapshot?.engine.capabilities.includes("waveform.tiles.v2"))void this.refreshSnapshot().catch(()=>{});
            const session = this.sessionId, t0 = performance.now();
            void this.send("engine.clock.probe").then(value => {
              const probe = value as {engineEpoch:string;receivedNativeUs:number;sentNativeUs:number};
              if (session === this.sessionId && probe.engineEpoch === payload.engineId)
                deckRealtimeStore.mapping.probe(t0,probe.receivedNativeUs,probe.sentNativeUs,performance.now());
            }).catch(() => {}).finally(() => {this.clockProbing=false;});
          }
        }
        if (payload.event === "deck.state" && ["loading","empty","error"].includes((payload.data as {status?:string})?.status ?? "")) {
          const deck = (payload.data as {deck?:string})?.deck;
          if (deck) deckRealtimeStore.invalidate(deck);
        }
        if (this.snapshotEventBuffer) {
          this.snapshotEventBuffer.push(payload);
          return;
        }
        const next = reduceEngineEvent(this.clientState, payload);
        if (next === this.clientState) return;
        this.clientState = next;
        this.emitState();
      });
      try {
        const unsubscribeStatus = await listen<unknown>(STATUS_CHANNEL, () => {
          void this.status().then((status) => this.emitStatus(status));
        });
        this.unsubscribers = [unsubscribeEvent, unsubscribeStatus];
      } catch (error) {
        unsubscribeEvent();
        throw error;
      }
    })().catch((error) => {
      // A transient registration failure must be retryable by the next connect.
      this.attaching = null;
      throw error;
    });

    return this.attaching;
  }

  /**
   * 惰性中に位置を動かすと、エンジンは古い基準位置へ着地しようとして音が跳ぶ。
   * 先に着地させてから次の操作を送る。回っていなければ null。
   */
  private settleBackspin(deck: DeckId): Promise<unknown> | null {
    const current = this.backspins.get(deck);
    if (!current) return null;
    this.backspins.delete(deck);
    return Promise.resolve(current.spin.stop()).catch(() => undefined);
  }

  private async releaseScratch(deck: DeckId): Promise<void> {
    // 回転だけ止め、下の end で最後に送った変位へ着地させる。
    this.backspins.get(deck)?.spin.grab();
    this.backspins.delete(deck);
    const gestures = [...this.scratchGestures.entries()].filter(([, item]) => item.deck === deck);
    if (!gestures.length) return;
    const results = await Promise.allSettled(gestures.map(([gestureId, item]) =>
      this.scratch(deck, "end", item.positionMs, gestureId)));
    const failed = results.find((result): result is PromiseRejectedResult => result.status === "rejected");
    if (failed) throw failed.reason;
  }

  private clearScratchState(): void {
    for (const queue of this.controlQueues.values()) queue.clear();
    this.controlQueues.clear();
    for (const queue of this.scratchQueues.values()) queue.clear();
    this.scratchQueues.clear();
    for (const { spin } of this.backspins.values()) spin.grab();
    this.backspins.clear();
    this.scratchGestures.clear();
  }

  private emitState(): void {
    for (const listener of this.stateListeners) {
      listener(this.clientState);
    }
  }

  private beginSnapshotBuffer(): void {
    if (this.snapshotEventBuffer) {
      throw new Error("状態スナップショットの同期はすでに実行中です");
    }
    this.snapshotEventBuffer = [];
  }

  private applyBufferedSnapshot(
    base: EngineClientState,
    snapshot: EngineSnapshot
  ): EngineClientState {
    return applySnapshotWithEvents(base, snapshot, this.snapshotEventBuffer ?? []);
  }

  private emitStatus(status: EngineStatus): void {
    for (const listener of this.statusListeners) {
      listener(status);
    }
  }
}

function toMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

/** アプリ全体で 1 つ。エンジンプロセスも 1 つだけを前提にする。 */
export const djEngineClient = new DjEngineClient();
