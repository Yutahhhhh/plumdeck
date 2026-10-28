import type { StartupProgress } from "@/hooks/useStartupProgress";

/** After this many seconds the startup screen reassures the user and offers details. */
export const STARTUP_SLOW_SECONDS = 15;

/**
 * Shown under the status line once startup is slow. It names the usual causes
 * (first launch, right after an update: unpacking the analysis service and
 * loading its engines) without blaming the size of the music library.
 */
export const STARTUP_SLOW_HINT = "初回起動やアップデートの直後は、いつもより長くかかることがあります。";

// Friendly copy for the stage ids the backend reports (backend://startup).
// Unknown stages fall back to the backend's own label so new milestones still read well.
// "services" loads the analysis and playback code (backend/server.py), not the
// user's music library, so its copy must not read as a library scan.
const STAGE_MESSAGES = new Map<string, string>([
  ["launch", "plumdeck を起動しています…"],
  ["settings", "設定を読み込んでいます…"],
  ["services", "解析・再生エンジンを準備しています…"],
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

// Break points for Japanese copy: after 、。・：）」 and before （「. Whitespace
// comes back as its own item so it stays a real (breakable) space between phrases.
const PHRASE_BREAK = /(?<=[、。・：）」])(?=[^、。・：）」\s])|(?=[（「])|(\s+)/;

/**
 * Splits copy into phrases so it can wrap between them instead of mid-word
 * (e.g. "読み込 / んでいます"). Whitespace items are returned as " ".
 */
export function startupPhrases(text: string): string[] {
  return text
    .split(PHRASE_BREAK)
    .filter((part): part is string => Boolean(part))
    .map(part => (/^\s+$/.test(part) ? " " : part));
}

/** Plain-text summary of the startup state for the 「詳細をコピー」 button (for support requests). */
export function startupDetailsText({ version, progress, elapsedSeconds, connectionError }: {
  version: string;
  progress: StartupProgress;
  elapsedSeconds: number;
  connectionError?: string;
}): string {
  const error = progress.error?.trim();
  const lines = [`plumdeck v${version}`];
  if (error) lines.push(`エラー: ${error}`);
  if (progress.label) lines.push(`処理: ${progress.label}`);
  lines.push(`状態: ${progress.stage || "—"} (${progress.completed}/${progress.total})`);
  lines.push(`経過時間: ${formatElapsed(elapsedSeconds)}`);
  if (connectionError?.trim()) lines.push(`接続: ${connectionError.trim()}`);
  return lines.join("\n");
}
