import { useCallback, useEffect, useMemo, useState } from "react";
import type { AssistSnapshot } from "@/services/assist";
import { advancePlayback, EMPTY_PLAYBACK, pickMaster, playbackReadable, playingSlots } from "./deck-playback";

export type BaseDeckPick = number | "master";

/** The deck suggestions are built from: picked by hand, or MASTER following whichever deck is playing. */
export function useBaseDeck(snapshot: AssistSnapshot | null) {
  const [playback, setPlayback] = useState(EMPTY_PLAYBACK);
  const [pick, setPick] = useState<BaseDeckPick>(1);
  const [followed, setFollowed] = useState(1);
  const master = pick === "master";
  const slot = master ? followed : pick;

  useEffect(() => {
    setPlayback(prev => snapshot ? advancePlayback(prev, snapshot.captured_at_ms, snapshot.decks) : EMPTY_PLAYBACK);
  }, [snapshot]);
  useEffect(() => {
    if (master) setFollowed(prev => pickMaster(playback, prev, prev));
  }, [playback, master]);

  const select = useCallback((value: BaseDeckPick) => {
    // Choosing MASTER re-decides from scratch; the old base only matters when nothing plays.
    if (value === "master") setFollowed(pickMaster(playback, null, slot));
    setPick(value);
  }, [playback, slot]);

  const playing = useMemo(() => playingSlots(playback), [playback]);
  const readable = !!snapshot && playbackReadable(snapshot.decks);
  return { slot, master, select, playing, masterAvailable: readable };
}

export type BaseDeck = ReturnType<typeof useBaseDeck>;
