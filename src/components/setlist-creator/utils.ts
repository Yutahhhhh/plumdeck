import { FilterState } from "@/components/music-library/types";
import { KEYS } from "@/lib/keys.generated";
import { parseKey } from "@/services/dj-engine/musical-key";
import { Track } from "@/types";

// --- Camelot Wheel (Harmonic Mixing) ---
// Whole-key parsing: matching a table name inside the text turned
// "Ab minor" into B minor (10A).

function normalizeKeyToCamelot(key?: string | null): string | null {
  const parsed = parseKey(key);
  return parsed ? KEYS[parsed.pitch + (parsed.minor ? 12 : 0)].alphanumeric : null;
}

export type KeyCompatibility = "perfect" | "compatible" | "clash" | "unknown";

/** 前曲とのキー相性を Camelot Wheel の隣接関係で判定する */
export function getKeyCompatibility(
  fromKey?: string | null,
  toKey?: string | null
): KeyCompatibility {
  const a = normalizeKeyToCamelot(fromKey);
  const b = normalizeKeyToCamelot(toKey);
  if (!a || !b) return "unknown";
  if (a === b) return "perfect";

  const num = (c: string) => parseInt(c.slice(0, -1), 10);
  const letter = (c: string) => c.slice(-1);

  // 同番号の A/B 切替、または ±1 (12↔1 のラップあり) で同レターは相性良
  if (num(a) === num(b)) return "compatible";
  const diff = Math.abs(num(a) - num(b));
  if ((diff === 1 || diff === 11) && letter(a) === letter(b)) return "compatible";
  return "clash";
}

/** 前曲との遷移情報 (BPM 差・キー相性) を返す。生成結果リストの根拠表示用。 */
export function getTransitionInfo(
  prev: Track | null | undefined,
  next: Track
): { bpmDiff: number | null; keyCompat: KeyCompatibility } {
  const bpmDiff =
    prev?.bpm && next.bpm && prev.bpm > 0 && next.bpm > 0
      ? Math.round((next.bpm - prev.bpm) * 10) / 10
      : null;
  return {
    bpmDiff,
    keyCompat: getKeyCompatibility(prev?.key, next.key),
  };
}

export function buildTrackSearchParams(query: string, filters: FilterState) {
  const params: any = { title: query, limit: 50 };

  // Basic Filters Mapping
  if (filters.bpm && filters.bpm > 0) {
    params["bpm"] = filters.bpm.toString();
    params["bpm_range"] = filters.bpmRange.toString();
  }
  if (filters.key) params["key"] = filters.key;
  if (filters.artist) params["artist"] = filters.artist;
  if (filters.genres && filters.genres.length > 0) {
    params["genres"] = filters.genres;
  }

  // Advanced Filters
  if (filters.minEnergy > 0)
    params["min_energy"] = filters.minEnergy.toString();
  if (filters.maxEnergy < 1)
    params["max_energy"] = filters.maxEnergy.toString();
  if (filters.minDanceability > 0)
    params["min_danceability"] = filters.minDanceability.toString();
  if (filters.maxDanceability < 1)
    params["max_danceability"] = filters.maxDanceability.toString();
  if (filters.minBrightness > 0)
    params["min_brightness"] = filters.minBrightness.toString();
  if (filters.maxBrightness < 1)
    params["max_brightness"] = filters.maxBrightness.toString();

  return params;
}
