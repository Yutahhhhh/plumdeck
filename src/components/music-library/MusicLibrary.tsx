import { useState, useRef, useCallback } from "react";
import { Button } from "@/components/ui/button";
import { AddMusicButton } from "@/components/import/AddMusicButton";
import { Input } from "@/components/ui/input";
import { Sparkles, Search, X } from "lucide-react";
import { Track } from "@/types";
import { Badge } from "@/components/ui/badge";
import { FilterDialog } from "./FilterDialog";
import { TrackList } from "./TrackList";
import { FilterState } from "./types";
import { useTrackSearch } from "./useTrackSearch";
import { LIMIT, INITIAL_FILTERS } from "./constants";
import { useIngestion } from "@/contexts/IngestionContext";
import { toast } from "@/components/ui/toast";
import { getErrorDetail } from "@/services/api-client";

interface MusicLibraryProps {
  isPlayerLoading?: boolean;
}

export function MusicLibrary({
  isPlayerLoading,
}: MusicLibraryProps) {
  const [analyzingId, setAnalyzingId] = useState<number | null>(null);
  const { startIngestion, waitForIngestionComplete } = useIngestion();

  const {
    query,
    setQuery,
    tracks,
    loading,
    filters,
    isFilterOpen,
    setIsFilterOpen,
    applyFilters,
    clearAllFilters,
    loadMore,
    hasMore,
    activeFilterCount,
    setTracks,
    search,
    totalCount,
  } = useTrackSearch({ limit: LIMIT });

  const observer = useRef<IntersectionObserver | null>(null);
  const lastTrackElementRef = useCallback(
    (node: HTMLTableRowElement | null) => {
      if (loading) return;
      if (observer.current) observer.current.disconnect();
      observer.current = new IntersectionObserver((entries) => {
        if (entries[0].isIntersecting && hasMore) {
          loadMore();
        }
      });
      if (node) observer.current.observe(node);
    },
    [loading, hasMore, loadMore]
  );

  const clearFilter = (key: keyof FilterState) => {
    const newFilters = { ...filters };
    // Reset specific fields to default
    if (key === "bpm") newFilters.bpm = null;
    else if (key === "key") newFilters.key = "";
    else if (key === "artist") newFilters.artist = "";
    else if (key === "album") newFilters.album = "";
    else if (key === "lyrics") newFilters.lyrics = "";
    else if (key === "genres") newFilters.genres = [];
    else if (key === "subgenres") newFilters.subgenres = [];
    else if (key === "minYear" || key === "maxYear") {
      newFilters.minYear = INITIAL_FILTERS.minYear;
      newFilters.maxYear = INITIAL_FILTERS.maxYear;
    }
    // Reset ranges to full
    else if (key.toString().includes("Energy")) {
      newFilters.minEnergy = 0.0;
      newFilters.maxEnergy = 1.0;
    } else if (key.toString().includes("Danceability")) {
      newFilters.minDanceability = 0.0;
      newFilters.maxDanceability = 1.0;
    } else if (key.toString().includes("Brightness")) {
      newFilters.minBrightness = 0.0;
      newFilters.maxBrightness = 1.0;
    }

    applyFilters(newFilters);
  };

  const handleAnalyze = async (track: Track) => {
    if (analyzingId) return;
    setAnalyzingId(track.id);

    try {
      // バックグラウンド解析を開始し、WebSocket の完了通知を待ってからリストを更新する
      const upgradingLightAnalysis = track.analysis_level === "light";
      await startIngestion([track.filepath], true, upgradingLightAnalysis ? "full" : undefined);
      const outcome = await waitForIngestionComplete();
      if (outcome.type !== "complete" || outcome.errors > 0) {
        throw new Error(outcome.lastError || outcome.message || "解析に失敗しました");
      }
      search(true);
      toast.success(
        upgradingLightAnalysis ? "詳細解析が完了しました" : "解析が完了しました",
        `${track.title || track.filepath}`,
      );
    } catch (error) {
      console.error("Error calling analyze API", error);
      toast.error("解析に失敗しました", getErrorDetail(error));
    } finally {
      setAnalyzingId(null);
    }
  };

  const handleTrackUpdate = (trackId: number, updates: Partial<Track>) => {
    setTracks((prev) =>
      prev.map((t) => (t.id === trackId ? { ...t, ...updates } : t))
    );
  };

  // Helper to count active filters for display logic (hook provides total count, but we need specific checks for badges)
  const isFeatureActive = (min: number, max: number) => min > 0 || max < 1;

  return (
    <div className="h-full min-h-0 flex flex-col p-4 gap-4">
      <div className="flex flex-col gap-4">
        <div className="flex items-center justify-between">
          <h2 className="text-2xl font-bold flex items-center gap-2">
            <Sparkles className="h-6 w-6 text-purple-500" />
            Music Library
          </h2>
          <div className="flex items-center gap-3">
            <div className="text-sm text-muted-foreground">
              {totalCount !== null
                ? `${tracks.length} / ${totalCount} tracks`
                : `${tracks.length} tracks loaded`}
            </div>
            <AddMusicButton />
          </div>
        </div>

        {/* Search & Filter Bar */}
        <div className="flex items-center gap-2">
          <div className="relative flex-1">
            <Search className="absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
            <Input
              placeholder="Search by Title, Artist or Tags..."
              className="pl-9"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            {query && (
              <button
                className="absolute right-2.5 top-2.5 text-muted-foreground hover:text-foreground"
                onClick={() => setQuery("")}
              >
                <X className="h-4 w-4" />
              </button>
            )}
          </div>

          <FilterDialog
            isOpen={isFilterOpen}
            onOpenChange={setIsFilterOpen}
            currentFilters={filters}
            onApply={applyFilters}
          />
        </div>

        {/* Active Filters Display */}
        {activeFilterCount > 0 && (
          <div className="flex flex-wrap gap-2 items-center">
            <span className="text-xs text-muted-foreground mr-1">Active:</span>

            {filters.bpm && (
              <Badge variant="secondary" className="gap-1">
                BPM: {filters.bpm}
                <X
                  className="h-3 w-3 cursor-pointer"
                  onClick={() => clearFilter("bpm")}
                />
              </Badge>
            )}
            {/* Key Badge */}
            {filters.key && (
              <Badge
                variant="secondary"
                className="gap-1 bg-green-100 text-green-800 hover:bg-green-200"
              >
                Key: {filters.key}
                <X
                  className="h-3 w-3 cursor-pointer"
                  onClick={() => clearFilter("key")}
                />
              </Badge>
            )}

            {filters.artist && (
              <Badge variant="secondary" className="gap-1">
                Artist: {filters.artist}
                <X
                  className="h-3 w-3 cursor-pointer"
                  onClick={() => clearFilter("artist")}
                />
              </Badge>
            )}
            {filters.album && (
              <Badge variant="secondary" className="gap-1">
                Album: {filters.album}
                <X
                  className="h-3 w-3 cursor-pointer"
                  onClick={() => clearFilter("album")}
                />
              </Badge>
            )}
            {filters.lyrics && (
              <Badge variant="secondary" className="gap-1">
                Lyrics: {filters.lyrics}
                <X
                  className="h-3 w-3 cursor-pointer"
                  onClick={() => clearFilter("lyrics")}
                />
              </Badge>
            )}
            {filters.genres && filters.genres.length > 0 && (
              <Badge variant="secondary" className="gap-1">
                Genres: {filters.genres.slice(0, 3).join(", ")}
                {filters.genres.length > 3 && ` +${filters.genres.length - 3}`}
                <X
                  className="h-3 w-3 cursor-pointer"
                  onClick={() => clearFilter("genres")}
                />
              </Badge>
            )}
            {filters.subgenres && filters.subgenres.length > 0 && (
              <Badge variant="secondary" className="gap-1">
                Subgenres: {filters.subgenres.slice(0, 3).join(", ")}
                {filters.subgenres.length > 3 &&
                  ` +${filters.subgenres.length - 3}`}
                <X
                  className="h-3 w-3 cursor-pointer"
                  onClick={() => clearFilter("subgenres")}
                />
              </Badge>
            )}

            {/* Feature Badges */}
            {isFeatureActive(filters.minEnergy, filters.maxEnergy) && (
              <Badge variant="secondary" className="gap-1">
                Energy: {filters.minEnergy} - {filters.maxEnergy}
                <X
                  className="h-3 w-3 cursor-pointer"
                  onClick={() => clearFilter("minEnergy")}
                />
              </Badge>
            )}
            {isFeatureActive(
              filters.minDanceability,
              filters.maxDanceability
            ) && (
              <Badge variant="secondary" className="gap-1">
                Dance: {filters.minDanceability} - {filters.maxDanceability}
                <X
                  className="h-3 w-3 cursor-pointer"
                  onClick={() => clearFilter("minDanceability")}
                />
              </Badge>
            )}
            {isFeatureActive(filters.minBrightness, filters.maxBrightness) && (
              <Badge variant="secondary" className="gap-1">
                Bright: {filters.minBrightness} - {filters.maxBrightness}
                <X
                  className="h-3 w-3 cursor-pointer"
                  onClick={() => clearFilter("minBrightness")}
                />
              </Badge>
            )}

            <Button
              variant="ghost"
              size="sm"
              className="h-6 text-xs"
              onClick={clearAllFilters}
            >
              Clear All
            </Button>
          </div>
        )}
      </div>

      {/* Track List */}
      <TrackList
        tracks={tracks}
        isLoading={loading && tracks.length === 0}
        isLoadingMore={loading && tracks.length > 0}
        lastTrackElementRef={lastTrackElementRef}
        analyzingId={analyzingId}
        onAnalyze={handleAnalyze}
        disabled={isPlayerLoading}
        onTrackUpdate={handleTrackUpdate}
        hasMore={hasMore}
        onLoadMore={loadMore}
      />
    </div>
  );
}
