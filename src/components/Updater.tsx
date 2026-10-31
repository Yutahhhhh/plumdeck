import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { Download, Loader2, RefreshCw } from "lucide-react";
import { checkJunctionActive } from "@/services/junction/client";
import { createUpdateController } from "@/services/update-controller";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Progress } from "@/components/ui/progress";
import { version } from "../../package.json";

const updates = createUpdateController({
  currentVersion: version,
  check: () => check({ timeout: 15000 }),
  activeSession: checkJunctionActive,
  relaunch,
});
const mb = (bytes: number) => `${(bytes / 1024 / 1024).toFixed(1)} MB`;

export function Updater() {
  const [open, setOpen] = useState(false);
  const state = useSyncExternalStore(updates.subscribe, updates.getSnapshot);
  const native = isTauri();
  const busy = ["checking", "downloading", "installing"].includes(state.phase);
  const manual = useCallback(() => {
    setOpen(true);
    const phase = updates.getSnapshot().phase;
    if (isTauri() && !["checking", "downloading", "installing", "available", "ready"].includes(phase)) void updates.check();
  }, []);
  useEffect(() => {
    if (native) void updates.automaticCheck();
  }, [native]);
  useEffect(() => {
    if (!native) return;
    let live = true;
    let off: (() => void) | undefined;
    void listen("app://check-updates", manual).then(unsubscribe => {
      if (!live) unsubscribe(); else off = unsubscribe;
    }).catch(console.error);
    return () => { live = false; off?.(); };
  }, [native, manual]);
  useEffect(() => {
    if (state.phase === "available") setOpen(true);
  }, [state.phase]);
  return <>
    <Button variant="ghost" size="sm" className="h-7 shrink-0 gap-1.5 whitespace-nowrap px-2 text-xs text-slate-200 hover:bg-slate-700 hover:text-white" onClick={manual} title="バージョン・更新を確認">
      {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : state.phase === "available" ? <Download className="h-3.5 w-3.5" /> : <RefreshCw className="h-3.5 w-3.5" />}
      <span className="hidden lg:inline">{busy ? "更新状況" : state.phase === "available" ? "更新あり" : "更新確認"}</span>
    </Button>
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader><DialogTitle>plumdeckの更新</DialogTitle>
          <DialogDescription>現在のバージョン: v{state.currentVersion}</DialogDescription>
        </DialogHeader>
        {!native ? <p className="text-sm">バージョンの更新はデスクトップアプリから利用できます。</p> : <div className="space-y-4 text-sm" aria-live="polite">
          {state.phase === "checking" && <p>最新版を確認しています…</p>}
          {state.phase === "latest" && <p>最新バージョンを使用しています。</p>}
          {state.version && state.phase !== "latest" && <p>新しいバージョン: v{state.version}</p>}
          {state.notes && <p className="max-h-40 overflow-y-auto whitespace-pre-wrap text-xs text-muted-foreground">{state.notes}</p>}
          {state.phase === "available" && <p>更新を適用するとアプリが再起動します。</p>}
          {state.phase === "downloading" && <div className="space-y-2">
            <p>ダウンロード中 · {mb(state.downloaded)}{state.total ? ` / ${mb(state.total)} (${Math.min(100, Math.floor(state.downloaded / state.total * 100))}%)` : ""}</p>
            {state.total ? <Progress value={Math.min(100, state.downloaded / state.total * 100)} /> : <p className="text-xs text-muted-foreground">全体サイズを取得できないため、受信済み容量を表示しています。</p>}
          </div>}
          {state.phase === "installing" && <p>更新をインストールしています…</p>}
          {state.phase === "ready" && <p>インストールが完了しました。再起動すると更新が反映されます。</p>}
          {state.error && <p role="alert" className="break-words text-destructive">{state.error}</p>}
          {state.phase === "available" && <Button onClick={() => void updates.install()}>更新して再起動</Button>}
          {state.phase === "ready" && <Button onClick={() => void updates.restart()}>再起動</Button>}
          {!busy && !["available", "ready"].includes(state.phase) && <Button variant="outline" onClick={() => void updates.check()}>再確認</Button>}
        </div>}
      </DialogContent>
    </Dialog>
  </>;
}
