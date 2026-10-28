export interface PerformanceCuePoint {
  slot: number;
  position_ms: number;
  label: string;
  color: string | null;
}

export interface PerformanceLoop {
  id: string;
  start_ms: number;
  end_ms: number;
  label: string;
}

export interface PerformanceBeatGrid {
  bpm: number;
  first_beat_ms: number;
  beats_per_bar: number;
  beat_times_ms?: number[] | null;
  beat_numbers?: number[] | null;
  source?: "rekordbox" | "analysis" | "manual" | null;
  confidence?: number | null;
}

export interface PerformanceMetadata {
  track_id: number;
  revision: number;
  cue_points: PerformanceCuePoint[];
  loops: PerformanceLoop[];
  beat_grid: PerformanceBeatGrid | null;
  grid_warning?: string | null;
  /** The missing grid is analyzed in the background; GET again for it. */
  grid_pending?: boolean;
  created_at: string | null;
  updated_at: string | null;
}

export type PerformanceMetadataWrite = Pick<
  PerformanceMetadata,
  "revision" | "cue_points" | "loops" | "beat_grid"
>;
