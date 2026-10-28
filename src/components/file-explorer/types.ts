export interface FileItem {
  name: string;
  path: string;
  is_dir: boolean;
  is_analyzed?: boolean;
  analysis_level?: "light" | "full" | null;
  /** The optional similarity vector is stored (otherwise queued in the background). */
  embedding_ready?: boolean;
}
