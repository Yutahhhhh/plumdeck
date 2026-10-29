import { apiClient } from "./api-client";
import type { Track } from "@/types";

export interface MirrorSource {
  id: string;
  name: string;
  imported_at: string;
  playlist_count: number;
  member_count: number;
  unmapped_count: number;
}

export interface MirrorPlaylist {
  source_id: string;
  external_id: string;
  parent_external_id: string | null;
  name: string;
  sort_order: number;
  kind: "folder" | "playlist" | "smart";
  track_count: number;
  unmapped_count: number;
}

export interface MirrorTrack extends Partial<Track> {
  position: number;
  local_track_id: number | null;
  resolved: boolean;
  source_filepath: string | null;
  title: string;
  artist: string;
  duration: number;
}

export interface HistoryTrack extends Partial<Track> {
  id: number;
  event_key: string;
  session_id: string;
  deck: string;
  track_id: number;
  loaded_at: string;
  first_played_at: string | null;
  ended_at: string | null;
  played_ms: number;
  completed: boolean;
  reason: string | null;
}

export interface RecordingEntry {
  revision: number;
  id: number;
  recording_key: string;
  session_id: string | null;
  filepath: string;
  started_at: string;
  ended_at: string | null;
  duration_ms: number;
  status: "recording" | "completed" | "failed";
  error: string | null;
  artist: string | null;
  title: string | null;
}

export type RecordingExportFormat = "wav" | "flac" | "mp3";
export interface RecordingFormatOption { value: RecordingExportFormat; extension: string; label: string }

export interface Page<T> { items: T[]; total: number; limit: number; offset: number; has_more: boolean }
export interface LocalPlaylist { id: number; name: string; source: "plumdeck"; editable: true; track_count: number; created_at?: string; updated_at?: string }
export interface LocalPlaylistTrack extends Track { setlist_track_id: number; position: number }
export interface MirrorCopyResult { playlist: LocalPlaylist; copied: number; resolved?: number; skipped_unresolved: number }

export const playService = {
  recordingAudioUrl: (id: number) => `${apiClient.baseUrl}/play/recordings/${id}/audio`,
  recordingFormats: () => apiClient.get<{ formats: RecordingFormatOption[] }>("/play/recordings/formats", undefined, 190_000),
  nameRecording: (id: number, artist: string, title: string, format?: RecordingExportFormat) =>
    apiClient.patch<RecordingEntry>(`/play/recordings/${id}`, { artist, title, format }, 31 * 60_000),
  discardRecording: (id: number) => apiClient.delete(`/play/recordings/${id}`),
  tracksPage: (params: { q?: string; limit?: number; offset?: number; genres?: string[]; subgenres?: string[]; sort?: string; order?: "asc" | "desc"; bpm?: number; bpm_range?: number }) => apiClient.get<Page<Track>>("/tracks/page", params),
  recommendationsPage: (trackId: number, params: { q?: string; limit?: number; offset?: number; genres?: string[]; subgenres?: string[] } = {}) => apiClient.get<Page<Track>>("/recommendations/next/page", { track_id: trackId, ...params }),
  localPlaylists: (limit = 100, offset = 0) => apiClient.get<Page<LocalPlaylist>>("/play/playlists", { limit, offset }),
  createLocalPlaylist: (name: string) => apiClient.post<LocalPlaylist>("/play/playlists", { name }),
  renameLocalPlaylist: (id: number, name: string) => apiClient.patch<LocalPlaylist>(`/play/playlists/${id}`, { name }),
  deleteLocalPlaylist: (id: number) => apiClient.delete(`/play/playlists/${id}`),
  localPlaylistTracks: (id: number, limit = 100, offset = 0) => apiClient.get<Page<LocalPlaylistTrack>>(`/play/playlists/${id}/tracks`, { limit, offset }),
  addLocalPlaylistTrack: (id: number, trackId: number, position?: number) => apiClient.post<{ setlist_track_id: number }>(`/play/playlists/${id}/tracks`, { track_id: trackId, position }),
  removeLocalPlaylistTrack: (id: number, setlistTrackId: number) => apiClient.delete(`/play/playlists/${id}/tracks/${setlistTrackId}`),
  sources: () => apiClient.get<MirrorSource[]>("/play/rekordbox/sources"),
  tree: (sourceId: string) => apiClient.get<{ source: MirrorSource; items: MirrorPlaylist[] }>(`/play/rekordbox/${encodeURIComponent(sourceId)}/tree`),
  playlistTracks: (sourceId: string, playlistId: string) => apiClient.get<MirrorTrack[]>(`/play/rekordbox/${encodeURIComponent(sourceId)}/playlists/${encodeURIComponent(playlistId)}/tracks`),
  mirrorTreePage: (sourceId: string, parentExternalId: string | null, limit = 100, offset = 0) => apiClient.get<Page<MirrorPlaylist>>(`/play/rekordbox/${encodeURIComponent(sourceId)}/tree/page`, { parent_external_id: parentExternalId, limit, offset }),
  mirrorPlaylistTracksPage: (sourceId: string, playlistId: string, limit = 100, offset = 0) => apiClient.get<Page<MirrorTrack>>(`/play/rekordbox/${encodeURIComponent(sourceId)}/playlists/${encodeURIComponent(playlistId)}/tracks/page`, { limit, offset }),
  copyMirrorPlaylist: (sourceId: string, playlistId: string, name?: string) => apiClient.post<MirrorCopyResult>(`/play/rekordbox/${encodeURIComponent(sourceId)}/playlists/${encodeURIComponent(playlistId)}/copy`, { name }),
  startSession: (id: string, deckCount: 2 | 4) => apiClient.post("/play/sessions", { id, deck_count: deckCount }),
  endSession: (id: string) => apiClient.post(`/play/sessions/${encodeURIComponent(id)}/end`, {}),
  upsertHistory: (entry: Record<string, unknown>) => apiClient.put("/play/history", entry),
  history: () => apiClient.get<HistoryTrack[]>("/play/history", { limit: 200 }),
  recordings: () => apiClient.get<RecordingEntry[]>("/play/recordings", { limit: 200 }),
  upsertRecording: (entry: Record<string, unknown>) => apiClient.put<{ recording_key: string; id: number }>("/play/recordings", entry),
};
