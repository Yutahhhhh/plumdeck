import { apiClient } from "@/services/api-client";
import { DECK_IDS, type EngineClientState } from "@/types/dj-engine";
import { djEngineClient } from "./client";

/**
 * Mirrors the Play decks to the local backend (`PUT /play/live-decks`) so other local tools — the cluster
 * show director — can follow what is on the decks without rekordbox. One reporter for the whole app,
 * started by whichever view mounts the engine hook first.
 */
const REPORT_INTERVAL_MS = 1000;

export interface LiveDeckReport {
  slot: number;
  loaded: boolean;
  playing: boolean;
  title: string | null;
  artist: string | null;
  track_bpm: number | null;
  tempo_bpm: number | null;
  display_key: string | null;
  elapsed_seconds: number | null;
}

export function buildLiveDecks(state: EngineClientState): LiveDeckReport[] {
  const decks = state.snapshot?.decks;
  if (!decks) return [];
  return DECK_IDS.map((id, index) => {
    const deck = decks[id];
    const loaded = !!deck?.track && deck.status !== "empty" && deck.status !== "loading";
    return {
      slot: index + 1,
      loaded,
      playing: loaded && deck.status === "playing",
      title: loaded ? deck.track?.title ?? null : null,
      artist: loaded ? deck.track?.artist ?? null : null,
      track_bpm: loaded ? deck.track?.bpm ?? null : null,
      tempo_bpm: loaded ? deck.effectiveBpm ?? null : null,
      display_key: null,
      elapsed_seconds: loaded ? Math.round(deck.positionMs / 100) / 10 : null,
    };
  });
}

let users = 0;
let timer: ReturnType<typeof setInterval> | null = null;
let inFlight = false;

async function send() {
  if (inFlight) return;
  inFlight = true;
  try {
    await apiClient.put("/play/live-decks", { decks: buildLiveDecks(djEngineClient.getState()) });
  } catch {
    // The backend being briefly unreachable must never disturb playback.
  } finally {
    inFlight = false;
  }
}

/** Starts the shared reporter; returns the matching stop function. */
export function startLiveDeckReport(): () => void {
  users += 1;
  if (!timer) timer = setInterval(() => void send(), REPORT_INTERVAL_MS);
  void send();
  return () => {
    users -= 1;
    if (users === 0 && timer) {
      clearInterval(timer);
      timer = null;
    }
  };
}
