import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { cn } from "@/lib/utils";
import { EMPTY_FILTERS, hasFilters, type AssistFilters } from "@/services/assist";
import { genreService } from "@/services/genres";

interface Draft {
  bpmMin: string;
  bpmMax: string;
  genres: string[];
  artists: string;
  keys: string;
  yearMin: string;
  yearMax: string;
  query: string;
}

const toDraft = (filters: AssistFilters): Draft => ({
  bpmMin: filters.bpm_min?.toString() ?? "",
  bpmMax: filters.bpm_max?.toString() ?? "",
  genres: filters.genres,
  artists: filters.artists.join(", "),
  keys: filters.keys.join(", "),
  yearMin: filters.year_min?.toString() ?? "",
  yearMax: filters.year_max?.toString() ?? "",
  query: filters.query,
});

const list = (value: string) => value.split(/[,、]/).map(item => item.trim()).filter(Boolean);
const number = (value: string) => {
  const parsed = Number(value);
  return value.trim() !== "" && Number.isFinite(parsed) && parsed > 0 ? parsed : null;
};

export function draftToFilters(draft: Draft): AssistFilters {
  return {
    bpm_min: number(draft.bpmMin),
    bpm_max: number(draft.bpmMax),
    genres: draft.genres,
    subgenres: [],
    artists: list(draft.artists),
    keys: list(draft.keys),
    year_min: number(draft.yearMin),
    year_max: number(draft.yearMax),
    query: draft.query.trim(),
  };
}

/** Compound conditions, applied on 「適用」 so typing never refetches mid-word. */
export function FilterPanel({ value, onChange, disabled }: { value: AssistFilters; onChange: (filters: AssistFilters) => void; disabled: boolean }) {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState(() => toDraft(value));
  const [genres, setGenres] = useState<string[]>([]);
  const active = hasFilters(value);

  useEffect(() => { setDraft(toDraft(value)); }, [value]);
  useEffect(() => {
    if (!open || genres.length) return;
    genreService.getAllGenres().then(setGenres).catch(() => setGenres([]));
  }, [open, genres.length]);

  const set = (patch: Partial<Draft>) => setDraft(current => ({ ...current, ...patch }));
  const apply = () => onChange(draftToFilters(draft));
  const input = "w-full rounded border border-slate-700 bg-slate-900 px-2 py-1 text-[11px] text-slate-200 placeholder:text-slate-600";

  return (
    <section aria-label="条件で絞り込む" className="rounded-xl border border-white/10 bg-white/[0.02]">
      <button type="button" disabled={disabled} aria-expanded={open} onClick={() => setOpen(!open)} className="flex w-full items-center justify-between px-2.5 py-1.5 text-[10px] text-slate-400 disabled:opacity-50">
        <span>条件で絞り込む{active && <span className="ml-1.5 rounded bg-amber-300/15 px-1.5 py-0.5 text-amber-200">適用中</span>}</span>
        <span>{open ? "閉じる" : "開く"}</span>
      </button>
      {open && (
        <form className="space-y-2 border-t border-white/5 px-2.5 py-2" onSubmit={event => { event.preventDefault(); apply(); }}>
          <fieldset disabled={disabled} className="space-y-2">
            <div className="grid grid-cols-[3.5rem_1fr_auto_1fr] items-center gap-1.5 text-[10px] text-slate-500">
              <label htmlFor="assist-bpm-min">BPM</label>
              <input id="assist-bpm-min" inputMode="decimal" placeholder="下限" value={draft.bpmMin} onChange={event => set({ bpmMin: event.target.value })} className={input} />
              <span>〜</span>
              <input aria-label="BPM 上限" inputMode="decimal" placeholder="上限" value={draft.bpmMax} onChange={event => set({ bpmMax: event.target.value })} className={input} />
              <label htmlFor="assist-year-min">年代</label>
              <input id="assist-year-min" inputMode="numeric" placeholder="1990" value={draft.yearMin} onChange={event => set({ yearMin: event.target.value })} className={input} />
              <span>〜</span>
              <input aria-label="年代 上限" inputMode="numeric" placeholder="2005" value={draft.yearMax} onChange={event => set({ yearMax: event.target.value })} className={input} />
            </div>
            <div className="grid grid-cols-[3.5rem_1fr] items-center gap-1.5 text-[10px] text-slate-500">
              <label htmlFor="assist-genre-add">ジャンル</label>
              <select id="assist-genre-add" value="" onChange={event => { const genre = event.target.value; if (genre && !draft.genres.includes(genre)) set({ genres: [...draft.genres, genre] }); }} className={input}>
                <option value="">{genres.length ? "追加…" : "読み込み中…"}</option>
                {genres.filter(genre => !draft.genres.includes(genre)).map(genre => <option key={genre} value={genre}>{genre}</option>)}
              </select>
              {draft.genres.length > 0 && <><span /><div className="flex flex-wrap gap-1">{draft.genres.map(genre => <button type="button" key={genre} onClick={() => set({ genres: draft.genres.filter(item => item !== genre) })} className="flex items-center gap-0.5 rounded bg-white/10 px-1.5 py-0.5 text-[10px] text-slate-200">{genre}<X className="size-3" /></button>)}</div></>}
              <label htmlFor="assist-artists">アーティスト</label>
              <input id="assist-artists" placeholder="カンマ区切りで複数" value={draft.artists} onChange={event => set({ artists: event.target.value })} className={input} />
              <label htmlFor="assist-keys">キー</label>
              <input id="assist-keys" placeholder="8A, 9A / A minor" value={draft.keys} onChange={event => set({ keys: event.target.value })} className={input} />
              <label htmlFor="assist-query">キーワード</label>
              <input id="assist-query" placeholder="曲名・アーティスト・アルバム" value={draft.query} onChange={event => set({ query: event.target.value })} className={input} />
            </div>
            <p className="text-[9px] leading-relaxed text-slate-500">項目どうしは「かつ」、同じ項目の複数指定は「または」。値が未登録の曲は条件に合わないものとして扱います。</p>
            <div className="flex justify-end gap-1.5">
              <button type="button" onClick={() => { setDraft(toDraft(EMPTY_FILTERS)); onChange(EMPTY_FILTERS); }} className="rounded px-2 py-1 text-[10px] text-slate-400 hover:bg-white/5">クリア</button>
              <button type="submit" className={cn("rounded px-3 py-1 text-[10px] font-medium", "bg-amber-300 text-slate-950")}>適用</button>
            </div>
          </fieldset>
        </form>
      )}
    </section>
  );
}
