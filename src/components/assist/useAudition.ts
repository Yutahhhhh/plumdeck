import { useCallback, useMemo, useRef, useState } from "react";
import { useAudioPreview } from "@/hooks/useAudioPreview";
import { AUDITION_SECONDS, auditionStart, type AuditionControl, type AuditionTrack } from "./audition";

/** One suggestion at a time, streamed from the backend and played by the webview. */
export function useAudition() {
  const [playingId, setPlayingId] = useState<number | null>(null);
  const [failedId, setFailedId] = useState<number | null>(null);
  const current = useRef<number | null>(null);
  const end = useCallback(() => { current.current = null; setPlayingId(null); }, []);
  const fail = useCallback(() => setFailedId(current.current), []);
  const { playPreview, stopPreview } = useAudioPreview({ maxDuration: AUDITION_SECONDS * 1000, preRollTime: 0, onPlayEnd: end, onError: fail });

  const toggle = useCallback((track: AuditionTrack) => {
    if (current.current === track.id) { stopPreview(); return; }
    current.current = track.id;
    setPlayingId(track.id);
    setFailedId(null);
    playPreview(track.filepath, auditionStart(track.duration));
  }, [playPreview, stopPreview]);
  const stop = useCallback(() => { if (current.current !== null) stopPreview(); }, [stopPreview]);

  const control = useMemo<AuditionControl>(() => ({ playingId, failedId, toggle }), [playingId, failedId, toggle]);
  return { control, playingId, stop };
}
