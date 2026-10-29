import {
  Music,
  BookOpen,
  Settings,
  Menu,
  List,
  Tags,
  LayoutDashboard,
  Bot,
  MessageSquareQuote,
  Wrench,
  Heart,
} from "lucide-react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/toast";
import { cn } from "@/lib/utils";
import { NavButton } from "./NavButton";

interface SidebarProps {
  activeView: string;
  onNavigate: (view: string) => void;
  isOpen: boolean;
  toggleSidebar: () => void;
}

export function Sidebar({
  activeView,
  onNavigate,
  isOpen,
  toggleSidebar,
}: SidebarProps) {
  const supportUrl = "https://ko-fi.com/yutahhh";
  const openSupport = async (event: React.MouseEvent<HTMLAnchorElement>) => {
    if (!isTauri()) return;
    event.preventDefault();
    try {
      await invoke("plugin:shell|open", { path: supportUrl });
    } catch {
      toast.error("ブラウザを開けませんでした", supportUrl);
    }
  };
  return (
    <div
      className={cn(
        "flex flex-col shrink-0 min-h-0 border-r bg-background transition-all duration-300",
        isOpen ? "w-64" : "w-16"
      )}
    >
      <div className="p-4 flex shrink-0 items-center justify-between h-16 border-b">
        {isOpen && <span className="font-bold text-xl">plumdeck</span>}
        <Button variant="ghost" size="icon" aria-label="サイドバーを開閉" onClick={toggleSidebar}>
          <Menu className="h-5 w-5" />
        </Button>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto py-4 flex flex-col gap-2">
        <NavButton
          icon={<LayoutDashboard className="h-5 w-5" />}
          label="Dashboard"
          isActive={activeView === "dashboard"}
          onClick={() => onNavigate("dashboard")}
          isOpen={isOpen}
        />
        <NavButton
          icon={<Music className="h-5 w-5" />}
          label="Library"
          isActive={activeView === "library"}
          onClick={() => onNavigate("library")}
          isOpen={isOpen}
        />
        <NavButton
          icon={<List className="h-5 w-5" />}
          label="Setlists"
          isActive={activeView === "setlists"}
          onClick={() => onNavigate("setlists")}
          isOpen={isOpen}
        />
        <NavButton
          icon={<Tags className="h-5 w-5" />}
          label="Tags"
          isActive={activeView === "tags"}
          onClick={() => onNavigate("tags")}
          isOpen={isOpen}
        />
        <NavButton
          icon={<Bot className="h-5 w-5" />}
          label="MCP"
          isActive={activeView === "mcp"}
          onClick={() => onNavigate("mcp")}
          isOpen={isOpen}
        />
        <NavButton
          icon={<MessageSquareQuote className="h-5 w-5" />}
          label="Wordplay"
          isActive={activeView === "wordplay"}
          onClick={() => onNavigate("wordplay")}
          isOpen={isOpen}
        />
        <NavButton
          icon={<Wrench className="h-5 w-5" />}
          label="Workflow Tools"
          isActive={activeView === "workflows"}
          onClick={() => onNavigate("workflows")}
          isOpen={isOpen}
        />
        <NavButton
          icon={<BookOpen className="h-5 w-5" />}
          label="Docs"
          isActive={activeView === "docs"}
          onClick={() => onNavigate("docs")}
          isOpen={isOpen}
        />
        <NavButton
          icon={<Settings className="h-5 w-5" />}
          label="Settings"
          isActive={activeView === "settings"}
          onClick={() => onNavigate("settings")}
          isOpen={isOpen}
        />
      </div>
      <div className="shrink-0 border-t p-2">
        <a
          href={supportUrl}
          target="_blank"
          rel="noopener noreferrer"
          onClick={openSupport}
          title="開発を応援（Ko-fi・ブラウザで開く）"
          aria-label="開発を応援（Ko-fi・ブラウザで開く）"
          className={cn(
            "flex h-9 items-center gap-3 rounded-md px-2 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
            !isOpen && "justify-center"
          )}
        >
          <Heart className="h-4 w-4 shrink-0" aria-hidden="true" />
          {isOpen && <span>開発を応援 <span className="ml-1 text-[10px]">Ko-fi ↗</span></span>}
        </a>
      </div>
    </div>
  );
}
