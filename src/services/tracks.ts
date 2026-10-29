import { apiClient } from "./api-client";
import { Track } from "@/types";

export const tracksService = {
  getTracks: async (params: Record<string, string | number | boolean | null | undefined | string[]>) => {
    return apiClient.get<Track[]>("/tracks", params);
  },
  getTrackIds: async (params: Record<string, string | number | boolean | null | undefined | string[]>) => {
    return apiClient.get<number[]>("/tracks/ids", params);
  },
  getCount: async (params: Record<string, string | number | boolean | null | undefined | string[]>) => {
    return apiClient.get<{ count: number }>("/tracks/count", params);
  },
  getSimilarTracks: async (trackId: number, limit: number = 20) => {
    return apiClient.get<Track[]>(`/tracks/${trackId}/similar`, { limit });
  },
  updateGenre: async (trackId: number, genre: string) => {
    return apiClient.patch<Track>(`/tracks/${trackId}/genre`, { genre });
  },
  updateTrackInfo: async (trackId: number, info: { title?: string; artist?: string; album?: string; year?: number }) => {
    return apiClient.patch<Track>(`/tracks/${trackId}/info`, info);
  },
  suggestGenre: async (trackId: number) => {
    return apiClient.get<{
      suggested_genre: string | null;
      suggested_subgenre?: string | null;
      confidence?: number | null;
      level?: "high" | "medium" | "low" | null;
      reason?: string | null;
    }>(`/tracks/${trackId}/suggest-genre`);
  },
};
