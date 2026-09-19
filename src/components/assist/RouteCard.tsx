import type { MutableRefObject } from "react";
import type { AssistCandidate, AssistRoute } from "@/services/assist";
import { CandidateCard } from "./CandidateCard";

interface RouteCardProps {
  route: AssistRoute;
  index: number;
  dragging: boolean;
  freeze: MutableRefObject<boolean>;
  onDrag: (filepath: string, label: string) => void;
}

function cardTrack(track: AssistRoute["tracks"][number], summary: string, step?: AssistRoute["steps"][number]): AssistCandidate {
  return {
    ...track,
    score: step?.score ?? null,
    components: {},
    summary,
    strengths: [],
    reasons: step?.reasons ?? [],
    wordplay: null,
  };
}

/** One exact path from the current deck track to the selected destination. */
export function RouteCard({ route, index, dragging, freeze, onDrag }: RouteCardProps) {
  const destinationIndex = route.tracks.length - 1;
  return (
    <article className="space-y-1.5 rounded-xl border border-sky-300/20 bg-sky-300/[0.035] p-2">
      <div className="flex items-center justify-between text-[10px] text-sky-100">
        <span>ルート {index + 1} · {route.steps.length - 1}曲の中継</span>
        <span className="tabular-nums text-sky-200/70">相性 {route.score.toFixed(2)}</span>
      </div>
      <div className="space-y-1">
        {route.tracks.slice(1).map((track, offset) => {
          const trackIndex = offset + 1;
          const step = route.steps[trackIndex - 1];
          const isDestination = trackIndex === destinationIndex;
          const summary = isDestination ? "指定した目的曲" : step?.summary ?? "中継曲";
          const candidate = cardTrack(track, summary, step);
          return (
            <div key={`${track.id}-${trackIndex}`} className="space-y-1">
              {trackIndex > 1 && <p className="pl-5 text-[9px] text-slate-600">↓ {step?.summary}</p>}
              <CandidateCard
                track={candidate}
                badge={isDestination ? "GOAL" : String(trackIndex).padStart(2, "0")}
                dragging={dragging}
                freeze={freeze}
                onDrag={(filepath, label) => onDrag(filepath, label)}
              />
            </div>
          );
        })}
      </div>
    </article>
  );
}
