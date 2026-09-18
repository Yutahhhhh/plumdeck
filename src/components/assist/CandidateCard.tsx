import { useRef, type MutableRefObject } from "react";
import { GripVertical } from "lucide-react";
import { cn } from "@/lib/utils";
import type { AssistCandidate } from "@/services/assist";
import { canDragOut } from "./native-drag";

interface CandidateCardProps {
  track: AssistCandidate;
  /** Position badge ("01"), or a short label such as "盛り上げるなら". */
  badge: string;
  dragging: boolean;
  freeze: MutableRefObject<boolean>;
  onDrag: (filepath: string, label: string) => void;
}

/** One suggestion the DJ can drag straight onto a rekordbox deck. */
export function CandidateCard({ track, badge, dragging, freeze, onDrag }: CandidateCardProps) {
  const pointer = useRef<{ id: number; x: number; y: number } | null>(null);
  const cautions = track.reasons.filter(reason => reason.tone === "caution");

  return (
    <article
      draggable={false}
      onPointerDown={event => {
        if (!canDragOut() || freeze.current || event.button !== 0 || event.pointerType !== "mouse" || (event.target as Element).closest("button, a, input, summary, details")) return;
        event.preventDefault();
        pointer.current = { id: event.pointerId, x: event.clientX, y: event.clientY };
        event.currentTarget.setPointerCapture(event.pointerId);
      }}
      onPointerMove={event => {
        const current = pointer.current;
        if (!current || current.id !== event.pointerId) return;
        if (!(event.buttons & 1)) { pointer.current = null; return; }
        if (Math.hypot(event.clientX - current.x, event.clientY - current.y) < 5) return;
        pointer.current = null;
        event.currentTarget.releasePointerCapture(event.pointerId);
        // Use a native gesture directly, avoiding a competing HTML drag session.
        onDrag(track.filepath, `${track.artist} — ${track.title}`);
      }}
      onPointerUp={() => { pointer.current = null; }}
      onPointerCancel={() => { pointer.current = null; }}
      onLostPointerCapture={() => { pointer.current = null; }}
      onDragStart={event => event.preventDefault()}
      className={cn("group flex select-none gap-2 rounded-lg border border-white/10 bg-white/[0.025] px-2.5 py-2", canDragOut() && "cursor-grab hover:border-amber-300/30 active:cursor-grabbing")}
    >
      <span className="shrink-0 pt-0.5 text-[10px] tabular-nums text-slate-600">{badge}</span>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline justify-between gap-2"><p className="truncate text-xs font-medium" title={track.title}>{track.title}</p><span className="shrink-0 text-[10px] tabular-nums text-amber-200/80">{track.bpm || "—"} · {track.key || "—"}</span></div>
        <p className="truncate text-[10px] text-slate-400">{track.artist}{track.year ? ` · ${track.year}` : ""}{track.genre ? ` · ${track.genre}` : ""}</p>
        {track.agent_reason && <p className="mt-1 text-[10px] leading-snug text-sky-200">{track.agent_reason}</p>}
        <p className="mt-1 line-clamp-2 text-[10px] leading-snug text-slate-300" title={track.summary}>{track.summary}</p>
        {cautions.length > 0 && <p className="mt-0.5 truncate text-[10px] text-amber-300/80" title={cautions.map(reason => reason.text).join("\n")}>{cautions[0].text}{cautions.length > 1 ? ` ほか${cautions.length - 1}件` : ""}</p>}
        <details className="mt-0.5 text-[10px]" onClick={event => { event.stopPropagation(); if (dragging) event.preventDefault(); }}>
          <summary className="w-fit cursor-pointer text-slate-500 hover:text-slate-300">詳しく</summary>
          <div className="space-y-1 pt-1.5">
            {track.reasons.map((reason, i) => <p key={i} className={cn("leading-relaxed", reason.tone === "caution" ? "text-amber-300/80" : reason.tone === "good" ? "text-emerald-300/80" : "text-slate-400")}>{reason.text}</p>)}
            {track.wordplay?.transition_notes && <p className="text-violet-300">{track.wordplay.transition_notes}</p>}
          </div>
        </details>
      </div>
      <GripVertical className="mt-1 size-3.5 shrink-0 text-slate-600" />
    </article>
  );
}
