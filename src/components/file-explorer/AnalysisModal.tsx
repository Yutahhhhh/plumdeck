import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
} from "@/components/ui/dialog";
import { Trash2, Play, Loader2, Check } from "lucide-react";
import { useState } from "react";

interface AnalysisModalProps {
  isOpen: boolean;
  onOpenChange: (open: boolean) => void;
  selectedPaths: Set<string>;
  forceUpdate: boolean;
  onSetForceUpdate: (val: boolean) => void;
  onToggleSelection: (path: string) => void;
  onClearSelection: () => void;
  onIngest: () => void;
}

export function AnalysisModal({
  isOpen,
  onOpenChange,
  selectedPaths,
  forceUpdate,
  onSetForceUpdate,
  onToggleSelection,
  onClearSelection,
  onIngest,
}: AnalysisModalProps) {
  const [isStarting, setIsStarting] = useState(false);
  const [hasStarted, setHasStarted] = useState(false);

  const getFileName = (path: string) => {
    if (!path) return "";
    return path.split(/[/\\]/).pop() || path;
  };

  const handleStart = async () => {
    setIsStarting(true);
    await onIngest();
    setIsStarting(false);
    setHasStarted(true);
    
    // Reset state when modal closes (handled by parent or user action)
  };

  // Reset internal state when modal opens
  if (!isOpen && (hasStarted || isStarting)) {
      setHasStarted(false);
      setIsStarting(false);
  }

  return (
    <Dialog open={isOpen} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[500px]">
        <DialogHeader>
          <DialogTitle>Confirm Analysis</DialogTitle>
          <DialogDescription>
            You are about to analyze {selectedPaths.size} items. This process
            extracts BPM, Key, timbre and the waveform. MusiCNN embeddings for
            similarity search are computed in the background afterwards.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-2">
          {/* File List Preview */}
          <div className="border rounded-md">
            <div className="bg-muted px-3 py-2 text-xs font-medium text-muted-foreground border-b flex justify-between items-center">
              <span>Selected Items</span>
              <Button
                variant="ghost"
                size="sm"
                className="h-5 text-[10px] px-2"
                onClick={onClearSelection}
                disabled={isStarting || hasStarted}
              >
                Clear All
              </Button>
            </div>
            <ScrollArea className="h-[200px] p-2">
              <div className="space-y-1">
                {Array.from(selectedPaths).map((path) => (
                  <div
                    key={path}
                    className="flex items-center justify-between text-sm p-1.5 hover:bg-muted rounded group"
                  >
                    <span
                      className="truncate flex-1 mr-2 text-xs font-mono min-w-0"
                      title={path}
                    >
                      {getFileName(path)}
                    </span>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-5 w-5 opacity-0 group-hover:opacity-100 transition-opacity"
                      onClick={() => onToggleSelection(path)}
                      disabled={isStarting || hasStarted}
                    >
                      <Trash2 className="h-3 w-3 text-destructive" />
                    </Button>
                  </div>
                ))}
              </div>
            </ScrollArea>
          </div>

          {/* Options */}
          <div className="flex flex-row items-start space-x-3 space-y-0 rounded-md border p-4 shadow-sm">
            <Checkbox
              id="force-update"
              checked={forceUpdate}
              onCheckedChange={(checked: boolean | string) => onSetForceUpdate(checked === true)}
              disabled={isStarting || hasStarted}
            />
            <div className="space-y-1 leading-none">
              <Label
                htmlFor="force-update"
                className="text-sm font-medium leading-none peer-disabled:cursor-not-allowed peer-disabled:opacity-70"
              >
                Force Re-analysis
              </Label>
              <p className="text-sm text-muted-foreground">
                If checked, already analyzed files will be processed again.
                Uncheck to skip existing tracks (recommended).
              </p>
            </div>
          </div>
        </div>

        <DialogFooter>
          <DialogClose asChild>
            <Button variant="ghost">Close</Button>
          </DialogClose>
          <Button 
            onClick={handleStart} 
            className="gap-2 min-w-[140px]" 
            disabled={isStarting || hasStarted}
          >
            {isStarting ? (
                <>
                    <Loader2 className="h-4 w-4 animate-spin" />
                    Starting...
                </>
            ) : hasStarted ? (
                <>
                    <Check className="h-4 w-4" />
                    Started
                </>
            ) : (
                <>
                    <Play className="h-4 w-4" />
                    Start Analysis
                </>
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
