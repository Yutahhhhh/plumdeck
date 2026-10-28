import { useState, useEffect, useRef } from "react";
import { Track } from "@/types";
import { tracksService } from "@/services/tracks";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Check, Sparkles, Loader2 } from "lucide-react";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";

interface GenreCellProps {
  track: Track;
  onUpdate: (trackId: number, newGenre: string) => void;
}

export function GenreCell({ track, onUpdate }: GenreCellProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [value, setValue] = useState(track.genre || "");
  const [suggestedGenre, setSuggestedGenre] = useState<string | null>(null);
  const [suggestionReason, setSuggestionReason] = useState<string | null>(null);
  const [isLoadingSuggestion, setIsLoadingSuggestion] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (isOpen) {
      setValue(track.genre || "");
      // Fetch suggestion if not already available or verified
      if (!suggestedGenre && !suggestionReason) {
        setIsLoadingSuggestion(true);
        tracksService.suggestGenre(track.id)
          .then((res) => {
            if (res.suggested_genre) {
              setSuggestedGenre(res.suggested_genre);
            } else if (res.reason) {
                setSuggestionReason(res.reason);
            }
          })
          .catch(() => {})
          .finally(() => setIsLoadingSuggestion(false));
      }
      // Focus input
      setTimeout(() => inputRef.current?.focus(), 0);
    }
  }, [isOpen, track.id, track.genre]);

  const handleSave = async (newValue: string) => {
    try {
      await tracksService.updateGenre(track.id, newValue);
      onUpdate(track.id, newValue);
      setIsOpen(false);
    } catch (e) {
      console.error("Failed to update genre", e);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
        handleSave(value);
    }
    if (e.key === "Escape") {
        setIsOpen(false);
    }
  };

  const getReasonMessage = (reason: string) => {
    switch (reason) {
        case "no_embedding": return "Timbre comparison pending (embedding not computed yet)";
        case "no_verified_tracks": return "No verified tracks to compare";
        case "no_valid_candidates": return "No similar tracks found";
        default: return "No suggestions found";
    }
  };

  return (
    <Popover open={isOpen} onOpenChange={setIsOpen}>
      <PopoverTrigger asChild>
        <div 
            className="flex items-center gap-2 group cursor-pointer h-full w-full min-h-5 px-1 hover:bg-muted/50 rounded-sm" 
            title={track.is_genre_verified ? "Verified Genre" : "Click to edit"}
        >
            {track.genre ? (
                <Badge variant="secondary" className="truncate max-w-[150px] font-normal hover:bg-secondary/80">
                    {track.genre}
                </Badge>
            ) : (
                <span className="text-muted-foreground italic text-xs px-1">No Genre</span>
            )}
            {track.is_genre_verified && (
                <Check className="h-3 w-3 text-green-500 shrink-0" />
            )}
        </div>
      </PopoverTrigger>
      <PopoverContent className="p-0 w-[220px]" align="start" side="bottom">
        <div className="p-2 flex gap-1 border-b">
            <Input
                ref={inputRef}
                value={value}
                onChange={(e) => setValue(e.target.value)}
                className="h-8 text-sm"
                onKeyDown={handleKeyDown}
                placeholder="Enter genre..."
            />
            <Button size="icon" variant="ghost" className="h-8 w-8 shrink-0" onClick={() => handleSave(value)}>
                <Check className="h-4 w-4 text-green-500" />
            </Button>
        </div>
        <div className="p-1">
            <div className="text-xs font-medium text-muted-foreground px-2 py-1.5">Suggestions</div>
            {isLoadingSuggestion ? (
                <div className="flex items-center gap-2 px-2 py-1.5 text-xs text-muted-foreground">
                    <Loader2 className="h-3 w-3 animate-spin" />
                    Loading...
                </div>
            ) : suggestedGenre ? (
                <div 
                    className="flex items-center gap-2 px-2 py-1.5 text-sm hover:bg-accent hover:text-accent-foreground rounded-sm cursor-pointer"
                    onClick={() => {
                        handleSave(suggestedGenre);
                    }}
                >
                    <Sparkles className="h-3 w-3 text-yellow-500" />
                    <span>{suggestedGenre}</span>
                </div>
            ) : (
                <div className="px-2 py-1.5 text-xs text-muted-foreground italic">
                    {suggestionReason ? getReasonMessage(suggestionReason) : "No suggestions found"}
                </div>
            )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
