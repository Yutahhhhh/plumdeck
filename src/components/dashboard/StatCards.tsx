import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import {
  Music,
  CheckCircle2,
  AlertCircle,
  Wand2,
  Bot,
  Mic,
} from "lucide-react";
import { DashboardStats } from "@/services/system";

interface StatCardsProps {
  stats: DashboardStats;
  onNavigate: (view: string) => void;
  onAnalyze: () => void;
}

export function StatCards({
  stats,
  onNavigate,
  onAnalyze,
}: StatCardsProps) {
  const analyzedPercent =
    stats.total_tracks > 0
      ? Math.round((stats.analyzed_tracks / stats.total_tracks) * 100)
      : 0;

  return (
    <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
      {/* Total Tracks */}
      <Card>
        <CardContent className="p-4 flex items-center gap-4">
          <div className="bg-blue-100 p-3 rounded-full text-blue-600">
            <Music className="h-5 w-5" />
          </div>
          <div>
            <p className="text-sm font-medium text-muted-foreground">
              Total Tracks
            </p>
            <h3 className="text-2xl font-bold">{stats.total_tracks}</h3>
          </div>
        </CardContent>
      </Card>

      {/* Analysis Status */}
      <Card
        className={
          stats.unanalyzed_tracks > 0 ? "border-amber-200 bg-amber-50/50" : ""
        }
      >
        <CardContent className="p-4">
          <div className="flex justify-between items-start mb-2">
            <div className="flex items-center gap-2">
              {stats.unanalyzed_tracks > 0 ? (
                <div className="bg-amber-100 p-2 rounded-full text-amber-600">
                  <AlertCircle className="h-4 w-4" />
                </div>
              ) : (
                <div className="bg-green-100 p-2 rounded-full text-green-600">
                  <CheckCircle2 className="h-4 w-4" />
                </div>
              )}
              <div>
                <p className="text-sm font-medium text-muted-foreground">
                  Analyzed
                </p>
                <h3 className="text-2xl font-bold">{analyzedPercent}%</h3>
              </div>
            </div>
          </div>
          {!!stats.embedding_pending_tracks && (
            <p className="mb-2 text-xs text-muted-foreground">
              音色計算待ち {stats.embedding_pending_tracks.toLocaleString()}曲
            </p>
          )}
          {stats.unanalyzed_tracks > 0 && (
            <Button
              size="sm"
              variant="outline"
              className="w-full h-7 text-xs bg-white"
              onClick={onAnalyze}
            >
              Analyze {stats.unanalyzed_tracks} Tracks
            </Button>
          )}
        </CardContent>
      </Card>

      {/* Unverified Genres */}
      <Card>
        <CardContent className="p-4">
          <div className="flex items-center gap-4 mb-2">
            <div className="bg-purple-100 p-3 rounded-full text-purple-600">
              <Wand2 className="h-5 w-5" />
            </div>
            <div>
              <p className="text-sm font-medium text-muted-foreground">
                Unverified
              </p>
              <h3 className="text-2xl font-bold">
                {stats.unverified_genres_count}
              </h3>
            </div>
          </div>
          <Button
            variant="ghost"
            size="sm"
            className="w-full h-7 text-xs text-muted-foreground"
            onClick={() => onNavigate("genres")}
          >
            Manage Genres &rarr;
          </Button>
        </CardContent>
      </Card>

      {/* Lyrics Count */}
      <Card>
        <CardContent className="p-4 flex items-center gap-4">
          <div className="bg-indigo-100 p-3 rounded-full text-indigo-600">
            <Mic className="h-5 w-5" />
          </div>
          <div>
            <p className="text-sm font-medium text-muted-foreground">
              With Lyrics
            </p>
            <h3 className="text-2xl font-bold">{stats.lyrics_tracks_count}</h3>
          </div>
        </CardContent>
      </Card>

      {/* Config Status */}
      <Card
        className={
          !stats.config.has_root_path
            ? "border-red-200 bg-red-50/30"
            : ""
        }
      >
        <CardContent className="p-4 flex flex-col justify-between h-full">
          <div className="flex items-center gap-4">
            <div className="bg-gray-100 p-3 rounded-full text-gray-600">
              <Bot className="h-5 w-5" />
            </div>
            <div className="min-w-0">
              <p className="text-sm font-medium text-muted-foreground">
                System
              </p>
              <div className="flex items-center gap-2">
                <span
                  className="text-xs px-2 py-0.5 rounded-full bg-blue-100 text-blue-700"
                >
                  AI: MCP client
                </span>
              </div>
            </div>
          </div>

          {!stats.config.has_root_path && (
            <Button
              size="sm"
              variant="destructive"
              className="w-full h-7 text-xs mt-2"
              onClick={() => onNavigate("settings")}
            >
              Set Root Path
            </Button>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
