import { useCallback, useEffect, useRef, useState, type MutableRefObject } from "react";
import { DeckResolutionCache } from "./deck-resolution-cache";
import { getErrorDetail } from "@/services/api-client";
import { assistService, type AssistSnapshot, type DeckResolution } from "@/services/assist";

/** Sequential reads, including manual refresh; results begun before a drag are discarded. */
export function useRekordboxDecks(active: boolean, paused: boolean, freezeRef?: MutableRefObject<boolean>) {
  const [snapshot, setSnapshot] = useState<AssistSnapshot | null>(null);
  const [resolution, setResolution] = useState<DeckResolution | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const pausedRef = useRef(paused);
  pausedRef.current = paused;
  const revision = useRef(0);
  const refresh = useCallback(() => { revision.current += 1; }, []);
  useEffect(() => {
    if (!active) return;
    let live = true;
    let timer: number;
    let fingerprint = "";
    let resolvedAt = 0;
    let lastRevision = -1;
    let retry = true;
    const cache = new DeckResolutionCache<DeckResolution>();
    const frozen = () => pausedRef.current || freezeRef?.current;
    const tick = async () => {
      const version = revision.current;
      try {
        if (frozen()) return;
        const next = await assistService.snapshot();
        if (!live || frozen() || version !== revision.current) return;
        setSnapshot(next);
        if (!next.supported || !next.permission_granted || !next.app_running || next.unavailable_reason || !next.decks.length) {
          cache.clear();
          setResolution(null);
          fingerprint = "";
          setError(null);
          return;
        }
        if (Date.now() - next.captured_at_ms > 10_000) {
          cache.clear();
          setResolution(null);
          setError("デッキ情報が古くなっています。再読み込みしています…");
          fingerprint = "";
          return;
        }
        const identity = cache.observe(next.decks);
        setResolution(cache.value);
        const key = JSON.stringify([identity, [...new Set(next.open_audio_paths)].sort()]);
        if (key === fingerprint && version === lastRevision && Date.now() - resolvedAt < (retry ? 5000 : 30000)) return;
        // The playback readout changes every tick and the backend rejects unknown fields.
        const result = await assistService.resolveDecks(next.decks.map(({ elapsed_seconds: _elapsed, ...deck }) => deck), next.open_audio_paths);
        if (!live || frozen() || version !== revision.current) return;
        if (Date.now() - next.captured_at_ms > 10_000) {
          cache.clear();
          setResolution(null);
          setError("曲の照合に時間がかかっています。再確認しています…");
          fingerprint = "";
          return;
        }
        fingerprint = key;
        lastRevision = version;
        resolvedAt = Date.now();
        retry = !!result.library_error || result.decks.some(deck => !["resolved", "empty"].includes(deck.status));
        cache.publish(result);
        setResolution(result);
        setError(null);
      } catch (failure) {
        if (live && !frozen() && version === revision.current) {
          cache.clear();
          setResolution(null);
          setSnapshot(null);
          fingerprint = "";
          setError(getErrorDetail(failure));
        }
      } finally {
        if (live) {
          if (!frozen()) setLoading(false);
          timer = window.setTimeout(tick, 1000);
        }
      }
    };
    void tick();
    return () => { live = false; window.clearTimeout(timer); };
  }, [active, freezeRef]);
  return { snapshot, resolution, decks: resolution?.decks ?? [], error, loading, refresh };
}
