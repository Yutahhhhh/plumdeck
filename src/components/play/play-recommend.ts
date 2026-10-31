import type { AssistCandidate, AssistHistoryEvidence, AssistIntent, GenreScope } from "@/services/assist";

/** First page and each 「さらに表示」 step; the engine answers at most 50. */
export const RECOMMEND_PAGE = 15;
export const RECOMMEND_MAX = 50;

export const nextRecommendLimit = (limit: number) => Math.min(RECOMMEND_MAX, limit + RECOMMEND_PAGE);

/** Tracks already played this session never come back as the next pick (the source is excluded by the engine). */
export function recommendExcludeIds(played: ReadonlySet<number> | undefined, enabled: boolean, seed: number | null): number[] {
  if (!enabled || !played) return [];
  return [...played].filter(id => id !== seed).sort((a, b) => a - b);
}

/** 0..1 preset fit as a whole percent, or null when the engine gave none. */
export const scorePercent = (score: number | null | undefined) =>
  typeof score === "number" && Number.isFinite(score) ? Math.round(Math.max(0, Math.min(1, score)) * 100) : null;

/** A short badge for the owner's own history after the source track, strongest evidence first. */
export function historyBadge(history: AssistHistoryEvidence | null | undefined): string | null {
  if (!history) return null;
  if (history.followed > 0) return `次に${history.followed}回`;
  if (history.nearby > 0) return `近くで${history.nearby}回`;
  return null;
}

/** The cautions the row surfaces without opening its details. */
export const cautionsOf = (track: Pick<AssistCandidate, "reasons">) => track.reasons.filter(reason => reason.tone === "caution");

export interface RecommendSettings {
  intent: AssistIntent;
  transition: boolean;
  genreScope: GenreScope;
  excludePlayed: boolean;
}

export const DEFAULT_RECOMMEND_SETTINGS: RecommendSettings = { intent: "keep", transition: false, genreScope: "any", excludePlayed: true };

const INTENTS: AssistIntent[] = ["keep", "hype", "dance", "calm", "emotional", "bright", "throwback", "wordplay"];
const SCOPES: GenreScope[] = ["any", "same_genre", "same_subgenre"];

/** Reads remembered panel settings, falling back field by field. */
export function parseRecommendSettings(raw: string | null): RecommendSettings {
  let value: Partial<Record<keyof RecommendSettings, unknown>> = {};
  try { value = raw ? JSON.parse(raw) ?? {} : {}; } catch { value = {}; }
  if (typeof value !== "object" || Array.isArray(value)) value = {};
  return {
    intent: INTENTS.includes(value.intent as AssistIntent) ? value.intent as AssistIntent : DEFAULT_RECOMMEND_SETTINGS.intent,
    transition: typeof value.transition === "boolean" ? value.transition : DEFAULT_RECOMMEND_SETTINGS.transition,
    genreScope: SCOPES.includes(value.genreScope as GenreScope) ? value.genreScope as GenreScope : DEFAULT_RECOMMEND_SETTINGS.genreScope,
    excludePlayed: typeof value.excludePlayed === "boolean" ? value.excludePlayed : DEFAULT_RECOMMEND_SETTINGS.excludePlayed,
  };
}
