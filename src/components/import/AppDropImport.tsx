import { useEffect, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Music } from "lucide-react";
import { importToCollection } from "@/services/library-import";
import { importErrorToast, importedToast } from "./AddMusicButton";

/**
 * App-wide drop target: files or folders dropped anywhere are registered and
 * analyzed into the Collection through the shared import queue. Play mode
 * keeps its own drop handling (decks, playlists, sampler), so this listener
 * is off there.
 */
export function AppDropImport({ enabled }: { enabled: boolean }) {
  const [hovering, setHovering] = useState(false);
  useEffect(() => {
    if (!enabled) { setHovering(false); return; }
    let disposed = false;
    let cleanup: (() => void) | undefined;
    void Promise.resolve().then(() => getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === "enter") { setHovering(payload.paths.length > 0); return; }
      if (payload.type === "over") return;
      setHovering(false);
      if (payload.type !== "drop" || payload.paths.length === 0) return;
      void importToCollection(payload.paths, "native_file_drop").then(importedToast).catch(importErrorToast);
    })).then((unlisten) => { if (disposed) unlisten(); else cleanup = unlisten; }).catch(() => undefined);
    return () => { disposed = true; cleanup?.(); };
  }, [enabled]);
  if (!enabled || !hovering) return null;
  return (
    <div className="pointer-events-none fixed inset-0 z-[150] flex items-center justify-center bg-background/70 backdrop-blur-sm animate-in fade-in-0 duration-150">
      <div className="flex flex-col items-center gap-3 rounded-2xl border-2 border-dashed border-primary/60 bg-card/90 px-12 py-10 text-center shadow-2xl">
        <div className="rounded-full bg-primary/10 p-4 text-primary"><Music className="h-8 w-8" /></div>
        <p className="text-lg font-semibold">ドロップしてライブラリに追加</p>
        <p className="text-sm text-muted-foreground">曲やフォルダをまとめて追加できます。解析はバックグラウンドで進みます。</p>
      </div>
    </div>
  );
}
