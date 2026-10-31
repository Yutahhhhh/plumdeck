import { useCallback, useEffect, useState } from "react";
import { getErrorDetail } from "@/services/api-client";
import { assistService, isDesktopShell } from "@/services/assist";

/** Wide enough for a title plus its reasons, short enough to sit beside rekordbox. */
export const COMPACT_WIDTH = 440;
export const COMPACT_HEIGHT = 650;

// Serialize window resizes and pinning across every caller (Assist, Play) and
// across React development remounts.
let windowOperation: Promise<unknown> = Promise.resolve();
export function enqueueWindowOperation<T>(operation: () => Promise<T>): Promise<T> {
  const next = windowOperation.then(operation, operation);
  windowOperation = next.catch(() => undefined);
  return next;
}

export interface AlwaysOnTop {
  alwaysOnTop: boolean;
  setAlwaysOnTop: (enabled: boolean) => void;
  /** Non-null when the window could not be pinned. */
  error: string | null;
  supported: boolean;
}

/**
 * Pin toggle for the main window. While `active` is false the pin is off; turning
 * `active` off (or unmounting) unpins, so a view never leaves the window pinned
 * over other apps after it is gone.
 */
export function useAlwaysOnTop(active: boolean): AlwaysOnTop {
  const [alwaysOnTop, setPinned] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const supported = isDesktopShell();

  useEffect(() => {
    if (!active || !supported) return;
    return () => {
      setPinned(false);
      void enqueueWindowOperation(() => assistService.setAlwaysOnTop(false)).catch(() => undefined);
    };
  }, [active, supported]);

  const setAlwaysOnTop = useCallback(
    (enabled: boolean) => {
      if (!supported || !active) return;
      setPinned(enabled);
      setError(null);
      void enqueueWindowOperation(() => assistService.setAlwaysOnTop(enabled)).catch((failure) => {
        setPinned(!enabled);
        setError(getErrorDetail(failure));
      });
    },
    [active, supported],
  );

  return { alwaysOnTop, setAlwaysOnTop, error, supported };
}

export interface CompactWindow {
  alwaysOnTop: boolean;
  setAlwaysOnTop: (enabled: boolean) => void;
  /** Non-null when the window could not be resized or pinned. */
  message: string | null;
  supported: boolean;
}

/**
 * Shrinks the main window while assist mode is active and puts back exactly the
 * bounds it had on the way out. The previous geometry is remembered natively, so
 * a webview reload in between cannot lose it.
 */
export function useCompactWindow(active: boolean): CompactWindow {
  // Declared first so its cleanup (unpin) is queued before the exit resize.
  const pin = useAlwaysOnTop(active);
  const [message, setMessage] = useState<string | null>(null);
  const supported = pin.supported;

  useEffect(() => {
    if (!active || !supported) return;
    let live = true;
    void enqueueWindowOperation(() => assistService.enterCompactWindow(COMPACT_WIDTH, COMPACT_HEIGHT))
      .then((result) => {
        if (live && result.message) setMessage(result.message);
      })
      .catch((failure) => {
        if (live) setMessage(getErrorDetail(failure));
      });
    return () => {
      live = false;
      void enqueueWindowOperation(() => assistService.exitCompactWindow()).catch(() => undefined);
    };
  }, [active, supported]);

  return { alwaysOnTop: pin.alwaysOnTop, setAlwaysOnTop: pin.setAlwaysOnTop, message: message ?? pin.error, supported };
}
