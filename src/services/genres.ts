import { apiClient } from "./api-client";
import { Track } from "../types";

export interface TrackSuggestion {
  id: number;
  title: string;
  artist: string;
  bpm: number;
  filepath: string;
  current_genre?: string;
}

export interface GenreBatchUpdateRequest {
  parent_track_id: number;
  target_track_ids: number[];
}

export type AnalysisMode = "genre" | "subgenre" | "both";

export interface GenreAnalysisResult {
  genre: string;
  subgenre?: string;
  reason: string;
  confidence: string;
}

export interface GenreUpdateResult {
  track_id: number;
  title: string;
  artist: string;
  old_genre: string;
  new_genre: string;
}

export type ConfidenceLevel = "high" | "medium" | "low";

export interface GenreMetric {
  genre: string;
  support: number;
  recall: number;
}

export interface ClassifierMetrics {
  samples: number;
  train_samples: number;
  holdout_samples: number;
  genre_accuracy: number;
  genre_top3_accuracy: number;
  subgenre_accuracy: number;
  high_confidence_coverage: number;
  high_confidence_accuracy: number;
  low_confidence_share: number;
  calibration_error: number;
  per_genre: GenreMetric[];
}

export interface ClassifierStatus {
  state: "untrained" | "training" | "ready";
  progress: number | null;
  training_labels: number | null;
  error: string | null;
  auto_started: boolean;
  trained_at: string | null;
  genres: string[];
  subgenre_count: number;
  metrics: ClassifierMetrics | null;
  new_labels: number | null;
  usable_labels: number | null;
  min_labels: number;
  auto_retrain_after: number;
  low_confidence: number;
  high_confidence: number;
}

export interface GenreCandidate {
  genre: string;
  subgenre?: string | null;
  probability: number;
}

export interface GenrePrediction {
  genre: string;
  confidence: number;
  level: ConfidenceLevel;
  subgenre: string | null;
  subgenre_confidence: number;
  candidates: GenreCandidate[];
  pair_candidates: GenreCandidate[];
}

export interface PredictedTrack {
  track: { id: number; title: string; artist: string; bpm: number; filepath: string };
  prediction: GenrePrediction;
}

export interface PredictionPage {
  trained: boolean;
  total: number;
  counts: Record<ConfidenceLevel, number>;
  without_features: number;
  items: PredictedTrack[];
}

export interface AcceptedGenre {
  track_id: number;
  genre: string;
  subgenre?: string | null;
}

export interface GenreCleanupGroup {
  primary_genre: string;
  variant_genres: string[];
  track_count: number;
  suggestions: TrackSuggestion[];
}

export const genreService = {
  getAllGenres: async (): Promise<string[]> => {
    return apiClient.get<string[]>("/genres/list");
  },

  getAllSubgenres: async (): Promise<string[]> => {
    return apiClient.get<string[]>("/genres/subgenres");
  },

  getUnknownTracks: async (
    offset: number = 0,
    limit: number = 50,
    mode: AnalysisMode = "genre"
  ): Promise<Track[]> => {
    return apiClient.get<Track[]>("/genres/unknown", { offset, limit, mode });
  },

  getAllUnknownTrackIds: async (mode: AnalysisMode = "genre"): Promise<number[]> => {
    return apiClient.get<number[]>("/genres/unknown-ids", { mode });
  },

  batchUpdateGenres: async (
    parentTrackId: number,
    targetTrackIds: number[]
  ): Promise<{ updated_count: number; genre: string }> => {
    return apiClient.post<{ updated_count: number; genre: string }>(
      "/genres/batch-update",
      {
        parent_track_id: parentTrackId,
        target_track_ids: targetTrackIds,
      }
    );
  },

  getCleanupSuggestions: async (mode: AnalysisMode = "genre"): Promise<GenreCleanupGroup[]> => {
    return apiClient.get<GenreCleanupGroup[]>("/genres/cleanup-suggestions", { mode });
  },

  executeCleanup: async (
    targetGenre: string,
    trackIds: number[],
    mode: AnalysisMode = "genre"
  ): Promise<{ updated_count: number; genre: string }> => {
    return apiClient.post<{ updated_count: number; genre: string }>(
      "/genres/cleanup-execute",
      {
        target_genre: targetGenre,
        track_ids: trackIds,
        mode,
      }
    );
  },

  getClassifierStatus: async (): Promise<ClassifierStatus> => {
    return apiClient.get<ClassifierStatus>("/genres/classifier");
  },

  trainClassifier: async (): Promise<ClassifierStatus> => {
    return apiClient.post<ClassifierStatus>("/genres/classifier/train", {});
  },

  getPredictions: async (
    level: ConfidenceLevel | "all" = "all",
    offset = 0,
    limit = 200
  ): Promise<PredictionPage> => {
    return apiClient.get<PredictionPage>("/genres/classifier/predictions", { level, offset, limit });
  },

  acceptPredictions: async (
    items: AcceptedGenre[]
  ): Promise<{ updated: number; new_labels: number; retraining: boolean }> => {
    return apiClient.post("/genres/classifier/accept", { items });
  },

  applyGenresToFiles: async (
    trackIds: number[]
  ): Promise<{ success: number; failed: number }> => {
    return apiClient.post<{ success: number; failed: number }>(
      "/genres/apply-to-files",
      { track_ids: trackIds }
    );
  },
};
