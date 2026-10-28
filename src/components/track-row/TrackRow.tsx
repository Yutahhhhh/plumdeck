import React from "react";
import { Track } from "@/types";
import { cn, formatDuration } from "@/lib/utils";
import { TrackPlayButton, TrackTitle, TrackMetaCell, TrackAttributeCell } from "./TrackParts";
import { GenreCell } from "@/components/music-library/GenreCell";
import { usePlayerStore } from "@/stores/playerStore";
import { Mic } from "lucide-react";
import { formatKey, useKeyDisplay } from "@/lib/key-display";

export type TrackRowViewType = "table" | "list";

export interface TrackRowProps {
  track: Track;
  viewType: TrackRowViewType;
  
  // State
  isSelected?: boolean;
  isUnanalyzed?: boolean;
  disabled?: boolean;
  
  // Actions
  onClick?: () => void;
  onDoubleClick?: () => void;
  onTrackUpdate?: (trackId: number, updates: Partial<Track>) => void;
  
  // Slots
  prefix?: React.ReactNode; // Drag handle, Checkbox etc.
  suffix?: React.ReactNode; // Action menu, Delete button etc.
  
  // Refs & Styles (for DnD)
  innerRef?: React.Ref<any>;
  style?: React.CSSProperties;
  className?: string;
  
  // Table specific (optional)
  showMeta?: boolean; // Show BPM, Key etc in list mode?
}

export function TrackRow({
  track,
  viewType,
  isSelected = false,
  isUnanalyzed = false,
  disabled = false,
  onClick,
  onDoubleClick,
  onTrackUpdate,
  prefix,
  suffix,
  innerRef,
  style,
  className,
  showMeta = true,
}: TrackRowProps) {
  const { currentTrack, play } = usePlayerStore();
  const keyDisplay = useKeyDisplay();
  const isPlaying = currentTrack?.id === track.id;
  
  const commonClasses = cn(
    "transition-colors",
    isPlaying && "bg-accent/50",
    isUnanalyzed && "bg-yellow-50/5",
    !isPlaying && !isUnanalyzed && "hover:bg-muted/50",
    isSelected && "bg-accent text-accent-foreground",
    disabled && "opacity-50 cursor-not-allowed",
    className
  );

  const handleDoubleClick = () => {
    if (!disabled) {
        play(track);
        onDoubleClick?.();
    }
  };

  // --- Table View (Render as <tr>) ---
  if (viewType === "table") {
    return (
      <tr
        ref={innerRef}
        style={style}
        className={cn("border-b", commonClasses)}
        onClick={onClick}
        onDoubleClick={handleDoubleClick}
      >
        <td className="p-2 text-center w-10">
          {prefix}
          {!prefix && (
            <TrackPlayButton 
              track={track} 
              disabled={disabled} 
            />
          )}
        </td>
        <td className="p-3 max-w-[200px]">
          <TrackTitle track={track} />
        </td>
        <td className="p-3 w-20">
          <TrackMetaCell value={track.year} />
        </td>
        <td className="p-3 w-[120px]">
          {onTrackUpdate ? (
            <GenreCell 
              track={track} 
              onUpdate={(id, genre) => onTrackUpdate(id, { genre, is_genre_verified: true })} 
            />
          ) : (
            <div className="truncate text-sm" title={track.genre}>
              {track.genre || "-"}
            </div>
          )}
        </td>
        <td className="p-3 w-20">
          <TrackMetaCell value={track.bpm ? Math.round(track.bpm) : "-"} align="right" />
        </td>
        <td className="p-3 w-20">
          <TrackMetaCell value={formatKey(track.key, keyDisplay.notation)} align="center" />
        </td>
        <td className="p-3 w-24">
          <TrackAttributeCell value={track.energy} color="bg-orange-500" />
        </td>
        <td className="p-3 w-24">
          <TrackAttributeCell value={track.danceability} color="bg-sky-500" />
        </td>
        <td className="p-3 w-[120px]">
          <div className="truncate text-xs text-muted-foreground" title={track.subgenre}>
            {track.subgenre || "-"}
          </div>
        </td>
        <td className="p-3 w-[100px]">
          {suffix}
        </td>
      </tr>
    );
  }

  // --- List View (Render as <div>) ---
  return (
    <div
      ref={innerRef}
      style={style}
      className={cn(
        "group relative flex items-center gap-2 p-2 rounded-md border bg-card hover:bg-accent/50 transition-all w-full overflow-hidden text-sm",
        isPlaying && "bg-accent border-primary/30 shadow-sm",
        isUnanalyzed && "bg-yellow-50/5 border-yellow-500/20",
        disabled && "opacity-50 cursor-not-allowed",
        className
      )}
      onClick={onClick}
      onDoubleClick={handleDoubleClick}
    >
      {/* Prefix (Drag Handle etc) */}
      {prefix && <div className="shrink-0 text-muted-foreground">{prefix}</div>}

      {/* Play Button */}
      <div className="shrink-0">
        <TrackPlayButton 
          track={track} 
          disabled={disabled} 
        />
      </div>

      {/* Title & Artist & Genre Area (Flex 1) */}
      <div className="flex-1 min-w-0 flex flex-col justify-center gap-0.5 mr-2">
        {/* Row 1: Title */}
        <div className="flex items-center gap-1">
            {track.has_lyrics && (
                <Mic className="h-3 w-3 text-blue-500 shrink-0" aria-label="Has lyrics" />
            )}
            <span className={cn(
                "font-medium truncate text-sm leading-tight",
                isPlaying && "text-primary"
            )} title={track.title}>
                {track.title}
            </span>
            {track.analysis_level === "light" && (
              <span className="shrink-0 rounded bg-amber-500/15 px-1 py-0.5 text-[9px] font-medium text-amber-700 dark:text-amber-300" title="再生に必要な情報だけを解析済み">
                軽量
              </span>
            )}
        </div>
        
        {/* Row 2: Artist */}
        <span className="text-xs text-muted-foreground truncate leading-tight" title={track.artist}>
            {track.artist}
        </span>

        {/* Row 3: Genre / Subgenre */}
        {(track.genre || track.subgenre) && track.genre !== "Unknown" && (
            <div className="text-[10px] text-muted-foreground/70 truncate leading-none mt-0.5 font-medium">
                {track.genre}
                {track.subgenre && <span className="opacity-70"> / {track.subgenre}</span>}
            </div>
        )}
      </div>

      {/* Right Side Stats */}
      {showMeta && (
        <div className="shrink-0 flex items-start gap-2 text-right ml-2">
            {/* Text Metrics: BPM, Key, Duration (Vertical Stack) */}
            <div className="flex flex-col items-end justify-center gap-0.5 min-w-[2.5rem] self-center">
                {track.bpm ? (
                    <span className="text-[10px] font-mono font-medium text-foreground/90 leading-none">{Math.round(track.bpm)}</span>
                ) : <span className="text-[10px] text-muted-foreground leading-none">-</span>}
                
                {track.key ? (
                    <span className="text-[9px] text-muted-foreground leading-none">{formatKey(track.key, keyDisplay.notation)}</span>
                ) : <span className="text-[9px] text-muted-foreground leading-none">-</span>}
                
                <span className="text-[9px] font-mono text-muted-foreground/50 leading-none">
                    {formatDuration(track.duration)}
                </span>
            </div>

            {/* Bars Stack */}
            <div className="flex flex-col items-center justify-end min-w-[1.5rem] self-end">
                {/* Vertical Bars: Energy / Dance */}
                {/* Max height limited to h-5 (approx 50-60% of row) to avoid overlap with Year badge at top-right */}
                <div className="flex items-end gap-1 h-5 opacity-80">
                    {/* Energy */}
                    <div className="relative w-1.5 h-full bg-muted/30 rounded-[1px] overflow-hidden" title={`Energy: ${Math.round((track.energy || 0) * 100)}%`}>
                        <div className="absolute bottom-0 w-full bg-orange-500/80" style={{ height: `${(track.energy || 0) * 100}%` }} />
                    </div>
                    {/* Dance */}
                    <div className="relative w-1.5 h-full bg-muted/30 rounded-[1px] overflow-hidden" title={`Dance: ${Math.round((track.danceability || 0) * 100)}%`}>
                        <div className="absolute bottom-0 w-full bg-sky-500/80" style={{ height: `${(track.danceability || 0) * 100}%` }} />
                    </div>
                </div>
            </div>
        </div>
      )}

      {/* Suffix (Actions) */}
      {suffix && <div className="shrink-0 flex items-center">{suffix}</div>}

      {/* Year Badge (Absolute Top Right) */}
      {track.year && (
        <div className="absolute top-0 right-0 px-1.5 py-0.5 text-[9px] font-bold leading-none text-primary-foreground bg-primary/70 rounded-bl-md rounded-tr-md backdrop-blur-[1px]">
            {track.year}
        </div>
      )}
    </div>
  );
}
