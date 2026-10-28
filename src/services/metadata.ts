import { apiClient } from "./api-client";

export interface FileMetadata {
  artwork: string | null;
  lyrics: string;
  waveform_peaks?: number[];
}

export const metadataService = {
  // ファイルからメタデータを取得
  getMetadata: (trackId: number) =>
    apiClient.get<FileMetadata>(`/metadata?track_id=${trackId}`),

  // 物理ファイルのタグ（歌詞・アートワーク）を更新
  updateMetadata: (
    trackId: number,
    updates: { lyrics?: string; artwork_data?: string }
  ) => apiClient.patch(`/metadata/update`, { track_id: trackId, ...updates }),

  fetchArtworkInfo: (trackId: number) =>
    apiClient.post<{ info: string }>(`/metadata/fetch-artwork-info`, {
      track_id: trackId,
    }),

  fetchLyricsSingle: (trackId: number) =>
    apiClient.post<{ lyrics: string }>(`/metadata/fetch-lyrics-single`, {
      track_id: trackId,
    }),

  getLyricsFromDB: (trackId: number) => 
    apiClient.get<{ content: string; source: string; language: string }>(`/tracks/${trackId}/lyrics`),
    
  updateLyricsInDB: (trackId: number, content: string) =>
    apiClient.put(`/tracks/${trackId}/lyrics`, { track_id: trackId, content }),
  
  startUpdate: (type: "release_date" | "lyrics", overwrite: boolean, trackIds?: number[]) =>
    apiClient.post("/metadata/update", { type, overwrite, track_ids: trackIds }),
  
  cancelUpdate: () =>
    apiClient.post("/metadata/cancel", {}),
  
  clearSkipCache: (updateType?: "release_date" | "lyrics") =>
    apiClient.post(`/metadata/clear-cache`, updateType ? { update_type: updateType } : {}),
};
