import React from "react";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ClassifierTab } from "./ClassifierTab";
import { McpAnalysisTab } from "./McpAnalysisTab";
import { CleanupTab } from "./CleanupTab";
import { Track } from "@/types";

import { AnalysisMode } from "@/services/genres";

interface GenreManagerProps {
  onPlay: (track: Track) => void;
  mode: AnalysisMode;
}

export const GenreManager: React.FC<GenreManagerProps> = ({ onPlay, mode }) => {
  const title = mode === "genre" ? "Genre Manager" : mode === "subgenre" ? "Subgenre Manager" : "Genre & Subgenre Manager";

  return (
    <div className="h-full flex flex-col p-6 space-y-6">
      <div className="flex justify-between items-start">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">{title}</h1>
          <p className="text-muted-foreground">
            Confirm genres suggested by the on-device classifier, clean up spellings, or use your MCP client's model.
          </p>
        </div>
      </div>

      <Tabs defaultValue="classifier" className="flex-1 flex flex-col min-h-0">
        <TabsList>
          <TabsTrigger value="classifier">自動分類</TabsTrigger>
          <TabsTrigger value="cleanup">Cleanup</TabsTrigger>
          <TabsTrigger value="mcp-analysis">MCP Analysis</TabsTrigger>
        </TabsList>

        <div className="flex-1 mt-4 border rounded-lg bg-background/50 flex flex-col min-h-0 overflow-hidden">
          <TabsContent
            value="cleanup"
            className="flex-1 p-0 m-0 min-h-0 data-[state=inactive]:hidden"
          >
            <CleanupTab onPlay={onPlay} mode={mode} />
          </TabsContent>
          <TabsContent
            value="classifier"
            className="flex-1 p-0 m-0 min-h-0 data-[state=inactive]:hidden"
          >
            <ClassifierTab mode={mode} />
          </TabsContent>
          <TabsContent
            value="mcp-analysis"
            className="flex-1 p-0 m-0 min-h-0 data-[state=inactive]:hidden"
          >
            <McpAnalysisTab mode={mode} />
          </TabsContent>
        </div>
      </Tabs>
    </div>
  );
};
