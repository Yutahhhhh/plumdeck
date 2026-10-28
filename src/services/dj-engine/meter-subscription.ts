/**
 * Shared `meters.subscribe` for one engine client. Each consumer (mixer,
 * master meter, controller LEDs …) acquires it; the stream is turned off only
 * when the last one releases, never by one consumer for everyone.
 */
type MeterClient = { subscribeMeters(enabled: boolean, intervalMs?: number): Promise<unknown> };
const counts = new WeakMap<MeterClient, number>();

export function acquireMeters(client: MeterClient, intervalMs = 33): () => void {
  const count = (counts.get(client) ?? 0) + 1;
  counts.set(client, count);
  // Re-sent on every acquire: a new engine session starts unsubscribed.
  void client.subscribeMeters(true, intervalMs).catch(() => undefined);
  let released = false;
  return () => {
    if (released) return;
    released = true;
    const left = Math.max(0, (counts.get(client) ?? 1) - 1);
    counts.set(client, left);
    if (!left) void client.subscribeMeters(false).catch(() => undefined);
  };
}
