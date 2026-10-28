/**
 * Waiting for a beat grid the backend analyzes after the deck load.
 *
 * A metadata GET no longer runs whole-track grid analysis inline: it answers
 * at once with `grid_pending` and analyzes in the background. The deck loads
 * without a grid and takes the grid when a later GET returns it.
 */
export interface PendingGridMetadata {
  beat_grid: unknown | null;
  grid_pending?: boolean;
}

export interface PendingGridOptions<T extends PendingGridMetadata> {
  /** GET the metadata again. */
  fetch: () => Promise<T>;
  /** Whether the deck still holds the load that is waiting. */
  current: () => boolean;
  intervalMs?: number;
  timeoutMs?: number;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
}

export const PENDING_GRID_INTERVAL_MS = 2000;
// Longer than the backend's own analysis limit, so a slow file still lands.
export const PENDING_GRID_TIMEOUT_MS = 180_000;

/**
 * Poll until the analysis settles. Resolves with the metadata that carries
 * the grid, or that ended without one (its warning says why); null when the
 * load was replaced or the wait timed out.
 */
export async function awaitPendingGrid<T extends PendingGridMetadata>(
  options: PendingGridOptions<T>,
): Promise<T | null> {
  const sleep = options.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const now = options.now ?? (() => Date.now());
  const deadline = now() + (options.timeoutMs ?? PENDING_GRID_TIMEOUT_MS);
  while (now() < deadline) {
    await sleep(options.intervalMs ?? PENDING_GRID_INTERVAL_MS);
    if (!options.current()) return null;
    let metadata: T;
    try {
      metadata = await options.fetch();
    } catch {
      continue; // a transient backend error; keep waiting until the deadline
    }
    if (!options.current()) return null;
    if (metadata.beat_grid || !metadata.grid_pending) return metadata;
  }
  return null;
}
