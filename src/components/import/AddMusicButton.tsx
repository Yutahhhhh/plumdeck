import { useState } from "react";
import { FolderOpen, FileAudio, Loader2, Plus } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { toast } from "@/components/ui/toast";
import { importToCollection, pickMusic } from "@/services/library-import";

/** Friendly wording for an import failure; the raw reason goes in the description. */
export function importErrorToast(cause: unknown) {
  const detail = cause instanceof Error ? cause.message : String(cause);
  toast.error("曲を追加できませんでした", detail.includes("対応する読み取り可能な音源がありません")
    ? "選んだ場所に対応している音源（MP3・WAV・FLAC・AIFF・M4A など）が見つかりませんでした。"
    : detail);
}

export function importedToast(count: number) {
  toast.success(`${count.toLocaleString()}曲を追加しています`, "バックグラウンドで解析します。終わった曲から Library に並びます。");
}

/** 「曲を追加」: the OS file or folder picker into the shared import queue. */
export function AddMusicButton({ size = "sm", variant = "default" }: { size?: "sm" | "default"; variant?: "default" | "outline" }) {
  const [busy, setBusy] = useState(false);
  const choose = async (kind: "files" | "folder") => {
    if (busy) return;
    try {
      const paths = await pickMusic(kind);
      if (!paths) return;
      setBusy(true);
      importedToast(await importToCollection(paths, "file_picker"));
    } catch (cause) {
      importErrorToast(cause);
    } finally {
      setBusy(false);
    }
  };
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button size={size} variant={variant} disabled={busy} className="gap-1.5">
          {busy ? <Loader2 className="h-4 w-4 animate-spin" /> : <Plus className="h-4 w-4" />}
          曲を追加
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-56">
        <DropdownMenuItem onSelect={() => void choose("files")} className="gap-2">
          <FileAudio className="h-4 w-4" />
          ファイルを選ぶ…
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => void choose("folder")} className="gap-2">
          <FolderOpen className="h-4 w-4" />
          フォルダごと追加…
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
