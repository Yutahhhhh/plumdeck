import type { AssistArtist } from "@/services/assist";

/** Same folding as the backend filter: width, case and whitespace. */
export const foldArtist = (value: string) =>
  value.normalize("NFKC").toLowerCase().split(/\s+/).filter(Boolean).join(" ");

/**
 * Artists offered for what the DJ has typed: names starting with it first,
 * then names containing it, each keeping the library's most-tracks-first
 * order. Already selected artists are left out.
 */
export function matchArtists(artists: AssistArtist[], query: string, selected: string[], limit = 50): AssistArtist[] {
  const taken = new Set(selected.map(foldArtist));
  const wanted = foldArtist(query);
  const available = artists.filter(artist => !taken.has(foldArtist(artist.name)));
  if (!wanted) return available.slice(0, limit);
  const starts: AssistArtist[] = [];
  const contains: AssistArtist[] = [];
  for (const artist of available) {
    const name = foldArtist(artist.name);
    if (name.startsWith(wanted)) starts.push(artist);
    else if (name.includes(wanted)) contains.push(artist);
  }
  return [...starts, ...contains].slice(0, limit);
}

/** The backend accepts at most this many artists in one filter. */
export const MAX_ARTISTS = 32;

/** Adds an artist unless a spelling of it is already selected or the filter is full. */
export function addArtist(selected: string[], name: string): string[] {
  const text = name.trim().split(/\s+/).join(" ");
  if (!text || selected.length >= MAX_ARTISTS || selected.some(item => foldArtist(item) === foldArtist(text))) return selected;
  return [...selected, text];
}
