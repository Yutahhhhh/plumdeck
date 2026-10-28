import { apiClient } from "./api-client";
import { Setlist } from "./setlists";

export interface DashboardStats {
  total_tracks: number;
  analyzed_tracks: number;
  unanalyzed_tracks: number;
  /** Analyzed tracks whose similarity vector is still computed in the background. */
  embedding_pending_tracks?: number;
  genre_distribution: {
    name: string;
    count: number;
  }[];
  unverified_genres_count: number;
  lyrics_tracks_count: number;
  recent_setlists: Setlist[];
  config: {
    has_root_path: boolean;
  };
}

export interface SystemHealth {
  status: string;
  duckdb_version: string;
  ai_runtime: "mcp-client";
}

export const systemService = {
  getHealth: async () => {
    return apiClient.get<SystemHealth>("/");
  },
  getDashboardStats: async () => {
    return apiClient.get<DashboardStats>("/dashboard");
  },
  saveFile: (path: string, content: string) =>
    apiClient.post("/system/save-file", { path, content }),
  revealFile: (path: string) =>
    apiClient.post("/system/reveal-file", { path }),
};
