import { useState, useEffect, useCallback } from "react";
import { MetadataAnalysisResult } from "@/services/settings";
import { AnalysisDialog } from "./AnalysisDialog";
import { TrackList } from "@/components/music-library/TrackList";
import { Track } from "@/types";

interface MetadataImportDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  analysis: MetadataAnalysisResult | null;
  isAnalyzing: boolean;
  isImporting: boolean;
  onExecute: () => void;
}

/** Rows as they will look after the import (unchanged fields keep the current value). */
function previewTracks(analysis: MetadataAnalysisResult): Track[] {
  return analysis.updates.map((u, idx) => ({
    id: idx,
    filepath: u.filepath,
    title: u.new.title ?? u.current.title,
    artist: u.new.artist ?? u.current.artist,
    album: u.new.album ?? u.current.album,
    genre: u.new.genre ?? u.current.genre,
    subgenre: u.new.subgenre ?? u.current.subgenre,
    year: u.new.year ?? u.current.year,
    is_genre_verified: u.new.is_genre_verified ?? u.current.is_genre_verified,
    bpm: u.new.bpm ?? u.current.bpm,
    key: u.new.key || u.current.key,
    duration: 0,
    energy: 0,
    danceability: 0,
    brightness: 0,
    contrast: 0,
    noisiness: 0,
    loudness: 0,
  }));
}

export function MetadataImportDialog({
  open,
  onOpenChange,
  analysis,
  isAnalyzing,
  isImporting,
  onExecute,
}: MetadataImportDialogProps) {
  const [visibleTracks, setVisibleTracks] = useState<Track[]>([]);
  const [page, setPage] = useState(1);
  const ITEMS_PER_PAGE = 50;

  const canExecute = !!analysis?.updates.length;

  useEffect(() => {
    if (analysis?.updates) {
      const allTracks = previewTracks(analysis);

      // Reset pagination
      setPage(1);
      setVisibleTracks(allTracks.slice(0, ITEMS_PER_PAGE));
    }
  }, [analysis]);

  const loadMoreTracks = useCallback(() => {
    if (!analysis?.updates) return;

    const allTracks = previewTracks(analysis);

    const nextPage = page + 1;
    const nextTracks = allTracks.slice(0, nextPage * ITEMS_PER_PAGE);

    if (nextTracks.length > visibleTracks.length) {
      setVisibleTracks(nextTracks);
      setPage(nextPage);
    }
  }, [analysis, page, visibleTracks.length]);

  const lastTrackElementRef = useCallback(
    (node: HTMLTableRowElement | null) => {
      if (node) {
        const observer = new IntersectionObserver((entries) => {
          if (entries[0].isIntersecting) {
            loadMoreTracks();
          }
        });
        observer.observe(node);
        return () => observer.disconnect();
      }
    },
    [loadMoreTracks]
  );

  return (
    <AnalysisDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Metadata Update Analysis"
      description="Review metadata changes. Only fields with differences are shown."
      isAnalyzing={isAnalyzing}
      isImporting={isImporting}
      onExecute={onExecute}
      canExecute={canExecute}
      className="max-w-4xl"
    >
      {analysis && (
        <div className="flex-1 overflow-hidden flex flex-col gap-4">
          <div className="grid grid-cols-2 gap-4 text-center">
            <div className="p-3 bg-blue-50 rounded-lg border border-blue-100">
              <div className="text-2xl font-bold text-blue-700">
                {analysis.updates.length}
              </div>
              <div className="text-xs text-blue-600 font-medium">
                Tracks to Update
              </div>
            </div>
            <div className="p-3 bg-yellow-50 rounded-lg border border-yellow-100">
              <div className="text-2xl font-bold text-yellow-700">
                {analysis.not_found.length}
              </div>
              <div className="text-xs text-yellow-600 font-medium">
                Not Found (Skipped)
              </div>
            </div>
          </div>

          {analysis.updates.length > 0 && (
            <div className="flex-1 border rounded-md overflow-hidden bg-background">
              <TrackList
                tracks={visibleTracks}
                lastTrackElementRef={lastTrackElementRef}
                analyzingId={null}
                onAnalyze={() => {}}
              />
            </div>
          )}
        </div>
      )}
    </AnalysisDialog>
  );
}
