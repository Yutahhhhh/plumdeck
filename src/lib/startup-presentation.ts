import type { StartupProgress } from "@/hooks/useStartupProgress";

/** After this many seconds the startup screen reassures the user and offers details. */
export const STARTUP_SLOW_SECONDS = 15;

// Friendly copy for the stage ids the backend reports (backend://startup).
// Unknown stages fall back to the backend's own label so new milestones still read well.
const STAGE_MESSAGES = new Map<string, string>([
  ["launch", "plumdeck を起動しています…"],
  ["settings", "設定を読み込んでいます…"],
  ["services", "ライブラリを準備しています…"],
  ["database", "楽曲データを読み込んでいます…"],
  ["queue", "前回の作業を復元しています…"],
  ["ready", "まもなく準備が整います…"],
]);

export function startupMessage(progress: Pick<StartupProgress, "stage" | "label">): string {
  const known = STAGE_MESSAGES.get(progress.stage);
  if (known) return known;
  const label = (progress.label ?? "").trim().replace(/[。.…]+$/, "");
  return label ? `${label}…` : "準備しています…";
}

export function startupRatio(completed: number, total: number): number {
  if (!(total > 0) || !Number.isFinite(completed)) return 0;
  return Math.min(1, Math.max(0, completed / total));
}

/**
 * Next value for the visible progress bar. It never moves backwards, snaps up
 * to each reported milestone, and (when `trickle` is on) eases toward — but
 * never reaches — the next milestone so a long stage still feels alive.
 */
export function advanceStartupBar(shown: number, completed: number, total: number, trickle: boolean): number {
  const floor = startupRatio(completed, total);
  const next = Math.max(Number.isFinite(shown) ? shown : 0, floor);
  if (!trickle || floor >= 1) return next;
  const ceiling = startupRatio(Math.floor(completed) + 0.8, total);
  if (next >= ceiling) return next;
  return next + (ceiling - next) * 0.08;
}

export function formatElapsed(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  if (whole < 60) return `${whole}秒`;
  return `${Math.floor(whole / 60)}分${String(whole % 60).padStart(2, "0")}秒`;
}
