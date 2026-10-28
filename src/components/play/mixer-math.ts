/**
 * Mixer value conversions: linear gain ⇄ dB text and channel fader curves.
 * Pure module: runs under `node --test`.
 */

export function gainToDb(gain: number): number {
  return gain > 0 && Number.isFinite(gain) ? 20 * Math.log10(gain) : Number.NEGATIVE_INFINITY;
}

/** "−6.0 dB", "+12.0 dB", "0.0 dB", "−∞". */
export function formatDb(gain: number): string {
  const db = gainToDb(gain);
  if (!Number.isFinite(db) || db <= -60) return "−∞";
  const text = Math.abs(db).toFixed(1);
  return `${Number(text) === 0 ? "" : db > 0 ? "+" : "−"}${text} dB`;
}

/** EQ at or below this linear gain counts as killed (≈ −40 dB). */
export const EQ_KILL_GAIN = 0.01;
export const isKilled = (gain: number | undefined) => gain !== undefined && gain <= EQ_KILL_GAIN;

/**
 * Channel fader curves. The engine fader is linear gain 0..1; the curve maps
 * the visual position onto it.
 *  - slow: stays low and rises late (gain = p²)
 *  - linear: gain = p (the engine's own response)
 *  - fast: rises early and holds near unity (gain = 1 − (1 − p)²)
 */
export const FADER_CURVES = ["slow", "linear", "fast"] as const;
export type FaderCurve = (typeof FADER_CURVES)[number];
export const FADER_CURVE_STORAGE = "plumdeck.channelFaderCurve";
export const FADER_CURVE_LABELS: Record<FaderCurve, string> = { slow: "ゆるやか", linear: "リニア", fast: "急峻" };

const unit = (value: number) => Math.max(0, Math.min(1, Number.isFinite(value) ? value : 0));

export function faderGain(position: number, curve: FaderCurve): number {
  const p = unit(position);
  return curve === "slow" ? p * p : curve === "fast" ? 1 - (1 - p) * (1 - p) : p;
}

export function faderPosition(gain: number, curve: FaderCurve): number {
  const g = unit(gain);
  return curve === "slow" ? Math.sqrt(g) : curve === "fast" ? 1 - Math.sqrt(1 - g) : g;
}

export function readFaderCurve(value: string | null | undefined): FaderCurve {
  return FADER_CURVES.includes(value as FaderCurve) ? value as FaderCurve : "linear";
}

/** Crossfader assign as the engine's `orientation`: 0 = A (left), 1 = THRU, 2 = B (right). */
export const CROSSFADER_ASSIGN = [
  { orientation: 0, label: "A", title: "クロスフェーダーの A 側に割り当て" },
  { orientation: 1, label: "THRU", title: "クロスフェーダーを通さない" },
  { orientation: 2, label: "B", title: "クロスフェーダーの B 側に割り当て" },
] as const;

/** Color FX types the current host implements, with the host effect name. */
export const COLOR_FX = [
  { effect: "filter", label: "FILTER" },
  { effect: "echo", label: "DUB ECHO" },
  { effect: "pitchshift", label: "PITCH" },
  { effect: "whitenoise", label: "NOISE" },
] as const;
export type ColorFxEffect = (typeof COLOR_FX)[number]["effect"];
export const COLOR_FX_STORAGE = "plumdeck.colorFx";
export function readColorFx(value: string | null | undefined): ColorFxEffect {
  return COLOR_FX.some((entry) => entry.effect === value) ? value as ColorFxEffect : "filter";
}

/** Knob readout for a Color FX amount (−1..1, centre bypass). */
export function formatColorAmount(effect: ColorFxEffect, amount: number): string {
  if (Math.abs(amount) < 0.005) return "OFF";
  const percent = `${Math.round(Math.abs(amount) * 100)}%`;
  if (effect === "filter") return `${amount > 0 ? "HPF" : "LPF"} ${percent}`;
  return `${amount > 0 ? "+" : "−"}${percent}`;
}

/** Crossfader curves the host maps onto its mixer profile (H-12). */
// Gains follow Mixxx's getXfadeGains: smooth is constant power (both −3 dB at
// the centre); standard and sharp keep both sides at full level at the centre.
export const CROSSFADER_CURVE_OPTIONS = [
  { curve: "smooth", label: "なめらか", title: "ロングミックス向け：中央で両方が約 −3 dB になり、音量感を保ったまま入れ替わります" },
  { curve: "standard", label: "標準", title: "中央では両方とも最大で、片側へ動かすと反対側がなだらかに下がります" },
  { curve: "sharp", label: "カット", title: "スクラッチ向け（初期設定）：端のわずかな範囲だけで切り替わり、それ以外は両方とも最大です" },
] as const;
export type CrossfaderCurveOption = (typeof CROSSFADER_CURVE_OPTIONS)[number]["curve"];
export const CROSSFADER_CURVE_STORAGE = "plumdeck.crossfaderCurve";
export function readCrossfaderCurve(value: string | null | undefined): CrossfaderCurveOption {
  // "sharp" is what hosts before H-12 always played (Mixxx's unconfigured profile).
  return CROSSFADER_CURVE_OPTIONS.some((entry) => entry.curve === value) ? value as CrossfaderCurveOption : "sharp";
}

/** Audio dropouts: xruns counted since the first reading of this session. */
export type DropoutState = { session: string; baseline: number; count: number; lastAt: number | null };
export function nextDropouts(previous: DropoutState | null, session: string, xruns: number, now: number): DropoutState {
  if (!previous || previous.session !== session || xruns < previous.baseline) return { session, baseline: xruns, count: 0, lastAt: null };
  const count = xruns - previous.baseline;
  return { session, baseline: previous.baseline, count, lastAt: count > previous.count ? now : previous.lastAt };
}
