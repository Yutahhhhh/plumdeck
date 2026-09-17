import type { Track } from "../types";
import type { DeckId } from "../types/dj-engine";

export type PlayTrackDragData = { kind: "play-track"; track: Track };
export type PlayDropData = { kind: "deck"; deck: DeckId } | { kind: "action"; accept: (track: Track) => void };

export function readPlayTrackDrag(data: unknown): Track | null {
  if (!data || typeof data !== "object") return null;
  const value = data as Partial<PlayTrackDragData>;
  return value.kind === "play-track" && value.track && Number.isSafeInteger(value.track.id) && value.track.id > 0 && Boolean(value.track.filepath)
    ? value.track : null;
}

export function routePlayTrackDrop(active: unknown, target: unknown, load: (deck: DeckId, track: Track) => void): boolean {
  if (!target || typeof target !== "object") return false;
  const drop = target as Partial<PlayDropData>;
  const deck = drop.kind === "deck" && typeof drop.deck === "string" && ["A", "B", "C", "D"].includes(drop.deck) ? drop.deck as DeckId : null;
  const track = readPlayTrackDrag(active);
  if (!track) return false;
  if (deck) {
    load(deck, track);
    return true;
  }
  if (drop.kind === "action" && typeof drop.accept === "function") {
    drop.accept(track);
    return true;
  }
  return false;
}
