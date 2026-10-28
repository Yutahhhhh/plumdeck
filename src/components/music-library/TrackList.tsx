import { ScrollArea } from "@/components/ui/scroll-area";
import { Loader2, RefreshCw } from "lucide-react";
import { Track } from "@/types";
import { TrackRow } from "@/components/track-row";
import { Button } from "@/components/ui/button";
import {
  TooltipProvider,
} from "@/components/ui/tooltip";

interface TrackListProps {
  tracks: Track[];
  isLoading?: boolean;
  isLoadingMore?: boolean;
  lastTrackElementRef: (node: HTMLTableRowElement | null) => void;
  analyzingId: number | null;
  onAnalyze: (track: Track) => void;
  disabled?: boolean;
  onTrackUpdate?: (trackId: number, updates: Partial<Track>) => void;
  hasMore?: boolean;
  onLoadMore?: () => void;
}

export function TrackList({
  tracks,
  isLoading = false,
  isLoadingMore = false,
  lastTrackElementRef,
  analyzingId,
  onAnalyze,
  disabled = false,
  onTrackUpdate,
  hasMore = false,
  onLoadMore,
}: TrackListProps) {
  return (
    <div className="flex-1 min-h-0 border rounded-md overflow-hidden bg-card relative">
      {isLoading && (
        <div className="absolute inset-0 bg-background/50 backdrop-blur-sm z-50 flex items-center justify-center">
          <div className="flex flex-col items-center gap-2">
            <Loader2 className="h-8 w-8 animate-spin text-primary" />
            <p className="text-sm text-muted-foreground">Loading tracks...</p>
          </div>
        </div>
      )}
      <ScrollArea className="h-full">
        <div className="p-0">
          <table className="w-full text-sm text-left">
            <thead className="text-muted-foreground border-b sticky top-0 bg-background z-10">
              <tr>
                <th className="p-3 font-medium w-10"></th>
                <th className="p-3 font-medium min-w-[200px]">
                  Title / Artist
                </th>
                <th className="p-3 font-medium w-20">Year</th>
                <th className="p-3 font-medium w-[120px]">Genre</th>
                <th className="p-3 font-medium text-right w-20">BPM</th>
                <th className="p-3 font-medium text-center w-20">Key</th>
                <th className="p-3 font-medium text-center w-20">Energy</th>
                <th className="p-3 font-medium text-center w-20">Dance</th>
                <th className="p-3 font-medium w-[120px]">Subgenre</th>
                <th className="p-3 font-medium w-[100px]">Action</th>
              </tr>
            </thead>
            <tbody>
              <TooltipProvider>
                {tracks.map((track, index) => {
                  const isUnanalyzed = track.is_analyzed == null ? !track.bpm || track.bpm === 0 : !track.is_analyzed;

                  return (
                    <TrackRow
                      key={track.id}
                      track={track}
                      viewType="table"
                      isUnanalyzed={isUnanalyzed}
                      disabled={disabled}
                      onTrackUpdate={onTrackUpdate}
                      innerRef={tracks.length === index + 1 ? lastTrackElementRef : null}
                      suffix={
                        <div className="flex justify-center">
                          <Button
                            variant="ghost"
                            size={track.analysis_level === "light" ? "sm" : "icon"}
                            title={track.analysis_level === "light" ? "詳細解析する" : "再解析"}
                            aria-label={track.analysis_level === "light" ? "詳細解析する" : "再解析"}
                            onClick={(e) => {
                              e.stopPropagation();
                              onAnalyze(track);
                            }}
                            disabled={analyzingId === track.id}
                          >
                            {analyzingId === track.id ? (
                              <Loader2 className="h-4 w-4 animate-spin" />
                            ) : (
                              <RefreshCw className="h-4 w-4 text-muted-foreground" />
                            )}
                            {track.analysis_level === "light" && <span className="text-xs">詳細</span>}
                          </Button>
                        </div>
                      }
                    />
                  );
                })}
                {hasMore && onLoadMore && (
                  <tr>
                    <td colSpan={10} className="p-3 text-center">
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={onLoadMore}
                        disabled={isLoadingMore}
                        className="gap-2"
                      >
                        {isLoadingMore && (
                          <Loader2 className="h-4 w-4 animate-spin" />
                        )}
                        Load more
                      </Button>
                    </td>
                  </tr>
                )}
              </TooltipProvider>
            </tbody>
          </table>
        </div>
      </ScrollArea>
    </div>
  );
}
