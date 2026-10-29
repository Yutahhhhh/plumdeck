import { useEffect, useState } from "react";
import { systemService, DashboardStats } from "@/services/system";
import { StatCards } from "./StatCards";
import { GenreBarChart } from "./GenreBarChart";
import { RecentSetlists } from "./RecentSetlists";
import { Button } from "@/components/ui/button";
import { Loader2, RefreshCw } from "lucide-react";
import { useIngestion } from "@/contexts/IngestionContext";
import { AddMusicButton } from "@/components/import/AddMusicButton";
import { libraryImportService } from "@/services/library-import";
import { toast } from "@/components/ui/toast";
import { AttentionSection } from "./AttentionSection";
import { RekordboxImportCard } from "./RekordboxImportCard";

interface DashboardViewProps {
  onNavigate: (view: string) => void;
}

export function DashboardView({ onNavigate }: DashboardViewProps) {
  const [stats, setStats] = useState<DashboardStats | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const { isAnalyzing } = useIngestion();
  const [refreshKey, setRefreshKey] = useState(0);

  // Load Data
  const fetchData = async () => {
    setIsLoading(true);
    setError(null);
    try {
      const statsData = await systemService.getDashboardStats();
      setStats(statsData);
    } catch (e: any) {
      console.error("Failed to load dashboard data", e);
      setError(e.message || "Failed to load dashboard data");
    } finally {
      setIsLoading(false);
    }
  };

  useEffect(() => {
    fetchData();
  }, [isAnalyzing]); // Refresh when analysis status changes

  // Imports finish in the background; keep the counts current.
  useEffect(() => {
    const refresh = () => { void systemService.getDashboardStats().then(setStats).catch(() => undefined); };
    window.addEventListener("plumdeck:import-tracks-updated", refresh);
    return () => window.removeEventListener("plumdeck:import-tracks-updated", refresh);
  }, []);

  const refreshAll = () => { void fetchData(); setRefreshKey((key) => key + 1); };
  const analyzeUnanalyzed = async () => {
    try {
      const { started } = await libraryImportService.fixAttention("unanalyzed");
      if (started) toast.success(`${started.toLocaleString()}曲を解析しています`, "進み具合は画面右下に表示されます。");
      else toast.info("解析できる曲が見つかりませんでした", "ファイルが移動・削除されていないか確認してください。");
    } catch (cause) {
      toast.error("解析を開始できませんでした", cause instanceof Error ? cause.message : String(cause));
    }
  };

  if (isLoading && !stats) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="flex flex-col items-center gap-2 text-muted-foreground">
          <Loader2 className="h-8 w-8 animate-spin" />
          <p>読み込み中…</p>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="h-full flex items-center justify-center p-4">
        <div className="text-center space-y-4">
          <h2 className="text-xl font-bold">
            ダッシュボードを表示できませんでした
          </h2>
          <p className="text-sm text-muted-foreground">少し待ってから、もう一度お試しください。</p>
          <Button onClick={fetchData}>再試行</Button>
          <details className="text-xs text-muted-foreground">
            <summary className="cursor-pointer">詳細</summary>
            <p className="mt-1 break-all">{error}</p>
          </details>
        </div>
      </div>
    );
  }

  if (!stats) return null;

  return (
    <div className="h-full flex flex-col p-6 space-y-6 overflow-y-auto">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold tracking-tight">Dashboard</h1>
          <p className="text-muted-foreground">
            ライブラリの状態と、やっておくとよいこと。曲やフォルダは画面のどこにでもドロップして追加できます。
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={refreshAll}
            disabled={isLoading}
            aria-label="更新"
          >
            <RefreshCw
              className={`h-4 w-4 ${isLoading ? "animate-spin" : ""}`}
            />
          </Button>
          <AddMusicButton />
        </div>
      </div>

      <RekordboxImportCard refreshKey={refreshKey} />

      <AttentionSection refreshKey={refreshKey} />

      {/* KPI Cards */}
      <StatCards
        stats={stats}
        onNavigate={onNavigate}
        onAnalyze={() => void analyzeUnanalyzed()}
      />

      {/* Main Charts Area */}
      <div className="h-[400px]">
        <GenreBarChart data={stats.genre_distribution} />
      </div>

      {/* Bottom Section */}
      <div className="grid gap-4 md:grid-cols-2 h-[300px]">
        <RecentSetlists
          setlists={stats.recent_setlists}
          onNavigate={onNavigate}
        />

        {/* Quick Actions / Tips Placeholder */}
        <div className="border rounded-xl bg-card p-6 flex flex-col justify-center items-center text-center space-y-4 shadow-sm">
          <h3 className="font-semibold text-lg">Ready to Create?</h3>
          <p className="text-sm text-muted-foreground max-w-xs">
            Start building your next setlist with audio-feature recommendations,
            or ask your connected MCP client for a natural-language workflow.
          </p>
          <Button
            onClick={() => onNavigate("setlists")}
            className="w-full max-w-xs"
          >
            Create Setlist
          </Button>
          <Button
            variant="outline"
            onClick={() => onNavigate("library")}
            className="w-full max-w-xs"
          >
            Browse Library
          </Button>
        </div>
      </div>

    </div>
  );
}
