import { apiClient } from "./api-client";

export type GridJobPlan = {
  selected_tracks: number; matched_tracks: number; already_current: number;
  /** Tracks left out because rekordbox has a PQTZ grid for them. */
  rekordbox_grid_tracks?: number;
};
export type GridJob = {
  id?: string; status: string; config?: { features: string[]; background?: boolean }; total?: number; remaining?: number;
  elapsed?: number; estimated_remaining_seconds?: number | null;
  counts?: { completed: number; skipped: number; failed: number; pending: number; running: number };
  current_tracks?: { track_id: number; filepath: string }[];
  errors?: { track_id: number; filepath: string; error: string }[]; error?: string | null;
};
export const gridJobs = {
  plan: (only_outdated: boolean) => apiClient.post<GridJobPlan>("/grid-jobs/plan", { only_outdated }),
  start: (only_outdated: boolean) => apiClient.post<GridJob>("/grid-jobs", { only_outdated }),
  status: () => apiClient.get<GridJob>("/grid-jobs", undefined, 5000),
  control: (id: string, action: "pause" | "resume" | "retry") => apiClient.post<GridJob>(`/grid-jobs/${id}/${action}`, {}),
};
