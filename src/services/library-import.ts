import { open } from "@tauri-apps/plugin-dialog";
import { apiClient } from "./api-client";
import { workflowsService, type ImportBatch } from "./workflows";

/** Extensions the import queue accepts (analysis::SUPPORTED_EXTENSIONS). */
export const AUDIO_EXTENSIONS = ["mp3", "m4a", "flac", "wav", "ogg", "aac", "aiff"];

export type AttentionKind = "unanalyzed" | "failed" | "timbre_default" | "embedding_pending" | "outdated";

export interface AttentionJob {
  id: string;
  status: string;
  features: string[];
  total: number;
  done: number;
  failed: number;
}

export interface LibraryAttention extends Record<AttentionKind, number> {
  job: AttentionJob | null;
}

export interface RekordboxCandidate {
  id: string;
  title: string | null;
  artist: string | null;
  filepath: string;
  bpm: number | null;
  key: string | null;
  duration: number | null;
}

export interface RekordboxUnregistered {
  available: boolean;
  total: number;
  tracks: RekordboxCandidate[];
}

/** Tells the queue indicator and the Dashboard that a batch was added. */
export const announceImport = () => window.dispatchEvent(new Event("plumdeck:imports-changed"));

/** Max paths per request (POST /workflows/play/imports). */
const CHUNK = 2000;

/**
 * Registers files or folders in the Collection through the one import queue
 * (the same pipeline as Play's drops). Folders are expanded by the backend.
 * Returns how many audio files were queued.
 */
export async function importToCollection(paths: string[], origin: "native_file_drop" | "file_picker"): Promise<number> {
  let queued = 0;
  for (let start = 0; start < paths.length; start += CHUNK) {
    const batch: ImportBatch = await workflowsService.createImport(paths.slice(start, start + CHUNK), { kind: "collection" }, origin);
    queued += batch.items?.length ?? Number(batch.total_items ?? 0);
  }
  announceImport();
  return queued;
}

/** Opens the OS picker. `null` when the user cancels. */
export async function pickMusic(kind: "files" | "folder"): Promise<string[] | null> {
  const picked = kind === "folder"
    ? await open({ directory: true, multiple: true, title: "取り込むフォルダを選択" })
    : await open({ multiple: true, title: "取り込む曲を選択", filters: [{ name: "音源", extensions: AUDIO_EXTENSIONS }] });
  if (!picked) return null;
  const paths = Array.isArray(picked) ? picked : [picked];
  return paths.length ? paths : null;
}

export const libraryImportService = {
  attention: () => apiClient.get<LibraryAttention>("/library/attention", undefined, 30_000),
  fixAttention: async (kind: AttentionKind) => {
    const result = await apiClient.post<{ started: number }>(`/library/attention/${kind}`, {}, 120_000);
    announceImport();
    return result;
  },
  rekordboxUnregistered: () => apiClient.get<RekordboxUnregistered>("/rekordbox/unregistered", undefined, 120_000),
  importRekordbox: async (ids?: string[]) => {
    const result = await apiClient.post<{ started: number }>("/rekordbox/unregistered/import", ids ? { ids } : {}, 120_000);
    announceImport();
    return result;
  },
};
