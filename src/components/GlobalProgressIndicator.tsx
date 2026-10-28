import { useState, useEffect } from "react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import {
  Loader2,
  CheckCircle2,
  Activity,
  StopCircle,
  FileAudio,
  Music,
  AlertCircle,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { useIngestion } from "@/contexts/IngestionContext";
import { useMetadata } from "@/contexts/MetadataContext";
import { ImportQueueProgress } from "./ImportQueueProgress";

export function GlobalProgressIndicator() {
  const ingestion = useIngestion();
  const metadata = useMetadata();
  return <>
    <ImportQueueProgress raised={ingestion.isAnalyzing || ingestion.showComplete || metadata.isUpdating} />
    <LegacyProgressIndicator />
  </>;
}

function LegacyProgressIndicator() {
  const {
    isAnalyzing: isIngesting,
    progress: ingestProgress,
    statusText: ingestStatus,
    currentFile: ingestFile,
    stats: ingestStats,
    showComplete: ingestComplete,
    cancelIngestion,
    dismissComplete: dismissIngestComplete,
    lastError, elapsedSeconds, activeFiles, connectionError,
    analysisProfile, effectiveAnalysisProfile, queued,
  } = useIngestion();

  const {
    isUpdating: isMetadataUpdating,
    progress: metadataProgress,
    statusText: metadataStatus,
    currentTrack: metadataTrack,
    stats: metadataStats,
    cancelUpdate: cancelMetadataUpdate,
  } = useMetadata();

  const [isOpen, setIsOpen] = useState(false);

  // Determine active task
  const isWorking = isIngesting || isMetadataUpdating;
  const showComplete = ingestComplete; // Metadata doesn't have explicit complete state yet

  // Derived values based on priority (Ingestion > Metadata)
  const activeType = isIngesting ? "ingestion" : isMetadataUpdating ? "metadata" : null;
  
  const showIngestion = isIngesting || ingestComplete && !isMetadataUpdating;
  const hasIngestionError = showIngestion && Boolean(lastError || ingestStats.errors);
  const progress = showIngestion ? ingestProgress : metadataProgress;
  const statusText = showIngestion ? ingestStatus : metadataStatus;
  const currentItem = showIngestion ? ingestFile : metadataTrack;
  const stats = showIngestion ? ingestStats : metadataStats;

  // Auto-close logic when complete
  useEffect(() => {
    if (showComplete && !lastError && !ingestStats.errors) {
      const timer = setTimeout(() => {
        setIsOpen(false);
        dismissIngestComplete();
      }, 5000);
      return () => clearTimeout(timer);
    }
  }, [showComplete, dismissIngestComplete, lastError, ingestStats.errors]);

  const getFileName = (path: string) => {
    if (!path) return "";
    return path.split(/[/\\]/).pop() || path;
  };

  const handleCancel = async () => {
    if (isIngesting) {
      await cancelIngestion();
    } else if (isMetadataUpdating) {
      await cancelMetadataUpdate();
    }
    setIsOpen(false);
  };

  // 表示すべき状態でない場合は何もレンダリングしない
  if (!isWorking && !showComplete && !isOpen) {
    return null;
  }

  const title = showIngestion ? "音源解析" : "メタデータ更新";
  const description = showIngestion
    ? queued
      ? "別の解析が終わるまで待機しています。完了すると自動で開始します。"
      : effectiveAnalysisProfile === "light"
      ? "再生に必要なBPM、キー、基本情報を軽量解析しています。"
      : "BPM、キー、音色と波形を解析しています。音色の比較用データは取り込み後に裏で計算します。"
    : "曲のメタデータを更新しています。";

  return (
    <>
      {/* Floating Action Button (FAB) */}
      <div className="fixed bottom-6 right-6 z-50">
        <Button
          variant="outline"
          onClick={() => setIsOpen(true)}
          className={cn(
            "h-12 rounded-full shadow-lg border pl-3 pr-5 gap-3 transition-all duration-300",
            isWorking
              ? "bg-background/80 backdrop-blur-md hover:bg-background/90 border-primary/20"
              : showComplete
              ? hasIngestionError
                ? "bg-destructive hover:bg-destructive/90 text-destructive-foreground border-destructive"
                : "bg-green-500 hover:bg-green-600 text-white border-green-600"
              : "bg-background"
          )}
        >
          {isWorking ? (
            <>
              <div className="relative flex items-center justify-center">
                <Loader2 className="h-5 w-5 animate-spin text-primary" />
              </div>
              <div className="flex flex-col items-start text-xs leading-none gap-0.5">
                <span className="font-semibold">{activeType === "ingestion"
                  ? stats.total > 0 ? `${Math.max(0, stats.total - stats.current)}曲の解析が進行中` : ingestStatus
                  : "更新中"}</span>
                <span className="text-muted-foreground tabular-nums">
                  {Math.round(progress)}% • {stats.current}/{stats.total}
                </span>
              </div>
            </>
          ) : showComplete ? (
            <>
              {hasIngestionError ? <AlertCircle className="h-5 w-5" /> : <CheckCircle2 className="h-5 w-5" />}
              <span className="font-medium">{lastError || ingestStats.errors ? "解析結果・エラーあり" : "解析完了"}</span>
            </>
          ) : (
            <Activity className="h-5 w-5" />
          )}
        </Button>
      </div>

      {/* Detail Dialog */}
      <Dialog open={isOpen} onOpenChange={setIsOpen}>
        <DialogContent className="sm:max-w-xl">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              {isWorking ? (
                <>
                  <Activity className="h-5 w-5 text-primary animate-pulse" />
                  {title}
                </>
              ) : showComplete ? (
                <>
                  {hasIngestionError
                    ? <AlertCircle className="h-5 w-5 text-destructive" />
                    : <CheckCircle2 className="h-5 w-5 text-green-500" />}
                  {lastError || ingestStats.errors ? "解析結果・エラーあり" : "解析完了"}
                </>
              ) : (
                "Task Status"
              )}
            </DialogTitle>
            <DialogDescription>
              {isWorking ? description : "解析結果を確認できます。"}
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-6 py-4">
            {showIngestion && <div className="space-y-2 text-sm">
              <p className="text-xs text-muted-foreground">
                解析方法：{queued ? "開始待ち" : effectiveAnalysisProfile === "light" ? "軽量（低負荷）" : analysisProfile === "auto" ? "自動・詳細解析中" : "詳細"}
              </p>
              <p>経過 {Math.floor(elapsedSeconds / 60)}分{elapsedSeconds % 60}秒 · 成功 {ingestStats.processed} · スキップ {ingestStats.skipped} · 失敗 {ingestStats.errors}</p>
              {activeFiles.map(file => <div key={file.path} className="space-y-1 rounded border p-2" role="status">
                <p className="break-all">{getFileName(file.path)} · {file.seconds}秒</p>
                <p className="text-xs text-muted-foreground">{file.stage}</p>
              </div>)}
              {activeFiles.some(file => file.seconds > 120) && <p className="text-amber-600">解析に時間がかかっています。音源解析ワーカーにはタイムアウトがあり、失敗理由はここに表示されます。</p>}
              {(connectionError || lastError) && <p role="alert" className="break-all text-destructive">{connectionError || lastError}</p>}
            </div>}
            {/* Progress Bar */}
            <div className="space-y-2">
              <div className="flex justify-between text-xs text-muted-foreground">
                <span>Progress</span>
                <span>
                  {stats.current} / {stats.total} {showIngestion ? "曲" : "トラック"}
                </span>
              </div>
              <Progress value={progress} className="h-2" />
            </div>

            {/* Current File Info */}
            <div className="bg-muted/50 rounded-lg p-3 space-y-2 border overflow-hidden">
              <div className="flex items-start gap-3">
                <div className="h-8 w-8 bg-background rounded-full flex items-center justify-center shrink-0 border">
                  {isWorking ? (
                    <Loader2 className="h-4 w-4 animate-spin text-primary" />
                  ) : (
                    showIngestion ? <FileAudio className="h-4 w-4 text-muted-foreground" /> : <Music className="h-4 w-4 text-muted-foreground" />
                  )}
                </div>
                <div className="flex-1 min-w-0 grid gap-0.5">
                  <p className="text-sm font-medium truncate" title={getFileName(currentItem)}>
                    {getFileName(currentItem) || (showIngestion ? queued ? "開始待ち" : "解析対象を確認中" : "待機中")}
                  </p>
                  <p className="text-xs text-muted-foreground truncate">
                    {statusText}
                  </p>
                </div>
              </div>
            </div>

            {/* Stats Grid */}
            <div className="grid grid-cols-2 gap-4">
              <div className="p-3 border rounded-md text-center">
                <div className="text-2xl font-bold">{stats.processed}</div>
                <div className="text-xs text-muted-foreground">Processed</div>
              </div>
              <div className="p-3 border rounded-md text-center">
                <div className="text-2xl font-bold">
                  {Math.max(0, stats.total - stats.current)}
                </div>
                <div className="text-xs text-muted-foreground">Remaining</div>
              </div>
            </div>
          </div>

          <DialogFooter className="sm:justify-between gap-2">
            {isWorking ? (
              <Button
                variant="destructive"
                onClick={handleCancel}
                className="w-full sm:w-auto"
              >
                <StopCircle className="h-4 w-4 mr-2" />
                {activeType === "ingestion" ? "解析を停止" : "更新を停止"}
              </Button>
            ) : (
              <Button
                onClick={() => { setIsOpen(false); dismissIngestComplete(); }}
                className="w-full sm:w-auto ml-auto"
              >
                Close
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
