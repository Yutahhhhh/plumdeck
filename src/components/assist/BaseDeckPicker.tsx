import { cn } from "@/lib/utils";
import type { BaseDeck } from "./useBaseDeck";

interface BaseDeckPickerProps {
  slots: number[];
  base: BaseDeck;
  hasDecks: boolean;
  disabled: boolean;
}

/** 1 / MASTER / 2 …: pick the base deck by hand, or follow whichever deck is playing. */
export function BaseDeckPicker({ slots, base, hasDecks, disabled }: BaseDeckPickerProps) {
  const playingDot = (active: boolean) => <span aria-hidden className={cn("ml-1.5 inline-block size-1.5 animate-pulse rounded-full align-middle", active ? "bg-emerald-700" : "bg-emerald-400")} />;
  const masterTitle = base.masterAvailable
    ? "再生中のデッキを基準にします。複数が再生中のときは今の基準デッキを保ち、それが止まったら切り替えます（再生中のデッキがなければそのまま）"
    : hasDecks ? "この環境では rekordbox の再生位置を読み取れないため、再生中のデッキを判定できません" : "rekordbox のデッキを確認できるとMASTERを使えます";
  const master = (
    <button type="button" key="master" disabled={disabled || (!base.masterAvailable && !base.master)} aria-pressed={base.master} title={masterTitle} onClick={() => base.select("master")} className={cn("rounded px-3 py-1 text-xs font-semibold tracking-wide transition-colors disabled:opacity-50", base.master ? "bg-amber-300 text-slate-950" : "bg-white/5 text-slate-400 hover:bg-white/10")}>
      MASTER{base.master && <span className="ml-1 tabular-nums">· {base.slot}</span>}
    </button>
  );
  return (
    <div className="flex gap-1">
      {slots.flatMap((value, index) => {
        const playing = base.playing.includes(value);
        const active = !base.master && base.slot === value;
        const button = (
          <button type="button" key={value} disabled={disabled} aria-pressed={active} title={playing ? "再生中" : undefined} onClick={() => base.select(value)} className={cn("rounded px-3 py-1 text-xs transition-colors disabled:opacity-50", active ? "bg-amber-300 text-slate-950" : "bg-white/5 text-slate-400 hover:bg-white/10", base.master && base.slot === value && "ring-1 ring-amber-300/60")}>
            {value}{playing && playingDot(active)}
          </button>
        );
        return index === 1 ? [master, button] : [button];
      })}
    </div>
  );
}
