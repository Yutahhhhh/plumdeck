import { useRef, type MutableRefObject } from "react";
import { GripVertical, Play, Square } from "lucide-react";
import { cn } from "@/lib/utils";
import type { AssistCandidate } from "@/services/assist";
import { canDragOut } from "./native-drag";
import type { AuditionControl } from "./audition";
import { formatKey, useKeyDisplay } from "@/lib/key-display";

interface CandidateCardProps {
  track: AssistCandidate;
  /** Position badge ("01"), or a short label such as "盛り上げるなら". */
  badge: string;
  dragging: boolean;
  freeze: MutableRefObject<boolean>;
  onDrag: (filepath: string, label: string) => void;
  /** Omit to hide the audition button. */
  audition?: AuditionControl;
}

/** One suggestion the DJ can drag straight onto a rekordbox deck. */
export function CandidateCard({ track, badge, dragging, freeze, onDrag, audition }: CandidateCardProps) {
  const pointer = useRef<{ id: number; x: number; y: number } | null>(null);
  const keyDisplay = useKeyDisplay();
  const cautions = track.reasons.filter(reason => reason.tone === "caution");
  const playing = audition?.playingId === track.id;

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
      aria-current={playing || undefined}
      className={cn("group flex select-none gap-2 rounded-lg border px-2.5 py-2", playing ? "border-emerald-300/60 bg-emerald-300/[0.08] ring-1 ring-emerald-300/30" : "border-white/10 bg-white/[0.025]", canDragOut() && "cursor-grab active:cursor-grabbing", canDragOut() && !playing && "hover:border-amber-300/30")}
    >
      <span className="shrink-0 pt-0.5 text-[10px] tabular-nums text-slate-600">{badge}</span>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline justify-between gap-2"><p className="truncate text-xs font-medium" title={track.title}>{track.title}</p><span className="flex shrink-0 items-baseline gap-1.5">{playing && <span className="animate-pulse text-[9px] font-medium text-emerald-300">試聴中</span>}<span className="text-[10px] tabular-nums text-amber-200/80">{track.bpm || "—"} · {formatKey(track.key, keyDisplay.notation)}</span></span></div>
        <p className="truncate text-[10px] text-slate-400">{track.artist}{track.year ? ` · ${track.year}` : ""}{track.genre ? ` · ${track.genre}` : ""}</p>
        {audition?.failedId === track.id && <p role="alert" className="mt-1 text-[10px] text-rose-300">試聴できませんでした</p>}
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
      <div className="flex shrink-0 flex-col items-center gap-2">
        {audition && (
          <button
            type="button"
            disabled={dragging}
            aria-pressed={playing}
            aria-label={playing ? "試聴を停止" : "試聴"}
            title={playing ? "試聴を停止" : "曲の中盤から30秒試聴します。音はOSの既定の出力から鳴るため、それがDJのマスター出力だと会場に流れます"}
            onClick={() => audition.toggle(track)}
            className={cn("flex size-6 items-center justify-center rounded-full border transition-colors disabled:opacity-40", playing ? "border-emerald-300 bg-emerald-300 text-slate-950 hover:bg-emerald-200" : "border-white/15 text-slate-300 hover:border-emerald-300/50 hover:text-emerald-200")}
          >
            {playing ? <Square className="size-2.5 fill-current" /> : <Play className="size-3 translate-x-px fill-current" />}
          </button>
        )}
        <GripVertical className="size-3.5 text-slate-600" />
      </div>
    </article>
  );
}
