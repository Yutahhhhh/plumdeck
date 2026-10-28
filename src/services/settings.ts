import { apiClient, API_BASE_URL } from "./api-client";
import { Track } from "@/types";

export interface BaseAnalysisResult {
  total_rows: number;
}

export interface CsvImportRow {
  filepath: string;
  title: string;
  artist: string;
  album?: string;
  genre?: string;
  subgenre?: string;
  year?: number;
  bpm?: number;
  key?: string;
  energy?: number;
  danceability?: number;
  brightness?: number;
  loudness?: number;
  noisiness?: number;
  contrast?: number;
  duration?: number;
  is_genre_verified?: boolean;
  [key: string]: string | number | boolean | undefined;
}

export interface MetadataImportRow {
  filepath: string;
  title?: string;
  artist?: string;
  album?: string;
  genre?: string;
  subgenre?: string;
  year?: number;
  /** Imported as a manual value that rekordbox and re-analysis keep. */
  bpm?: number | null;
  key?: string | null;
  is_genre_verified?: boolean;
}

export interface LibraryAnalysisResult extends BaseAnalysisResult {
  new_tracks: CsvImportRow[];
  duplicates: CsvImportRow[];
  path_updates: {
    old_path: string;
    new_path: string;
    track: CsvImportRow;
  }[];
}

export interface MetadataAnalysisResult extends BaseAnalysisResult {
  updates: {
    filepath: string;
    current: Track;
    new: MetadataImportRow;
  }[];
  not_found: MetadataImportRow[];
}

export const settingsService = {
  getAll: async () => {
    return apiClient.get<Record<string, string>>("/settings");
  },
  
  save: async (key: string, value: string) => {
    return apiClient.post("/settings", { key, value });
  },

  getExportUrl: (type: 'library' | 'metadata') => {
    const endpoints = {
      library: "/settings/export/csv",
      metadata: "/settings/metadata/export",
    };
    return `${API_BASE_URL}${endpoints[type]}`;
  },

  analyzeImport: async <T>(file: File, type: 'library' | 'metadata'): Promise<T> => {
    const endpoints = {
      library: "/settings/import/analyze",
      metadata: "/settings/metadata/import/analyze",
    };
    
    const formData = new FormData();
    formData.append("file", file);

    const res = await fetch(`${API_BASE_URL}${endpoints[type]}`, {
      method: "POST",
      body: formData,
    });

    if (!res.ok) {
      const error = await res.json().catch(() => ({ detail: "Analysis failed" }));
      throw new Error(error.detail || "Analysis failed");
    }
    
    return res.json();
  },

  executeImport: async (payload: any, type: 'library' | 'metadata') => {
    const endpoints = {
      library: "/settings/import/execute",
      metadata: "/settings/metadata/import/execute",
    };
    
    return apiClient.post<{ message: string }>(endpoints[type], payload);
  }
};
