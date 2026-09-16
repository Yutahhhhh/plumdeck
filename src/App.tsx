import { useJunction } from '@/hooks/useJunction';
import { JunctionBar } from '@/components/junction/JunctionBar';
import { junctionState } from '@/services/junction/state';
import { useState, useEffect, useRef } from "react";
import { DocsView } from "@/components/docs/DocsView";
import { Sidebar } from "@/components/sidebar";
import { MusicLibrary } from "@/components/music-library";
import { SettingsView } from "@/components/settings-view";
import { McpView } from "@/components/mcp-view/McpView";
import { FileExplorer } from "@/components/file-explorer";
import { SetlistCreator } from "@/components/setlist-creator";
import { TagManager } from "@/components/tag-manager/TagManager";
import { MusicPlayer } from "@/components/MusicPlayer";
import { GlobalProgressIndicator } from "@/components/GlobalProgressIndicator";
import { IngestionProvider } from "@/contexts/IngestionContext";
import { MetadataProvider } from "@/contexts/MetadataContext";
import { DashboardView } from "@/components/dashboard/DashboardView";
import { useStartupProgress } from "@/hooks/useStartupProgress";
import { LoadingScreen } from "@/components/LoadingScreen";
import { Updater } from "@/components/Updater";
import { Toaster } from "@/components/ui/toast";
import { usePlayerStore } from "@/stores/playerStore";
import { WordplayView } from "@/components/wordplay";
import { releasePerformanceHardware, suspendPerformanceAudio, type ReleaseOutcome } from "@/services/performance-release";
import { isAppMode } from "@/components/play/ModeToggle";
import { AssistWorkspace } from "@/components/assist/AssistWorkspace";
import { ModeToggle, PlayWorkspace, type AppMode } from "@/components/play";
import { WorkflowView } from "@/components/workflows/WorkflowView";

function App() {
  const junction = useJunction();
  const [appMode, setAppMode] = useState<AppMode>(() => {
    const saved = sessionStorage.getItem("plumdeck.appMode");
    return isAppMode(saved) ? saved : "analysis";
  });
  const [activeView, setActiveView] = useState(() =>
    sessionStorage.getItem("plumdeck.activeView") ?? "dashboard"
  );
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const startup = useStartupProgress();
  const previousMode = useRef(appMode);
  const [releasingPerformanceAudio, setReleasingPerformanceAudio] = useState(appMode === "assist");
  const [releaseError, setReleaseError] = useState<string | null>(null);
  const [releaseRetry, setReleaseRetry] = useState(0);
  const releaseTask = useRef<Promise<ReleaseOutcome> | null>(null);
  const releaseAction = useRef(releasePerformanceHardware);

  // Music Player State
  const { currentTrack, pause } = usePlayerStore();
  const [isPlayerLoading, setIsPlayerLoading] = useState(false);

  useEffect(() => {
    sessionStorage.setItem("plumdeck.activeView", activeView);
  }, [activeView]);

  useEffect(() => {
    sessionStorage.setItem("plumdeck.appMode", appMode);
    const from = previousMode.current;
    previousMode.current = appMode;
    if (appMode !== "analysis") pause();
    if (appMode === "assist") {
      releaseAction.current = releasePerformanceHardware;
    } else if (from === "play" && appMode === "analysis") {
      releaseAction.current = suspendPerformanceAudio;
    } else if (!releaseTask.current && releaseRetry === 0) {
      return;
    }
    let live = true;
    setReleasingPerformanceAudio(true);
    setReleaseError(null);
    // StrictMode can remount the effect while a release is pending. Reuse it
    // so two engine stops cannot race with each other or with a new session.
    const task = releaseTask.current ?? releaseAction.current();
    releaseTask.current = task;
    void task.then((outcome) => {
      if (!live) return;
      if (!outcome.audioReleased || !outcome.midiReleased) {
        setReleaseError(outcome.problems.join("\n") || "オーディオとコントローラーの解放を確認できませんでした");
      } else if (outcome.problems.length) {
        console.warn("Performance release:", outcome.problems);
      }
    }).catch((failure) => {
      if (live) setReleaseError(String(failure));
    }).finally(() => {
      if (releaseTask.current === task) releaseTask.current = null;
      if (live) setReleasingPerformanceAudio(false);
    });
    return () => { live = false; };
  }, [appMode, pause, releaseRetry]);

  const changeMode = (mode: AppMode) => {
    if (mode === appMode || releasingPerformanceAudio) return;
    if (mode === "assist" && junctionState.active()) { setReleaseError("Junctionから退出またはセッションを終了してからAssistへ切り替えてください。"); return; }
    if (mode !== "analysis") pause();
    if (appMode === "play" || mode === "assist") setReleasingPerformanceAudio(true);
    setReleaseError(null);
    setReleaseRetry(0);
    setAppMode(mode);
  };

  if (!startup.ready) {
    return <LoadingScreen progress={startup.progress} seconds={startup.seconds} connectionError={startup.connectionError} onRetry={startup.retry} />;
  }

  const renderView = () => {
    switch (activeView) {
      case "dashboard":
        return <DashboardView onNavigate={setActiveView} />;
      case "library":
        return (
          <MusicLibrary
            isPlayerLoading={isPlayerLoading}
          />
        );
      case "setlists":
        return (
          <SetlistCreator />
        );
      case "explorer":
        return <FileExplorer />;
      case "tags":
        return <TagManager />;
      case "mcp":
        return <McpView />;
      case "wordplay":
        return <WordplayView />;
      case "docs":
        return <DocsView />;
      case "settings":
        return <SettingsView />;
      case "workflows":
        return <WorkflowView />;
      default:
        return <DashboardView onNavigate={setActiveView} />;
    }
  };

  return (
    <IngestionProvider>
      <MetadataProvider>
        <div className="flex h-screen min-h-0 flex-col overflow-hidden bg-[#080b11]">
          <div className="z-[80] flex h-10 shrink-0 items-center border-b border-slate-700 bg-[#11151d] px-3 shadow-md">
            <span className="shrink-0 text-[10px] font-bold uppercase tracking-[0.22em] text-slate-500">plumdeck<span className="hidden sm:inline"> Workspace</span></span>
            <JunctionBar showRootButton={appMode === "play"} />
            <div className="ml-auto flex items-center gap-2"><Updater /><ModeToggle mode={appMode} onChange={changeMode} disabled={releasingPerformanceAudio} /></div>
          </div>
        <div className="min-h-0 flex-1">
        {(appMode === "play" || junction?.active) && <div className={appMode === "play" && !releasingPerformanceAudio && !releaseError ? "h-full" : "hidden"}><PlayWorkspace /></div>}
        {releasingPerformanceAudio || releaseError ? <div className="flex h-full flex-col items-center justify-center gap-4 p-6 text-center text-sm text-slate-300" role="status">
          {releasingPerformanceAudio ? <p>オーディオとコントローラーを解放しています…</p> : <>
            <p className="whitespace-pre-wrap text-amber-200">{releaseError}</p>
            <button className="rounded border border-slate-600 px-4 py-2 hover:bg-slate-800" onClick={() => setReleaseRetry((value) => value + 1)}>再試行</button>
          </>}
        </div> : appMode === "play" ? null : appMode === "assist" ? <AssistWorkspace /> : <div className="h-full w-full bg-background text-foreground flex overflow-hidden">
        <Sidebar
          activeView={activeView}
          onNavigate={setActiveView}
          isOpen={sidebarOpen}
          toggleSidebar={() => setSidebarOpen(!sidebarOpen)}
        />
        <main className="flex-1 overflow-hidden relative flex flex-col">
          <div className="flex-1 overflow-hidden relative">{renderView()}</div>

          {/* Spacer for Music Player when active to prevent content overlap */}
          {currentTrack && !releasingPerformanceAudio && <div className="h-24 shrink-0" />}
        </main>

        {/* Global Components */}

        {!releasingPerformanceAudio && (
          <MusicPlayer onLoadingChange={setIsPlayerLoading} />
        )}
        <Toaster />
      </div>}
        </div>
        <GlobalProgressIndicator />
        </div>
      </MetadataProvider>
    </IngestionProvider>
  );
}

export default App;
