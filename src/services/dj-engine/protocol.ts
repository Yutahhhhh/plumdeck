/**
 * DJ エンジンプロトコルの純粋ロジック。
 *
 * このファイルは Tauri にも React にも依存しない。
 * `node --test` から直接実行できるようにするため、副作用も持たせない。
 * （テストは `native/dj-engine-host/tests-node/protocol.test.ts`）
 */

import {
  CROSSFADER_CURVES,
  DECK_IDS,
  DJ_ENGINE_EVENTS,
  DJ_ENGINE_RANGES,
  EQ_BANDS,
  PAD_EFFECTS,
} from "../../types/dj-engine.ts";
import type {
  DeckId,
  DeckState,
  DeckStatus,
  EngineClientState,
  EngineError,
  EngineEventMessage,
  EngineSnapshot,
  EqBand,
  MetersPayload,
  ScratchCommand,
  ScratchPhase,
  TrackDescriptor,
  PadEffect,
} from "../../types/dj-engine.ts";

/** 送信前のクライアント側検証で投げるエラー。 */
export class DjEngineValidationError extends Error {
  readonly field: string;

  constructor(field: string, message: string) {
    super(message);
    this.name = "DjEngineValidationError";
    this.field = field;
  }
}

// ------------------------------------------------------------------ 検証

function assertFinite(field: string, value: number): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new DjEngineValidationError(field, `${field} は有限の数値である必要があります`);
  }
  return value;
}

function assertRange(
  field: string,
  value: number,
  range: { min: number; max: number }
): number {
  assertFinite(field, value);
  if (value < range.min || value > range.max) {
    throw new DjEngineValidationError(
      field,
      `${field} は ${range.min} 以上 ${range.max} 以下である必要があります（指定値: ${value}）`
    );
  }
  return value;
}

export function assertDeckId(value: string): DeckId {
  if (!isDeckId(value)) {
    throw new DjEngineValidationError(
      "deck",
      `デッキは ${DECK_IDS.join(" / ")} のいずれかです（指定値: ${value}）`
    );
  }
  return value;
}

export function assertEqBand(value: string): EqBand {
  if (!EQ_BANDS.includes(value as EqBand)) {
    throw new DjEngineValidationError(
      "band",
      `band は ${EQ_BANDS.join(" / ")} のいずれかです（指定値: ${value}）`
    );
  }
  return value as EqBand;
}

// ------------------------------------------------------------------ パラメータ生成

/**
 * ロード用パラメータ。ファイルパスは **データとして** 渡すだけで、
 * シェルにも URL にも埋め込まない。
 */
export function buildLoadParams(
  deck: DeckId,
  track: TrackDescriptor
): Record<string, unknown> {
  assertDeckId(deck);
  if (!track.trackId.trim()) {
    throw new DjEngineValidationError("track.trackId", "trackId が空です");
  }
  if (!track.path.trim()) {
    throw new DjEngineValidationError("track.path", "path が空です");
  }
  assertFinite("track.durationMs", track.durationMs);
  if (track.durationMs <= 0) {
    throw new DjEngineValidationError(
      "track.durationMs",
      "durationMs は正の値である必要があります"
    );
  }
  if (track.bpm !== undefined) {
    assertRange("track.bpm", track.bpm, { min: 20, max: 300 });
  }
  if (track.hotCues !== undefined && (!Array.isArray(track.hotCues) || ![8, 16].includes(track.hotCues.length)
    || !track.hotCues.every(cue => cue === null || typeof cue === "number" && Number.isFinite(cue) && cue >= 0 && cue < track.durationMs))) {
    throw new DjEngineValidationError("track.hotCues", "Hot Cue は曲内の時刻または null を8個または16個指定してください");
  }
  return { deck, track };
}

/**
 * `extended` follows the `deck.loop.range.v2` capability: loops 1/64..512 and
 * jumps up to ±128 beats instead of the Mixxx host's 1/8..64.
 */
export function buildBeatStepParams(deck: DeckId, beats: number, loop = false, extended = false): Record<string, unknown> {
  assertDeckId(deck);
  const max = extended ? (loop ? 512 : 128) : 64, min = extended ? 1 / 64 : .125;
  assertRange("beats", beats, { min: loop ? min : -max, max });
  if (Math.abs(beats) < min) throw new DjEngineValidationError("beats", `拍数は ${extended ? "1/64" : "1/8"} 拍以上で指定してください`);
  return { deck, beats };
}

export function buildKeyShiftParams(deck: DeckId, semitones: number): Record<string, unknown> {
  assertDeckId(deck);
  assertRange("semitones", semitones, { min: -12, max: 12 });
  if (!Number.isInteger(semitones)) throw new DjEngineValidationError("semitones", "キーは半音単位で指定してください");
  return { deck, semitones };
}

/** Temporary speed offset; the host releases it ~150 ms after the last message. */
export function buildPitchbendParams(deck: DeckId, amount: number): Record<string, unknown> {
  assertDeckId(deck);
  assertRange("amount", amount, { min: -0.75, max: 0.75 });
  return { deck, amount };
}

export const COLOR_FX_EFFECTS = ["filter", "echo", "pitchshift", "whitenoise"] as const;
export function buildColorFxParams(deck: DeckId, effect: string, amount: number): Record<string, unknown> {
  assertDeckId(deck);
  if (!(COLOR_FX_EFFECTS as readonly string[]).includes(effect)) throw new DjEngineValidationError("effect", `Color FX は ${COLOR_FX_EFFECTS.join(" / ")} のいずれかです`);
  assertRange("amount", amount, { min: -1, max: 1 });
  return { deck, effect, amount };
}

/** Crossfader assign: 0 = A, 1 = THRU, 2 = B. */
export function buildOrientationParams(deck: DeckId, orientation: number): Record<string, unknown> {
  assertDeckId(deck);
  if (orientation !== 0 && orientation !== 1 && orientation !== 2) throw new DjEngineValidationError("orientation", "割り当ては 0 (A) / 1 (THRU) / 2 (B) です");
  return { deck, orientation };
}

export function buildCrossfaderCurveParams(curve: string): Record<string, unknown> {
  if (!(CROSSFADER_CURVES as readonly string[]).includes(curve)) throw new DjEngineValidationError("curve", `カーブは ${CROSSFADER_CURVES.join(" / ")} のいずれかです`);
  return { curve };
}

export function buildFilterParams(deck: DeckId, value: number): Record<string, unknown> {
  assertDeckId(deck); assertRange("value", value, { min: -1, max: 1 });
  return { deck, value };
}

export function buildTrimParams(deck: DeckId, gain: number): Record<string, unknown> {
  assertDeckId(deck); assertRange("gain", gain, { min: 0, max: 2 });
  return { deck, gain };
}

/** 送信の検証と受信スナップショットの検証で同じ値を使う。 */
export const FX_MIX_RANGE = { min: 0, max: 1 } as const;
export const FX_DEPTH_RANGE = { min: 0, max: 1 } as const;

/**
 * PAD FX on the deck chain. `beats` (1/16..16) and native `params` are sent
 * only to hosts declaring `mixer.fx.beats`; such hosts run insert types fully
 * wet and keep echo/reverb tails after `enabled: false` (see the capability).
 */
export function buildFxParams(deck: DeckId, effect: PadEffect, enabled: boolean, mix: number, depth?: number, beats?: number, parameters?: Record<string, number>): Record<string, unknown> {
  assertDeckId(deck);
  if (!PAD_EFFECTS.includes(effect) || typeof enabled !== "boolean") throw new DjEngineValidationError("effect", "エフェクトの種類または状態が不正です");
  assertRange("mix", mix, FX_MIX_RANGE);
  const params: Record<string, unknown> = { deck, effect, enabled, mix };
  if (depth !== undefined) { assertRange("depth", depth, FX_DEPTH_RANGE); params.depth = depth; }
  if (beats !== undefined) { assertRange("beats", beats, { min: 1 / 16, max: 16 }); params.beats = beats; }
  if (parameters !== undefined) {
    for (const [key, value] of Object.entries(parameters)) assertFinite(`params.${key}`, value);
    params.params = parameters;
  }
  return params;
}

export function buildSeekParams(
  deck: DeckId,
  positionMs: number,
  durationMs?: number
): Record<string, unknown> {
  assertDeckId(deck);
  const max = durationMs === undefined ? Number.MAX_SAFE_INTEGER : durationMs;
  assertRange("positionMs", positionMs, { min: -60_000, max });
  return { deck, positionMs };
}

export function buildScratchParams(
  deck: DeckId,
  phase: ScratchPhase,
  positionMs: number,
  gestureId: string,
): ScratchCommand {
  assertDeckId(deck);
  if (phase !== "begin" && phase !== "move" && phase !== "end") {
    throw new DjEngineValidationError("phase", "phase は begin / move / end のいずれかです");
  }
  assertRange("positionMs", positionMs, { min: -60_000, max: 60_000 });
  if (!gestureId.trim()) throw new DjEngineValidationError("gestureId", "gestureId が必要です");
  if (phase === "begin" && positionMs !== 0) {
    throw new DjEngineValidationError("positionMs", "scratch begin の positionMs は 0 である必要があります");
  }
  return { deck, phase, positionMs, gestureId };
}

export function buildTempoParams(deck: DeckId, rate: number): Record<string, unknown> {
  assertDeckId(deck);
  assertRange("rate", rate, DJ_ENGINE_RANGES.rate);
  return { deck, rate };
}

export function buildBeatgridParams(deck: DeckId, trackId: string, bpm: number, firstBeatMs: number, beatsPerBar = 4, beatTimesMs?: number[] | null, beatNumbers?: number[] | null): Record<string, unknown> {
  assertDeckId(deck);
  if (!trackId.trim()) throw new DjEngineValidationError("trackId", "曲IDが必要です");
  assertRange("bpm", bpm, { min: 20, max: 300 });
  assertRange("firstBeatMs", firstBeatMs, { min: 0, max: Number.MAX_SAFE_INTEGER });
  assertRange("beatsPerBar", beatsPerBar, { min: 1, max: 16 });
  if (!Number.isInteger(beatsPerBar)) throw new DjEngineValidationError("beatsPerBar", "拍子は整数で指定してください");
  if (beatTimesMs) {
    if (beatTimesMs.length < 2 || beatTimesMs.length > 100000 || beatTimesMs.some((ms, i) => !Number.isFinite(ms) || ms < 0 || i > 0 && ms <= beatTimesMs[i - 1])) throw new DjEngineValidationError("beatTimesMs", "拍位置は2〜100000個、時刻順で指定してください");
  }
  if (beatNumbers && (!beatTimesMs || beatNumbers.length !== beatTimesMs.length || beatNumbers.some(n => !Number.isInteger(n) || n < 1 || n > beatsPerBar))) throw new DjEngineValidationError("beatNumbers", "小節内拍番号が不正です");
  return { deck, trackId, bpm, firstBeatMs, beatsPerBar, ...(beatTimesMs ? { beatTimesMs } : {}), ...(beatNumbers ? { beatNumbers } : {}) };
}

export function buildChannelGainParams(
  deck: DeckId,
  gain: number
): Record<string, unknown> {
  assertDeckId(deck);
  assertRange("gain", gain, DJ_ENGINE_RANGES.channelGain);
  return { deck, gain };
}

export function buildEqParams(
  deck: DeckId,
  band: EqBand,
  gain: number
): Record<string, unknown> {
  assertDeckId(deck);
  assertEqBand(band);
  assertRange("gain", gain, DJ_ENGINE_RANGES.eqGain);
  return { deck, band, gain };
}

export function buildCrossfaderParams(position: number): Record<string, unknown> {
  assertRange("position", position, DJ_ENGINE_RANGES.crossfader);
  return { position };
}

export function buildHeadphoneGainParams(gain: number): Record<string, unknown> {
  assertRange("gain", gain, { min: 0, max: 1 }); return { gain };
}

export function buildHeadphoneMixParams(mix: number): Record<string, unknown> {
  assertRange("mix", mix, { min: -1, max: 1 }); return { mix };
}

export function buildMasterGainParams(gain: number): Record<string, unknown> {
  assertRange("gain", gain, DJ_ENGINE_RANGES.masterGain);
  return { gain };
}

export function buildHotcueParams(
  deck: DeckId,
  index: number,
  positionMs?: number
): Record<string, unknown> {
  assertDeckId(deck);
  assertRange("index", index, DJ_ENGINE_RANGES.hotCueIndex);
  if (!Number.isInteger(index)) {
    throw new DjEngineValidationError("index", "index は整数である必要があります");
  }
  if (positionMs === undefined) {
    return { deck, index };
  }
  assertRange("positionMs", positionMs, { min: 0, max: Number.MAX_SAFE_INTEGER });
  return { deck, index, positionMs };
}

export function buildLoopParams(
  deck: DeckId,
  startMs: number,
  endMs: number
): Record<string, unknown> {
  assertDeckId(deck);
  assertFinite("startMs", startMs);
  assertFinite("endMs", endMs);
  if (endMs <= startMs) {
    throw new DjEngineValidationError(
      "endMs",
      "endMs は startMs より大きい必要があります"
    );
  }
  return { deck, startMs, endMs };
}

export function buildMetersParams(
  enabled: boolean,
  intervalMs?: number
): Record<string, unknown> {
  if (intervalMs === undefined) {
    return { enabled };
  }
  assertRange("intervalMs", intervalMs, DJ_ENGINE_RANGES.metersIntervalMs);
  return { enabled, intervalMs };
}

// ------------------------------------------------------------------ 型ガード

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function readNumber(record: Record<string, unknown>, key: string): number | null {
  const value = record[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

export function isDeckId(value: unknown): value is DeckId {
  return typeof value === "string" && DECK_IDS.includes(value as DeckId);
}

function isDeckStatus(value: unknown): value is DeckStatus {
  return (
    value === "empty" ||
    value === "loading" ||
    value === "ready" ||
    value === "playing" ||
    value === "paused" ||
    value === "error"
  );
}

export function isEngineEventMessage(value: unknown): value is EngineEventMessage {
  if (!isRecord(value)) return false;
  return (
    value.kind === "event" &&
    value.protocol === 1 &&
    typeof value.event === "string" &&
    typeof value.engineId === "string" &&
    typeof value.seq === "number" &&
    Number.isSafeInteger(value.seq) &&
    value.seq >= 0 &&
    typeof value.rev === "number" &&
    Number.isSafeInteger(value.rev) &&
    value.rev >= 0 &&
    typeof value.engineTimeMs === "number" &&
    Number.isFinite(value.engineTimeMs) &&
    "data" in value
  );
}

/**
 * `deck.state` のペイロードを検証する。
 * 判別に必要なフィールドだけ実際に検査し、残りは記述子として受け入れる。
 * 相手が別実装（C++ ホスト）に替わっても、壊れたイベントで UI を壊さないための境界。
 */
function parseDeckState(data: unknown): DeckState | null {
  if (!isRecord(data)) return null;
  if (!isDeckId(data.deck)) return null;
  if (!isDeckStatus(data.status)) return null;
  if (
    !isFiniteNumber(data.positionMs) ||
    !isFiniteNumber(data.positionFrames) ||
    !(data.scratching === undefined || typeof data.scratching === "boolean") ||
    !(data.quantize === undefined || typeof data.quantize === "boolean") ||
    !isFiniteNumber(data.rate) ||
    typeof data.keylock !== "boolean" ||
    typeof data.syncEnabled !== "boolean" ||
    !(data.syncLeader === null || isDeckId(data.syncLeader)) ||
    !(data.effectiveBpm === null || isFiniteNumber(data.effectiveBpm)) ||
    !Array.isArray(data.hotCues) ||
    ![8, 16].includes(data.hotCues.length) ||
    !data.hotCues.every((cue) => cue === null || isFiniteNumber(cue)) ||
    !isNullableString(data.lastError) ||
    !(data.loadGeneration === undefined || Number.isSafeInteger(data.loadGeneration) && (data.loadGeneration as number) >= 0) ||
    !(data.loadId === null || Number.isSafeInteger(data.loadId))
  ) return null;
  if (data.track !== null) {
    if (!isRecord(data.track)) return null;
    if (
      typeof data.track.trackId !== "string" ||
      typeof data.track.path !== "string" ||
      !isNullableString(data.track.title) ||
      !isNullableString(data.track.artist) ||
      !isFiniteNumber(data.track.durationMs) ||
      !(data.track.bpm === null || isFiniteNumber(data.track.bpm)) ||
      !isFiniteNumber(data.track.sampleRateHz) ||
      !isFiniteNumber(data.track.channels) ||
      (data.track.beatgridOffsetMs !== undefined && !isFiniteNumber(data.track.beatgridOffsetMs))
    ) return null;
  }
  if (data.loopRegion !== null) {
    if (!isRecord(data.loopRegion)) return null;
    if (
      !isFiniteNumber(data.loopRegion.startMs) ||
      !isFiniteNumber(data.loopRegion.endMs) ||
      typeof data.loopRegion.enabled !== "boolean"
    ) return null;
  }
  return data as unknown as DeckState;
}

function parseMixerState(data: unknown): EngineSnapshot["mixer"] | null {
  if (!isRecord(data) || !isRecord(data.channels)) return null;
  if (
    !isFiniteNumber(data.crossfader) ||
    !isFiniteNumber(data.masterGain) ||
    !isFiniteNumber(data.headphoneGain) ||
    !isFiniteNumber(data.headphoneMix)
  ) return null;
  for (const deck of ["A", "B"] as const) {
    const channel = data.channels[deck];
    if (!isRecord(channel) || channel.deck !== deck) return null;
    if (
      !isFiniteNumber(channel.gain) ||
      !isFiniteNumber(channel.eqLow) ||
      !isFiniteNumber(channel.eqMid) ||
      !isFiniteNumber(channel.eqHigh) ||
      typeof channel.pfl !== "boolean"
    ) return null;
  }
  for (const deck of ["C", "D"] as const) {
    const channel = data.channels[deck];
    if (channel === undefined) continue;
    if (!isRecord(channel) || channel.deck !== deck || !isFiniteNumber(channel.gain) ||
        !isFiniteNumber(channel.eqLow) || !isFiniteNumber(channel.eqMid) ||
        !isFiniteNumber(channel.eqHigh) || typeof channel.pfl !== "boolean") return null;
  }
  for (const deck of DECK_IDS) {
    const channel = data.channels[deck];
    if (!isRecord(channel)) continue;
    if (channel.trim !== undefined && (!isFiniteNumber(channel.trim) || channel.trim < 0 || channel.trim > 2)) return null;
    if (channel.filter !== undefined && (!isFiniteNumber(channel.filter) || channel.filter < -1 || channel.filter > 1)) return null;
    // An empty or not-yet-loaded PAD FX slot (the host reports effect "" until
    // the first press) is "no effect", never a reason to drop the whole mixer
    // state: that rejected every snapshot and left the decks unusable.
    if (channel.fx !== undefined && (!isRecord(channel.fx) || !PAD_EFFECTS.includes(channel.fx.effect as PadEffect)
      || typeof channel.fx.enabled !== "boolean" || !isFiniteNumber(channel.fx.mix)
      || channel.fx.mix < FX_MIX_RANGE.min || channel.fx.mix > FX_MIX_RANGE.max)) delete channel.fx;
  }
  return data as unknown as EngineSnapshot["mixer"];
}

function isChannelPair(value: unknown): value is [number, number] {
  return Array.isArray(value) && value.length === 2 && value.every(Number.isSafeInteger);
}

function parseAudioConfig(data: unknown): EngineSnapshot["audio"] | null {
  if (!isRecord(data)) return null;
  if (
    !isNullableString(data.deviceId) ||
    !isFiniteNumber(data.sampleRateHz) ||
    !isFiniteNumber(data.bufferFrames) ||
    !isChannelPair(data.masterChannels) ||
    !(data.pflChannels === null || isChannelPair(data.pflChannels)) ||
    typeof data.applied !== "boolean" ||
    !(data.reason === undefined || typeof data.reason === "string")
  ) return null;
  return data as unknown as EngineSnapshot["audio"];
}

function parseMeters(data: unknown): MetersPayload | null {
  if (!isRecord(data) || typeof data.simulated !== "boolean" || !isRecord(data.master)) {
    return null;
  }
  if (!isFiniteNumber(data.master.peak) || !isFiniteNumber(data.master.rms)) return null;
  if (!isRecord(data.channels)) return null;
  for (const channel of Object.values(data.channels)) {
    if (
      !isRecord(channel) ||
      !isFiniteNumber(channel.peak) ||
      !isFiniteNumber(channel.rms) ||
      typeof channel.pfl !== "boolean"
    ) return null;
  }
  return data as unknown as MetersPayload;
}

function parseRecording(data: unknown): EngineSnapshot["recording"] | null {
  if (!isRecord(data) || typeof data.active !== "boolean" ||
      !isNullableString(data.path) || !isNullableString(data.startedAt) ||
      !isFiniteNumber(data.elapsedMs) || !isNullableString(data.error)) return null;
  return data as unknown as NonNullable<EngineSnapshot["recording"]>;
}

export function isEngineSnapshot(value: unknown): value is EngineSnapshot {
  if (!isRecord(value) || !isRecord(value.engine) || !isRecord(value.decks)) return false;
  const decks = value.decks;
  if (
    !Number.isSafeInteger(value.rev) ||
    !Number.isSafeInteger(value.seq) ||
    typeof value.engineId !== "string" ||
    !isNullableString(value.sessionId) ||
    !isFiniteNumber(value.engineTimeMs) ||
    typeof value.engine.name !== "string" ||
    typeof value.engine.version !== "string" ||
    typeof value.engine.implementation !== "string" ||
    typeof value.engine.simulated !== "boolean" ||
    typeof value.engine.deterministic !== "boolean" ||
    !(value.engine.audioAvailable === undefined || typeof value.engine.audioAvailable === "boolean") ||
    !Array.isArray(value.engine.decks) ||
    !Array.isArray(value.engine.capabilities)
  ) return false;
  if (!value.engine.decks.every(isDeckId)) return false;
  const engineDecks = value.engine.decks as DeckId[];
  const mixer = parseMixerState(value.mixer);
  return (
    engineDecks.every((deck) => parseDeckState(decks[deck]) !== null) &&
    mixer !== null &&
    engineDecks.every((deck) => mixer.channels[deck] !== undefined) &&
    parseAudioConfig(value.audio) !== null &&
    (value.recording === undefined || parseRecording(value.recording) !== null) &&
    isRecord(value.meters) &&
    typeof value.meters.enabled === "boolean" &&
    isFiniteNumber(value.meters.intervalMs) &&
    typeof value.meters.simulated === "boolean"
  );
}

interface DeckPositionUpdate {
  positionMs: number;
  positionFrames: number;
  rate: number;
  status: DeckStatus;
  scratching?: boolean;
  effectiveBpm?: number | null;
}

function parseDeckPositions(
  data: unknown
): Partial<Record<DeckId, DeckPositionUpdate>> | null {
  if (!isRecord(data)) return null;
  const decks = data.decks;
  if (!isRecord(decks)) return null;

  const parsed: Partial<Record<DeckId, DeckPositionUpdate>> = {};
  for (const deckId of DECK_IDS) {
    const entry = decks[deckId];
    if (!isRecord(entry)) continue;
    const positionMs = readNumber(entry, "positionMs");
    const positionFrames = readNumber(entry, "positionFrames");
    const rate = readNumber(entry, "rate");
    if (positionMs === null || positionFrames === null || rate === null) continue;
    if (!isDeckStatus(entry.status)) continue;
    if (!(entry.scratching === undefined || typeof entry.scratching === "boolean")) continue;
    if (!(entry.effectiveBpm === undefined || entry.effectiveBpm === null || isFiniteNumber(entry.effectiveBpm))) continue;
    parsed[deckId] = {
      positionMs, positionFrames, rate, status: entry.status,
      ...(typeof entry.scratching === "boolean" ? { scratching: entry.scratching } : {}),
      ...(entry.effectiveBpm === null || isFiniteNumber(entry.effectiveBpm) ? { effectiveBpm: entry.effectiveBpm } : {}),
    };
  }
  return parsed;
}

// ------------------------------------------------------------------ 状態の畳み込み

export function createInitialClientState(): EngineClientState {
  return {
    snapshot: null,
    rev: 0,
    lastSeq: 0,
    droppedEvents: 0,
    meters: null,
    sessionInvalidated: false,
  };
}

/** 取得したスナップショットを正本として置き換える。 */
export function applySnapshot(
  state: EngineClientState,
  snapshot: EngineSnapshot
): EngineClientState {
  // `seq` is the event watermark included in this authoritative snapshot.
  const sameEngine = state.snapshot?.engineId === snapshot.engineId;
  return {
    snapshot,
    rev: snapshot.rev,
    lastSeq: snapshot.seq,
    droppedEvents: 0,
    meters: sameEngine ? state.meters : null,
    sessionInvalidated: false,
  };
}

/** Apply a snapshot watermark, then replay only events generated afterward. */
export function applySnapshotWithEvents(
  state: EngineClientState,
  snapshot: EngineSnapshot,
  events: readonly EngineEventMessage[]
): EngineClientState {
  let next = applySnapshot(state, snapshot);
  for (const event of events) {
    if (event.engineId === snapshot.engineId && event.seq > snapshot.seq) {
      next = reduceEngineEvent(next, event);
    }
  }
  return next;
}

/**
 * イベントを 1 件適用する。
 *
 * 規則:
 * - 別エンジンインスタンスのイベントは捨てる。
 * - `seq` が戻る／重複するイベントは適用しない。
 * - `seq` が飛んだら `droppedEvents` を積む（呼び出し側がスナップショットを取り直す合図）。
 * - `rev` がクライアント側より古いイベントは状態を上書きしない。
 */
export function reduceEngineEvent(
  state: EngineClientState,
  event: EngineEventMessage
): EngineClientState {
  if (state.snapshot && event.engineId !== state.snapshot.engineId) {
    return state;
  }
  if (event.seq <= state.lastSeq) {
    return state;
  }

  const dropped =
    state.lastSeq > 0 && event.seq > state.lastSeq + 1
      ? state.droppedEvents + (event.seq - state.lastSeq - 1)
      : state.droppedEvents;

  const next: EngineClientState = {
    ...state,
    lastSeq: event.seq,
    droppedEvents: dropped,
  };

  if (event.event === DJ_ENGINE_EVENTS.sessionInvalidated) {
    const invalidatedSession = isRecord(event.data)
      ? event.data.sessionId
      : undefined;
    const activeSession = state.snapshot?.sessionId;
    // The hello reply and this event use separate delivery paths. A delayed
    // invalidation for s1 must never invalidate an already-applied s2 snapshot.
    if (
      typeof invalidatedSession === "string" &&
      typeof activeSession === "string" &&
      invalidatedSession !== activeSession
    ) {
      return next;
    }
    return { ...next, sessionInvalidated: true };
  }

  const snapshot = next.snapshot;
  if (!snapshot) {
    // スナップショット未取得。イベントだけでは状態を組み立てない。
    return next;
  }
  if (event.rev < next.rev) {
    return next;
  }

  switch (event.event) {
    case DJ_ENGINE_EVENTS.deckState: {
      const deckState = parseDeckState(event.data);
      if (!deckState) return next;
      const decks = { ...snapshot.decks };
      decks[deckState.deck] = deckState;
      return {
        ...next,
        rev: event.rev,
        snapshot: {
          ...snapshot,
          rev: event.rev,
          engineTimeMs: event.engineTimeMs,
          decks,
        },
      };
    }
    case DJ_ENGINE_EVENTS.deckPosition: {
      const positions = parseDeckPositions(event.data);
      if (!positions) return next;
      const decks = { ...snapshot.decks };
      for (const deckId of DECK_IDS) {
        const update = positions[deckId];
        if (!update) continue;
        decks[deckId] = { ...decks[deckId], ...update };
      }
      return {
        ...next,
        rev: event.rev,
        snapshot: { ...snapshot, rev: event.rev, engineTimeMs: event.engineTimeMs, decks },
      };
    }
    case DJ_ENGINE_EVENTS.mixerState: {
      const mixer = parseMixerState(event.data);
      if (!mixer) return next;
      return {
        ...next,
        rev: event.rev,
        snapshot: {
          ...snapshot,
          rev: event.rev,
          mixer,
        },
      };
    }
    case DJ_ENGINE_EVENTS.audioConfig: {
      const audio = parseAudioConfig(event.data);
      if (!audio) return next;
      return {
        ...next,
        rev: event.rev,
        snapshot: {
          ...snapshot,
          rev: event.rev,
          audio,
        },
      };
    }
    case DJ_ENGINE_EVENTS.recordingState: {
      const recording = parseRecording(event.data);
      if (!recording) return next;
      return { ...next, rev: event.rev, snapshot: { ...snapshot, rev: event.rev, recording } };
    }
    case DJ_ENGINE_EVENTS.meters: {
      const meters = parseMeters(event.data);
      if (!meters) return next;
      return { ...next, meters };
    }
    default:
      // 未知のイベントは seq だけ進めて無視する（前方互換）。
      return next;
  }
}

// ------------------------------------------------------------------ 表示

export function describeEngineError(error: EngineError): string {
  switch (error.code) {
    case "session_mismatch":
    case "session_required":
      return "エンジンとの接続が切れています。再接続してください。";
    case "engine_mismatch":
      return "別のエンジンインスタンス宛の操作です。再接続してください。";
    case "engine_exited":
      return "エンジンプロセスが終了しました。";
    case "no_track_loaded":
      return "デッキに曲がロードされていません。";
    case "track_not_ready":
      return "ロード中です。完了までお待ちください。";
    case "unsupported_operation":
      return `この実装では未対応の操作です: ${error.message}`;
    case "unknown_op":
      return `エンジンが知らない操作です: ${error.message}`;
    default:
      return error.message;
  }
}
