import {
  SkipBack,
  SkipForward,
  Volume2,
  X,
  Minimize2,
  Maximize2,
  Music,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Slider } from "@/components/ui/slider";
import { formatTime } from "@/lib/utils";
import { Track } from "@/types";
import { FileMetadata } from "@/services/metadata";
import { Waveform } from "./Waveform";
import { PlayButton } from "@/components/ui/PlayButton";

interface MiniPlayerProps {
  track: Track;
  metadata: FileMetadata | null;
  isPlaying: boolean;
  progress: number;
  duration: number;
  volume: number;
  isExpanded: boolean;
  onPlayPause: () => void;
  onSkipBack: () => void;
  onSkipForward: () => void;
  onVolumeChange: (val: number) => void;
  onToggleExpand: () => void;
  onClose: () => void;
  onSeek: (ratio: number) => void;
}

export function MiniPlayer({
  track,
  metadata,
  progress,
  duration,
  volume,
  isExpanded,
  onSkipBack,
  onSkipForward,
  onVolumeChange,
  onToggleExpand,
  onClose,
  onSeek,
}: MiniPlayerProps) {
  return (
    <div className="h-20 px-4 flex items-center justify-between bg-background/95 backdrop-blur shrink-0 z-10 relative">
      {/* Waveform Overlay */}
      <div 
        className="absolute top-0 left-0 right-0 h-full opacity-10 pointer-events-none z-0"
        style={{ maskImage: 'linear-gradient(to bottom, black, transparent)' }}
      >
      </div>

      <Waveform
        trackId={Number.isSafeInteger(track?.id) ? track.id : null}
        progress={progress} 
        duration={duration} 
        onSeek={onSeek} 
        isExpanded={isExpanded}
      />

      <div className="flex items-center gap-3 w-1/3 min-w-0 z-10">
        <div
          className="h-12 w-12 bg-muted rounded-md shrink-0 cursor-pointer overflow-hidden shadow-sm hover:ring-1 ring-primary transition-all flex items-center justify-center"
          onClick={onToggleExpand}
        >
          {metadata?.artwork ? (
            <img
              src={`data:image/jpeg;base64,${metadata.artwork}`}
              className="h-full w-full object-cover"
            />
          ) : (
            <Music className="h-6 w-6 text-muted-foreground opacity-20" />
          )}
        </div>
        <div className="min-w-0">
          <div className="font-bold truncate text-sm tracking-tight leading-tight">
            {track.title}
          </div>
          <div className="flex items-center gap-2 mt-0.5">
            <span className="text-xs text-muted-foreground truncate">
              {track.artist}
              {track.year ? ` • ${track.year}` : ""}
              {track.genre ? ` • ${track.genre}` : ""}
              {track.subgenre ? ` • ${track.subgenre}` : ""}
            </span>
          </div>
        </div>
      </div>

      <div className="flex flex-col items-center gap-1 z-10">
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="icon"
            className="h-8 w-8 text-muted-foreground"
            onClick={onSkipBack}
          >
            <SkipBack className="h-4 w-4" />
          </Button>
          <PlayButton
            track={track}
            size="icon"
            className="h-10 w-10 rounded-full shadow-md bg-primary text-primary-foreground hover:scale-105 transition-transform"
            iconClassName="h-5 w-5"
            showPauseWhenPlaying={true}
          />
          <Button
            variant="ghost"
            size="icon"
            className="h-8 w-8 text-muted-foreground"
            onClick={onSkipForward}
          >
            <SkipForward className="h-4 w-4" />
          </Button>
        </div>
        <div className="text-[10px] font-mono text-muted-foreground tabular-nums">
          {formatTime(progress)} /{" "}
          {formatTime(duration)}
        </div>
      </div>

      <div className="flex items-center justify-end gap-4 w-1/3 z-10">
        <div className="flex items-center gap-2 w-24">
          <Volume2 className="h-3 w-3 text-muted-foreground shrink-0" />
          <Slider
            value={[volume * 100]}
            onValueChange={([v]) => onVolumeChange(v / 100)}
            className="h-1.5"
          />
        </div>
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8"
          onClick={onToggleExpand}
        >
          {isExpanded ? (
            <Minimize2 className="h-4 w-4 text-primary" />
          ) : (
            <Maximize2 className="h-4 w-4" />
          )}
        </Button>
        <Button
          variant="ghost"
          size="icon"
          onClick={onClose}
          className="h-8 w-8 hover:text-destructive transition-colors"
        >
          <X className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}
