/**
 * One deck load, step by step, with a bound on every step.
 *
 * The Play load used to await each step without a limit inside the deck's
 * load queue, so one stalled step (engine start, audio, the metadata GET, the
 * engine's own load, or the history write after it) left the deck silently
 * unchanged and queued every later load for that deck behind it. Here every
 * step has a deadline and a message that names it, the metadata is optional
 * (the grid and cues follow later when it arrives late), and nothing after the
 * engine load is awaited by the caller's queue.
 */

export type DeckLoadStage = "engine" | "audio" | "metadata" | "load" | "settle";

export class DeckLoadError extends Error {
  readonly stage: DeckLoadStage;
  constructor(stage: DeckLoadStage, message: string) {
    super(message);
    this.stage = stage;
    this.name = "DeckLoadError";
  }
}

export interface DeckLoadPoll { status: string; trackId?: string | null; lastError?: string | null }

export interface DeckLoadSteps<M> {
  trackId: string;
  /** The load is still the one the user asked for (not replaced, same engine session). */
  current: () => boolean;
  /** Start / connect the engine when needed. */
  ensureEngine: () => Promise<void>;
  ensureAudio: () => Promise<void>;
  fetchMetadata: () => Promise<M>;
  /** Apply the metadata (or `null` when it is late or failed) to the descriptor. */
  prepare: (metadata: M | null) => void;
  load: () => Promise<unknown>;
  poll: () => Promise<DeckLoadPoll>;
  /** Called once the engine reports the track, never awaited by the queue. */
  onStage?: (stage: DeckLoadStage) => void;
  engineTimeoutMs?: number;
  audioTimeoutMs?: number;
  metadataTimeoutMs?: number;
  loadTimeoutMs?: number;
  settleTimeoutMs?: number;
  pollIntervalMs?: number;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
}

export type DeckLoadResult<M> =
  | { kind: "superseded" }
  | { kind: "loaded"; metadata: M | null; metadataError: string | null };

const MESSAGES: Record<DeckLoadStage, string> = {
  engine: "音声エンジンの起動が完了しないため、ロードを中止しました。エンジンを再起動して再試行してください。",
  audio: "音声出力の準備が完了しないため、ロードを中止しました。オーディオ設定を確認してください。",
  metadata: "グリッドとキューの読み込みが遅れています。曲はグリッドなしで読み込み、届きしだい反映します。",
  load: "音声エンジンがロード要求に応答しません。再試行してください。",
  settle: "曲のロードがタイムアウトしました。再試行してください。",
};

export function deckLoadMessage(stage: DeckLoadStage) { return MESSAGES[stage]; }

function bounded<T>(task: Promise<T>, ms: number, stage: DeckLoadStage): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const limit = new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new DeckLoadError(stage, MESSAGES[stage])), ms); });
  return Promise.race([task, limit]).finally(() => clearTimeout(timer));
}

export async function runDeckLoad<M>(steps: DeckLoadSteps<M>): Promise<DeckLoadResult<M>> {
  const sleep = steps.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const now = steps.now ?? (() => performance.now());
  const stage = (name: DeckLoadStage) => steps.onStage?.(name);
  if (!steps.current()) return { kind: "superseded" };
  stage("engine");
  await bounded(steps.ensureEngine(), steps.engineTimeoutMs ?? 30_000, "engine");
  if (!steps.current()) return { kind: "superseded" };
  stage("audio");
  await bounded(steps.ensureAudio(), steps.audioTimeoutMs ?? 20_000, "audio");
  if (!steps.current()) return { kind: "superseded" };
  stage("metadata");
  let metadata: M | null = null, metadataError: string | null = null;
  try {
    metadata = await bounded(steps.fetchMetadata(), steps.metadataTimeoutMs ?? 8_000, "metadata");
  } catch (cause) {
    metadataError = cause instanceof DeckLoadError ? cause.message : `グリッドとキューを読み込めませんでした（${cause instanceof Error ? cause.message : String(cause)}）。曲はグリッドなしで読み込みます。`;
  }
  if (!steps.current()) return { kind: "superseded" };
  steps.prepare(metadata);
  stage("load");
  await bounded(steps.load(), steps.loadTimeoutMs ?? 15_000, "load");
  stage("settle");
  const deadline = now() + (steps.settleTimeoutMs ?? 15_000);
  for (;;) {
    if (!steps.current()) return { kind: "superseded" };
    const state = await bounded(steps.poll(), steps.loadTimeoutMs ?? 15_000, "settle");
    if (state.status === "error") throw new DeckLoadError("settle", state.lastError || "曲のロードに失敗しました");
    if (state.trackId === steps.trackId && state.status !== "loading") return { kind: "loaded", metadata, metadataError };
    if (now() >= deadline) throw new DeckLoadError("settle", MESSAGES.settle);
    await sleep(steps.pollIntervalMs ?? 50);
  }
}
