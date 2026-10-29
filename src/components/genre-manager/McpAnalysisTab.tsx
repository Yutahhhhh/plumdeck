import { Bot, CheckCircle2 } from "lucide-react";
import { AnalysisMode } from "@/services/genres";

export function McpAnalysisTab({ mode }: { mode: AnalysisMode }) {
  return (
    <div className="h-full overflow-y-auto p-6">
      <div className="max-w-2xl mx-auto space-y-5 border rounded-xl p-6 bg-muted/20">
        <div className="flex items-start gap-3">
          <div className="rounded-full bg-primary/10 p-2 text-primary">
            <Bot className="h-5 w-5" />
          </div>
          <div>
            <h3 className="font-semibold">Analyze with your MCP client</h3>
            <p className="text-sm text-muted-foreground mt-1">
              plumdeck no longer chooses or calls a model. The model in your connected
              Codex, Claude, or other MCP client performs the {mode} classification.
            </p>
          </div>
        </div>

        <div className="space-y-3 text-sm">
          <div className="flex gap-2">
            <CheckCircle2 className="h-4 w-4 mt-0.5 text-green-600 shrink-0" />
            <span>
              The client calls <code>get_genre_predictions</code> (low-confidence tracks from the
              on-device classifier) or <code>get_genre_analysis_context</code>.
            </span>
          </div>
          <div className="flex gap-2">
            <CheckCircle2 className="h-4 w-4 mt-0.5 text-green-600 shrink-0" />
            <span>Its own model classifies the returned metadata and audio features.</span>
          </div>
          <div className="flex gap-2">
            <CheckCircle2 className="h-4 w-4 mt-0.5 text-green-600 shrink-0" />
            <span>It applies validated results with <code>apply_genre_analyses</code>.</span>
          </div>
        </div>

        <div className="rounded-md border bg-background p-3 text-xs">
          Example request: “Analyze all missing {mode} labels in plumdeck, reuse the
          existing taxonomy where appropriate, and apply Medium/High-confidence results.”
        </div>
      </div>
    </div>
  );
}
