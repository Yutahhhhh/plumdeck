/**
 * ネイティブ DJ エンジン（Phase 0）のプロトコル型。
 *
 * Rust 側の対応実装は `native/dj-engine-host/src/protocol.rs`。
 *
 * 単位の約束:
 * - 時間はすべてミリ秒（`...Ms`）。秒は使わない。
 * - `positionFrames` はサンプル位置。`sampleRateHz` と対で解釈する。
 * - `positionMs` / `positionFrames` は曲頭前の無音区間では負。通常の音声時計で進む。
 * - `rate` は再生速度の倍率（1.0 が原速）。パーセントではない。
 * - フェーダー・マスターの `gain` は線形 0.0〜1.0。dB ではない。
 * - EQ の `gain` は線形倍率 0.0〜4.0（1.0 がユニティ＝0 dB）。
 * - `crossfader` は -1.0（A 全開）〜 +1.0（B 全開）。
 *
 * 注意: TypeScript の `enum` は使わない（Node の型ストリップで実行できるようにするため）。
 */

export const DJ_ENGINE_PROTOCOL_VERSION = 1;

export const DECK_IDS = ["A", "B", "C", "D"] as const;
export type DeckId = (typeof DECK_IDS)[number];

export type DeckStatus =
  | "empty"
  | "loading"
  | "ready"
  | "playing"
  | "paused"
  | "error";

export type EngineErrorCode =
  | "malformed_message"
  | "protocol_version_unsupported"
  | "unknown_op"
  | "unsupported_operation"
  | "invalid_params"
  | "session_required"
  | "session_mismatch"
  | "engine_mismatch"
  | "stale_command_id"
  | "deck_not_found"
  | "no_track_loaded"
  | "track_not_ready"
  | "internal"
  /** スーパーバイザが子プロセスの終了を検出したときに合成する。 */
  | "engine_exited";

/** 操作名。文字列直書きを避けるための定数。 */
export const DJ_ENGINE_OPS = {
  ping: "engine.ping",
  snapshot: "state.snapshot",
  deckLoad: "deck.load",
  deckUnload: "deck.unload",
  deckPlay: "deck.play",
  deckPause: "deck.pause",
  deckSeek: "deck.seek",
  deckScratch: "deck.scratch",
  deckTempoSet: "deck.tempo.set",
  deckBeatgridSet: "deck.beatgrid.set",
  deckKeylockSet: "deck.keylock.set",
  deckSyncSet: "deck.sync.set",
  deckHotcueSet: "deck.hotcue.set",
  deckHotcueJump: "deck.hotcue.jump",
  deckHotcueClear: "deck.hotcue.clear",
  deckLoopSet: "deck.loop.set",
  deckLoopEnable: "deck.loop.enable",
  deckBeatJump: "deck.beatjump",
  deckBeatLoop: "deck.loop.beats",
  deckQuantizeSet: "deck.quantize.set",
  mixerFilterSet: "mixer.filter.set",
  mixerTrimSet: "mixer.trim.set",
  mixerFxSet: "mixer.fx.set",
  mixerChannelGain: "mixer.channel.gain",
  mixerChannelEq: "mixer.channel.eq",
  mixerChannelPfl: "mixer.channel.pfl",
  mixerCrossfader: "mixer.crossfader",
  mixerMasterGain: "mixer.master.gain",
  mixerHeadphoneGain: "mixer.headphone.gain",
  mixerHeadphoneMix: "mixer.headphone.mix",
  audioDevicesList: "audio.devices.list",
  audioConfigGet: "audio.config.get",
  audioConfigSet: "audio.config.set",
  metersSubscribe: "meters.subscribe",
  recordingStart: "recording.start",
  recordingStop: "recording.stop",
  recordingDirectorySet: "recording.directory.set",
  recordingFormatSet: "recording.format.set",
  deckPitchbend: "deck.pitchbend",
  deckKeyShift: "deck.key.shift",
  deckKeySync: "deck.key.sync",
  deckKeyReset: "deck.key.reset",
  deckSlipSet: "deck.slip.set",
  deckReverseSet: "deck.reverse.set",
  deckSlipReverseSet: "deck.slipReverse.set",
  mixerColorFxSet: "mixer.colorfx.set",
  mixerBeatFxSet: "mixer.beatfx.set",
  mixerChannelOrientation: "mixer.channel.orientation",
  /** Contract for H-12; gated by `mixer.crossfader.curve`. */
  mixerCrossfaderCurve: "mixer.crossfader.curve",
  engineAudioHealth: "engine.audioHealth",
} as const;

/**
 * Capability names the UI gates on. A host declares only what it implements;
 * controls for undeclared capabilities stay hidden or disabled with a reason.
 * Each entry documents the behaviour an engine promises by declaring it; the
 * Mixxx host and the Rust engine implement the same contract. Names marked
 * "Rust" are not declared by the Mixxx host.
 */
export const DJ_ENGINE_CAPABILITIES = {
  deckKey: "deck.key",
  /**
   * H-4: the engine knows each loaded track's key (`track.musicalKey`), and
   * `deck.key.sync` moves the deck to the sync leader's (else another playing
   * deck's) effective key by the shortest whole-semitone step, counting the
   * relative major/minor as the same key (Am with Cm: -3). Turns MT on; KEY
   * RESET restores the MT state from before the first shift (H-5).
   */
  deckKeySync: "deck.key.sync",
  deckSlip: "deck.slip",
  deckReverse: "deck.reverse",
  deckPitchbend: "deck.pitchbend",
  deckSync: "deck.sync",
  /**
   * H-2: every deck's `syncLeader` / `syncEnabled` mirror the engine each tick,
   * including leaders the engine elects by itself. H-3 (interim on Mixxx): SYNC
   * ON, or play while synced, aligns the follower's beat phase once (<=2 ms).
   */
  deckSyncLeaderTelemetry: "deck.sync.leader.v2",
  /** H-11: loops 1/64..512 beats and beat jumps 1/64..128 beats. */
  deckLoopExtended: "deck.loop.range.v2",
  mixerColorFx: "mixer.colorfx",
  mixerBeatFx: "mixer.beatfx",
  mixerBeatFxRelease: "mixer.beatfx.release",
  /**
   * H-8: `mixer.fx.set` accepts `beats` (1/16..16) and native `params`, pads
   * load the beat processors (ROLL, TRANS, SPIRAL …), insert types run fully
   * wet with DEPTH on the macro only, and echo/reverb pads keep a tail after
   * release (`fx.tail` while it decays).
   */
  mixerFxBeats: "mixer.fx.beats",
  /** Rust: RELEASE pads (ECHO OUT, BACK SPIN, VINYL BRAKE). */
  mixerFxRelease: "mixer.fx.release",
  /**
   * H-10: BEAT FX beats 1/16..16 on the delay/roll processors; echo/delay
   * types keep the dry signal at every LEVEL; PITCH is always fully wet and
   * LEVEL spans -1..+1 octave; echo/reverb OFF keeps a tail (`beatFx.tail`).
   */
  mixerBeatFxRange: "mixer.beatfx.range.v2",
  /** H-12: `mixer.crossfader.curve` sets smooth / standard / sharp; channels report `crossfaderGain`. */
  mixerCrossfaderCurve: "mixer.crossfader.curve",
  mixerCrossfader: "mixer.crossfader",
  mixerPfl: "mixer.pfl",
  /** H-7: `mixer.headphone.gain` (0..1) and `mixer.headphone.mix` (-1 cue .. 1 master); declared with a cue output. */
  mixerHeadphone: "mixer.headphone",
  /**
   * H-1: `meters.subscribe` streams `meters` events (20..2000 ms). Levels are
   * linear amplitudes: `peak` is held since the previous event, `rms` is the
   * latest audio block; channel meters are taken before the channel fader.
   */
  meters: "meters",
  sampler: "sampler",
  recording: "recording",
  /** Rust: pad modes that have no engine op today. */
  deckSlicer: "deck.slicer",
  deckSequencer: "deck.sequencer",
  deckCensor: "deck.censor",
  deckStems: "deck.stems",
} as const;

export const CROSSFADER_CURVES = ["smooth", "standard", "sharp"] as const;
export type CrossfaderCurve = (typeof CROSSFADER_CURVES)[number];

export type DjEngineOp = (typeof DJ_ENGINE_OPS)[keyof typeof DJ_ENGINE_OPS];

export type ScratchPhase = "begin" | "move" | "end";
export interface ScratchCommand {
  capturedNativeUs?: number;
  keepalive?: boolean;
  deck: DeckId;
  phase: ScratchPhase;
  /** Cumulative displacement in source-track milliseconds; begin is zero. */
  positionMs: number;
  gestureId: string;
}

/** イベント名。 */
export const DJ_ENGINE_EVENTS = {
  deckState: "deck.state",
  deckPosition: "deck.position",
  deckLoaded: "deck.loaded",
  deckLoadFailed: "deck.load.failed",
  mixerState: "mixer.state",
  audioConfig: "audio.config",
  meters: "meters",
  sessionInvalidated: "session.invalidated",
  recordingState: "recording.state",
} as const;

/** 受理される値域。エンジン側 (`engine.rs`) と同じ値を持つ。 */
export const DJ_ENGINE_RANGES = {
  rate: { min: 0.25, max: 4.0 },
  channelGain: { min: 0, max: 1 },
  eqGain: { min: 0, max: 4 },
  masterGain: { min: 0, max: 1 },
  crossfader: { min: -1, max: 1 },
  hotCueIndex: { min: 0, max: 15 },
  metersIntervalMs: { min: 20, max: 2000 },
} as const;

export const EQ_BANDS = ["low", "mid", "high"] as const;
export type EqBand = (typeof EQ_BANDS)[number];

/** plumdeck からエンジンへ渡す曲記述子。正本は plumdeck 側の DB。 */
export interface AssetWaveform {
  bins_per_second: number; duration_ms: number; amplitude_scale: number;
  peaks: number[]; low: number[]; mid: number[]; high: number[];
}
export interface TrackDescriptor {
  assetId?: string;
  waveform?: AssetWaveform;
  localTrackId?: number | null;
  musicalKey?: string;
  trackId: string;
  path: string;
  durationMs: number;
  title?: string;
  artist?: string;
  bpm?: number;
  sampleRateHz?: number;
  channels?: number;
  beatgridOffsetMs?: number;
  beatsPerBar?: number;
  beatTimesMs?: number[];
  beatNumbers?: number[];
  hotCues?: (number | null)[];
}

export interface LoadedTrack {
  assetId?: string;
  waveform?: AssetWaveform;
  localTrackId?: number | null;
  trackId: string;
  path: string;
  title: string | null;
  artist: string | null;
  durationMs: number;
  bpm: number | null;
  sampleRateHz: number;
  channels: number;
  beatgridOffsetMs?: number;
  beatsPerBar?: number;
  beatTimesMs?: number[];
  beatNumbers?: number[];
  beatgridApplied?: boolean;
}

export interface LoopRegion {
  startMs: number;
  endMs: number;
  enabled: boolean;
}

export interface DeckState {
  keyShift?: number;
  musicalKey?: number;
  slip?: boolean;
  reverse?: boolean;
  slipReverse?: boolean;
  deck: DeckId;
  status: DeckStatus;
  track: LoadedTrack | null;
  positionMs: number;
  positionFrames: number;
  quantize?: boolean;
  /** Optional for compatibility with engine snapshots predating scratch. */
  scratching?: boolean;
  rate: number;
  keylock: boolean;
  syncEnabled: boolean;
  syncLeader: DeckId | null;
  effectiveBpm: number | null;
  /** 8 スロット。未設定は null。 */
  hotCues: (number | null)[];
  loopRegion: LoopRegion | null;
  lastError: string | null;
  loadId: number | null;
  loadGeneration?: number;
}

/**
 * パッドから叩けるエフェクト。エンジン側 (`host.cpp` の `padEffects`) と同じ集合。
 * ホストは org.plumdeck.effects.<name>（拍同期の自前プロセッサ）を優先し、
 * 無ければ org.mixxx.effects.<name> を読む。`org.plumdeck.effects.` 付きの名前も受け付ける。
 */
export const PAD_EFFECTS = ["echo", "reverb", "flanger", "phaser", "filter", "bitcrusher", "distortion", "autopan", "tremolo", "moogladder4filter", "lowcutecho", "mtdelay", "spiral", "enigmajet", "sliproll", "roll", "pitchshift"] as const;
export type PadEffect = (typeof PAD_EFFECTS)[number];
/**
 * `enabled` is the pad's held state; `tail` is true while an echo/reverb
 * keeps decaying after release; `nativeEnabled` is the chain itself.
 */
export interface ChannelFx { effect: PadEffect; enabled: boolean; mix: number; depth?: number; beats?: number; tail?: boolean; nativeEnabled?: boolean; parameters?: Record<string, number> }
export interface ChannelColorFx { effect: string; amount: number }

export interface ChannelState {
  deck: DeckId;
  gain: number;
  eqLow: number;
  eqMid: number;
  eqHigh: number;
  pfl: boolean;
  trim?: number;
  filter?: number;
  fx?: ChannelFx;
  /** Color FX knob as last set; `filter` is derived from it and drifts for non-filter types. */
  colorFx?: ChannelColorFx;
  /** Crossfader assign: 0 = A, 1 = THRU, 2 = B. */
  orientation?: number;
  /** Linear gain the crossfader currently applies to this channel (curve and assign included). */
  crossfaderGain?: number;
}

export interface BeatFxState {
  bpm?: number;
  auto?: boolean;
  effect: string;
  target: string;
  enabled: boolean;
  mix: number;
  beats: number;
  /** RELEASE FX is held. */
  releaseActive?: boolean;
  /** An echo/reverb tail is still decaying after OFF. */
  tail?: boolean;
}

export interface MixerState {
  beatFx?: BeatFxState;
  crossfader: number;
  crossfaderCurve?: CrossfaderCurve;
  masterGain: number;
  headphoneGain: number;
  headphoneMix: number;
  channels: Record<DeckId, ChannelState>;
}

export interface MicrophoneSettings {
  deviceId: string | null;
  /** Zero-based mono input channel. */
  channel: number;
  enabled: boolean;
  gain: number;
  duckingEnabled: boolean;
  duckingStrength: number;
}

export interface MicrophoneState extends MicrophoneSettings {
  available: boolean;
  applied: boolean;
  level: number;
  reason?: string;
}

export interface AudioDevice {
  id: string;
  name: string;
  displayName: string;
  outputChannels: number;
  inputChannels?: number;
  isDefault: boolean;
  isDefaultInput?: boolean;
}

export interface AudioConfig {
  deviceId: string | null;
  sampleRateHz: number;
  bufferFrames: number;
  masterChannels: [number, number];
  /** null means the host has not configured a separate cue/PFL output. */
  pflChannels: [number, number] | null;
  /** 実デバイスに適用されたか。シミュレータでは常に false。 */
  applied: boolean;
  /** Optional host explanation when the requested routing was not applied. */
  reason?: string;
  microphone?: MicrophoneState;
}

export interface MetersPayload {
  simulated: boolean;
  master: { peak: number; rms: number };
  /** Headphone (cue) bus, when the engine has one. */
  headphone?: { peak: number; rms: number };
  /** Pre-fader channel levels. */
  channels: Record<string, { peak: number; rms: number; pfl: boolean }>;
}

export interface EngineInfo {
  name: string;
  version: string;
  /** `"simulator"` か、将来の実装名。 */
  implementation: string;
  /** true の間は音が出ていない。UI はこれを隠さない。 */
  simulated: boolean;
  deterministic: boolean;
  /** Real hosts set false while device initialization is unavailable/failed. */
  audioAvailable?: boolean;
  decks: DeckId[];
  capabilities: string[];
}

export interface EngineSnapshot {
  rev: number;
  /** Snapshot includes all engine events through this sequence number. */
  seq: number;
  engineId: string;
  sessionId: string | null;
  engineTimeMs: number;
  engine: EngineInfo;
  decks: Record<DeckId, DeckState>;
  mixer: MixerState;
  audio: AudioConfig;
  recording?: RecordingState;
  meters: { enabled: boolean; intervalMs: number; simulated: boolean };
}

export interface RecordingState {
  active: boolean;
  /** The encoder is flushing; playback/rename must wait until this is false. */
  stopping?: boolean;
  path: string | null;
  startedAt: string | null;
  elapsedMs: number;
  /** Authoritative count from Mixxx EngineRecord's record-tap clock. */
  frameCount?: number;
  sampleRateHz?: number;
  timelineQuality?: "recording_frame_clock" | "engine_sampled" | "incomplete";
  timelineDroppedEvents?: number;
  timeline?: Array<{ eventKey: string; deck: DeckId; loadGeneration: number; trackId: number | string; title?: string; artist?: string; startFrame: number; endFrame: number | null; source: "engine_observed" }>;
  error: string | null;
  /** このホストが実際に書き出せる形式。ビルド構成で決まる。 */
  formats?: { name: string; label: string; lossless: boolean; extension: string }[];
  /** 次の録音で使う形式。 */
  format?: string;
}

export interface EngineError {
  code: EngineErrorCode;
  message: string;
  retryable: boolean;
  details?: unknown;
}

/**
 * エンジンからのイベント。`data` は `unknown` のまま受け取り、
 * 使う側で絞り込む。将来 C++ ホストが未知のイベントを送っても壊れないため。
 */
export interface EngineEventMessage {
  kind: "event";
  protocol: number;
  engineId: string;
  event: string;
  seq: number;
  rev: number;
  data: unknown;
  engineTimeMs: number;
}

/** Tauri コマンド `dj_engine_status` の戻り値。 */
export interface EngineStatus {
  installed: boolean;
  running: boolean;
  binaryPath: string | null;
  engineId: string | null;
  sessionId: string | null;
  protocol: number | null;
  simulated: boolean;
  implementation: string | null;
  lastError: string | null;
  /** 未インストールなど、UI に出せる理由。 */
  detail: string | null;
}

/** Tauri コマンド `dj_engine_connect` の戻り値。 */
export interface EngineConnection {
  sessionId: string;
  engineId: string;
  protocol: number;
  engine: EngineInfo;
  snapshot: EngineSnapshot;
  rev: number;
}

/** Tauri コマンド `dj_engine_send` の戻り値。 */
export interface EngineReply {
  ok: boolean;
  data?: unknown;
  error?: EngineError;
  rev?: number;
}

/** クライアント側で保持する派生状態。 */
export interface EngineClientState {
  snapshot: EngineSnapshot | null;
  /** 適用済みの最大リビジョン。 */
  rev: number;
  /** 直近に適用したイベント通し番号。0 は未受信。 */
  lastSeq: number;
  /** 取りこぼしたイベント数。> 0 ならスナップショットを取り直す。 */
  droppedEvents: number;
  meters: MetersPayload | null;
  /** セッションが失効した（再接続が必要）。 */
  sessionInvalidated: boolean;
}
