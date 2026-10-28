import { memo } from "react";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Checkbox } from "@/components/ui/checkbox";
import { Badge } from "@/components/ui/badge";
import { Folder, Music, Loader2 } from "lucide-react";
import { FileItem } from "./types";

interface FileListProps {
  items: FileItem[];
  selectedPaths: Set<string>;
  hideAnalyzed: boolean;
  isLoading: boolean;
  disabled?: boolean;
  onToggleSelection: (path: string) => void;
  onNavigate: (path: string) => void;
}

// 個別の行コンポーネントとして切り出し、メモ化する
const FileRow = memo(
  ({
    item,
    isSelected,
    hideAnalyzed,
    disabled,
    onToggle,
    onNav,
  }: {
    item: FileItem;
    isSelected: boolean;
    hideAnalyzed: boolean;
    disabled: boolean;
    onToggle: (path: string) => void;
    onNav: (path: string) => void;
  }) => {
    return (
      <div
        className={`flex items-center gap-3 p-2 rounded-md transition-colors group ${
          disabled
            ? "opacity-50 cursor-not-allowed"
            : "hover:bg-accent cursor-pointer"
        }`}
        onClick={() => {
          if (disabled) return;
          if (item.is_dir) {
            onNav(item.path);
          } else {
            onToggle(item.path);
          }
        }}
      >
        <Checkbox
          checked={isSelected}
          onCheckedChange={() => !disabled && onToggle(item.path)}
          disabled={disabled}
          onClick={(e) => e.stopPropagation()}
        />

        <div className="flex items-center gap-2 flex-1 overflow-hidden">
          {item.is_dir ? (
            <Folder className="h-4 w-4 text-blue-400 shrink-0" />
          ) : (
            <Music className="h-4 w-4 text-gray-400 shrink-0" />
          )}
          <span
            className={`text-sm truncate ${
              item.is_analyzed ? "text-muted-foreground" : ""
            }`}
            title={item.name}
          >
            {item.name}
          </span>
          {!item.is_dir && item.is_analyzed && !hideAnalyzed && (
            <Badge
              variant="secondary"
              className="text-[10px] h-4 px-1 shrink-0"
            >
              {item.analysis_level === "light" ? "軽量" : "解析済み"}
            </Badge>
          )}
          {!item.is_dir && item.is_analyzed && !hideAnalyzed && item.embedding_ready === false && (
            <Badge
              variant="outline"
              className="text-[10px] h-4 px-1 shrink-0 text-muted-foreground"
              title="再生や推薦はできます。音色の比較は計算が済みしだい使えます"
            >
              音色計算待ち
            </Badge>
          )}
        </div>
      </div>
    );
  },
  (prev, next) => {
    // Propsの比較関数（パフォーマンスチューニング）
    return (
      prev.item === next.item &&
      prev.isSelected === next.isSelected &&
      prev.disabled === next.disabled &&
      prev.hideAnalyzed === next.hideAnalyzed
    );
  }
);

FileRow.displayName = "FileRow";

export const FileList = memo(
  ({
    items,
    selectedPaths,
    hideAnalyzed,
    isLoading,
    disabled = false,
    onToggleSelection,
    onNavigate,
  }: FileListProps) => {
    if (isLoading) {
      return (
        <div className="flex-1 flex items-center justify-center h-full min-h-[200px]">
          <div className="flex flex-col items-center gap-2 text-muted-foreground">
            <Loader2 className="h-8 w-8 animate-spin" />
            <span className="text-sm">Loading...</span>
          </div>
        </div>
      );
    }

    if (!items || items.length === 0) {
      return (
        <div className="flex-1 p-4 text-center text-muted-foreground">
          {hideAnalyzed ? "No unanalyzed items found." : "No items found."}
        </div>
      );
    }

    return (
      <ScrollArea className="flex-1 p-4">
        <div className="space-y-1">
          {items.map((item) => (
            <FileRow
              key={item.path}
              item={item}
              isSelected={selectedPaths.has(item.path)}
              hideAnalyzed={hideAnalyzed}
              disabled={disabled}
              onToggle={onToggleSelection}
              onNav={onNavigate}
            />
          ))}
        </div>
      </ScrollArea>
    );
  }
);

FileList.displayName = "FileList";
