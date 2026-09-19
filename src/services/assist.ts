import { invoke, isTauri } from "@tauri-apps/api/core";
import { apiClient } from "./api-client";
import type { Track } from "@/types";

/** Which way to move the floor. "wordplay" is the separate approved-pair mode. */
export type AssistIntent =
  | "keep" | "hype" | "dance" | "calm" | "emotional" | "bright" | "throwback" | "wordplay";
export type GenreScope = "any" | "same_genre" | "same_subgenre";
export type ReasonTone = "good" | "neutral" | "caution";
export type DeckStatus = "empty" | "resolved" | "ambiguous" | "unresolved" | "not_in_library";
export type MatchConfidence = "title_and_artist" | "title_only";

/** One deck exactly as rekordbox is drawing it. Never an identity on its own. */
export interface DeckObservation {
  slot: number;
  loaded: boolean;
  title: string | null;
  artist: string | null;
  track_bpm: number | null;
  tempo_bpm: number | null;
  display_key: string | null;
}

export interface AssistSnapshot {
  supported: boolean;
  permission_granted: boolean;
  app_running: boolean;
  app_path: string | null;
  layout_hint: string | null;
  decks: DeckObservation[];
  open_audio_paths: string[];
  open_paths_error: string | null;
  signature: string;
  captured_at_ms: number;
  unavailable_reason: string | null;
  warnings: string[];
}

export interface AmbiguousCandidate {
  rekordbox_id: string;
  filepath: string;
  title: string;
  artist: string;
}

export interface ResolvedDeck {
  slot: number;
  status: DeckStatus;
  observed: Omit<DeckObservation, "slot" | "loaded">;
  track: Track | null;
  rekordbox_id: string | null;
  filepath: string | null;
  match_confidence: MatchConfidence | null;
  candidates: AmbiguousCandidate[];
  message: string | null;
}

export interface DeckResolution {
  decks: ResolvedDeck[];
  library_error: string | null;
  inspected_paths: number;
}

export interface AssistReason {
  kind: string;
  tone: ReasonTone;
  text: string;
}

export interface AssistWordplay {
  pair_id: number;
  keyword: string;
  source_phrase: string;
  target_phrase: string;
  transition_notes: string;
  evidence_type: string;
  verification_status: string;
}

/** Compound conditions: AND across fields, OR inside a list. */
export interface AssistFilters {
  bpm_min: number | null;
  bpm_max: number | null;
  genres: string[];
  subgenres: string[];
  artists: string[];
  keys: string[];
  year_min: number | null;
  year_max: number | null;
  query: string;
}

export const EMPTY_FILTERS: AssistFilters = {
  bpm_min: null, bpm_max: null, genres: [], subgenres: [], artists: [], keys: [],
  year_min: null, year_max: null, query: "",
};

export const hasFilters = (filters: AssistFilters) =>
  filters.bpm_min != null || filters.bpm_max != null || filters.genres.length > 0
  || filters.subgenres.length > 0 || filters.artists.length > 0 || filters.keys.length > 0
  || filters.year_min != null || filters.year_max != null || filters.query.trim() !== "";

export interface AssistCandidate extends Track {
  /** Preset fit; null for plain search results that have no deck to mix from. */
  score: number | null;
  components: Record<string, number>;
  /** One line: what the preset asked for and the measured moves behind it. */
  summary: string;
  strengths: string[];
  reasons: AssistReason[];
  wordplay: AssistWordplay | null;
  /** Set on tracks a chat agent picked: its own reason for the pick. */
  agent_reason?: string | null;
}

export interface AssistAlternative {
  intent: AssistIntent;
  label: string;
  track: AssistCandidate;
}

export interface AssistRecommendations {
  intent: AssistIntent;
  intent_label: string;
  transition: boolean;
  source_track_id: number;
  basis: string;
  filters: string[];
  candidates: AssistCandidate[];
  alternatives: AssistAlternative[];
  notes: string[];
  caveats: string[];
  unavailable_originals: number;
}

export interface AssistRouteStep {
  from_track_id: number;
  to_track_id: number;
  score: number;
  summary: string;
  reasons: AssistReason[];
}

export interface AssistRoute {
  score: number;
  tracks: Track[];
  steps: AssistRouteStep[];
}

export interface AssistRoutes {
  source: Track | null;
  target: Track | null;
  intent: AssistIntent;
  intent_label: string;
  transition: boolean;
  max_intermediate: number;
  routes: AssistRoute[];
  notes: string[];
  caveats: string[];
}

export interface AssistSearchResult {
  filters: string[];
  candidates: AssistCandidate[];
  notes: string[];
  unavailable_originals: number;
  total_matches: number;
}

/** What the window is showing, reported so an MCP agent can read it. */
export interface AssistWindowState {
  deck_slot: number | null;
  source_track_id: number | null;
  destination_track_id: number | null;
  intent: AssistIntent;
  transition: boolean;
  genre_scope: GenreScope;
  limit: number;
  filters: AssistFilters;
  exclude_track_ids: number[];
  shown_track_ids: number[];
}

/** Settings an MCP agent asked the window to switch to. */
export interface AssistAgentSettings {
  revision: number;
  agent_name: string | null;
  intent?: AssistIntent;
  transition?: boolean;
  genre_scope?: GenreScope;
  limit?: number;
  filters?: AssistFilters;
}

/** Tracks an MCP agent put on the window, already checked as loadable. */
export interface AssistAgentList {
  revision: number;
  title: string;
  note: string | null;
  tracks: AssistCandidate[];
  rejected: { track_id: number; reason: string }[];
  agent_name: string | null;
  created_at: number;
}

export interface AssistAgentUpdates {
  revision: number;
  settings: AssistAgentSettings | null;
  list: AssistAgentList | null;
}

export interface CompactWindowResult {
  applied: boolean;
  message: string | null;
}

/** Snapshot used when there is no desktop shell to read rekordbox with. */
export const UNAVAILABLE_SNAPSHOT: AssistSnapshot = {
  supported: false,
  permission_granted: false,
  app_running: false,
  app_path: null,
  layout_hint: null,
  decks: [],
  open_audio_paths: [],
  open_paths_error: null,
  signature: "",
  captured_at_ms: 0,
  unavailable_reason:
    "デッキの読み取りは plumdeck デスクトップアプリでのみ動作します（ブラウザプレビューでは無効）",
  warnings: [],
};

export const isDesktopShell = () => isTauri();

export const assistService = {
  /** Reads the decks rekordbox currently has loaded. Read-only. */
  async snapshot(): Promise<AssistSnapshot> {
    if (!isDesktopShell()) return UNAVAILABLE_SNAPSHOT;
    return invoke<AssistSnapshot>("assist_snapshot");
  },

  /** Shows the macOS accessibility prompt. Only call from a user gesture. */
  async requestAccessibility(): Promise<boolean> {
    if (!isDesktopShell()) return false;
    return invoke<boolean>("assist_request_accessibility");
  },

  async openAccessibilitySettings(): Promise<void> {
    if (!isDesktopShell()) return;
    await invoke("assist_open_accessibility_settings");
  },

  async enterCompactWindow(width: number, height: number): Promise<CompactWindowResult> {
    if (!isDesktopShell()) return { applied: false, message: null };
    return invoke<CompactWindowResult>("assist_enter_compact_window", { width, height });
  },

  async exitCompactWindow(): Promise<CompactWindowResult> {
    if (!isDesktopShell()) return { applied: false, message: null };
    return invoke<CompactWindowResult>("assist_exit_compact_window");
  },

  async setAlwaysOnTop(enabled: boolean): Promise<void> {
    if (!isDesktopShell()) return;
    await invoke("assist_set_always_on_top", { enabled });
  },

  libraryStatus: () =>
    apiClient.get<{ available: boolean; message: string | null; registered_tracks: number }>(
      "/assist/library-status",
    ),

  resolveDecks: (decks: DeckObservation[], openAudioPaths: string[]) =>
    apiClient.post<DeckResolution>("/assist/decks/resolve", {
      decks,
      open_audio_paths: openAudioPaths,
    }, 30_000),

  recommendations: (params: {
    sourceTrackId: number;
    intent: AssistIntent;
    transition?: boolean;
    genreScope?: GenreScope;
    limit?: number;
    excludeTrackIds?: number[];
    filters?: AssistFilters;
  }) =>
    apiClient.post<AssistRecommendations>("/assist/recommendations", {
      source_track_id: params.sourceTrackId,
      intent: params.intent,
      transition: params.transition ?? false,
      genre_scope: params.genreScope ?? "any",
      limit: params.limit ?? 12,
      exclude_track_ids: params.excludeTrackIds ?? [],
      filters: params.filters ?? EMPTY_FILTERS,
    }, 60_000),

  routes: (params: {
    sourceTrackId: number;
    targetTrackId: number;
    intent: AssistIntent;
    transition?: boolean;
    maxIntermediate?: number;
    limit?: number;
    genreScope?: GenreScope;
    excludeTrackIds?: number[];
    filters?: AssistFilters;
  }) =>
    apiClient.post<AssistRoutes>("/assist/routes", {
      source_track_id: params.sourceTrackId,
      target_track_id: params.targetTrackId,
      intent: params.intent,
      transition: params.transition ?? false,
      max_intermediate: params.maxIntermediate ?? 2,
      limit: params.limit ?? 3,
      genre_scope: params.genreScope ?? "any",
      exclude_track_ids: params.excludeTrackIds ?? [],
      filters: params.filters ?? EMPTY_FILTERS,
    }, 60_000),

  search: (params: { filters: AssistFilters; limit?: number; excludeTrackIds?: number[] }) =>
    apiClient.post<AssistSearchResult>("/assist/search", {
      filters: params.filters,
      limit: params.limit ?? 12,
      exclude_track_ids: params.excludeTrackIds ?? [],
    }, 60_000),

  reportWindow: (state: AssistWindowState) => apiClient.put<{ ok: boolean }>("/assist/window", state),

  agentUpdates: () => apiClient.get<AssistAgentUpdates>("/assist/agent"),

  dismissAgentList: () => apiClient.delete("/assist/agent/list"),
};
