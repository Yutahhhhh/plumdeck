import { useEffect, useId, useMemo, useState } from "react";
import { X } from "lucide-react";
import { cn } from "@/lib/utils";
import { assistService, EMPTY_FILTERS, hasFilters, type AssistArtist, type AssistFilters } from "@/services/assist";
import { genreService } from "@/services/genres";
import { addArtist, matchArtists } from "./artist-options";

interface Draft {
  bpmMin: string;
  bpmMax: string;
  genres: string[];
  /** Selected artists stay in the draft while the checkbox is off, so turning it back on restores them. */
  artistsOn: boolean;
  artists: string[];
  artistQuery: string;
  keys: string;
  yearMin: string;
  yearMax: string;
  query: string;
}

const toDraft = (filters: AssistFilters): Draft => ({
  bpmMin: filters.bpm_min?.toString() ?? "",
  bpmMax: filters.bpm_max?.toString() ?? "",
  genres: filters.genres,
  artistsOn: filters.artists.length > 0,
  artists: filters.artists,
  artistQuery: "",
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
    // Text typed but not yet picked still counts, so 「適用」 never drops it.
    artists: draft.artistsOn ? addArtist(draft.artists, draft.artistQuery) : [],
    keys: list(draft.keys),
    year_min: number(draft.yearMin),
    year_max: number(draft.yearMax),
    query: draft.query.trim(),
  };
}

/** Compound conditions, applied on 「適用」 so typing never refetches mid-word. */
export function FilterPanel({ value, onChange, disabled, sourceArtist }: { value: AssistFilters; onChange: (filters: AssistFilters) => void; disabled: boolean; sourceArtist?: string | null }) {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState(() => toDraft(value));
  const [genres, setGenres] = useState<string[]>([]);
  const [artists, setArtists] = useState<AssistArtist[] | null>(null);
  const active = hasFilters(value);

  // Applying without artists keeps the picked ones in the draft (unchecked), so
  // checking the box again restores them instead of starting over.
  useEffect(() => {
    setDraft(current => value.artists.length ? toDraft(value) : { ...toDraft(value), artists: current.artists });
  }, [value]);
  useEffect(() => {
    if (!open || genres.length) return;
    genreService.getAllGenres().then(setGenres).catch(() => setGenres([]));
  }, [open, genres.length]);
  useEffect(() => {
    if (!open || !draft.artistsOn || artists) return;
    assistService.artists().then(setArtists).catch(() => setArtists([]));
  }, [open, draft.artistsOn, artists]);

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
              <label htmlFor="assist-keys">キー</label>
              <input id="assist-keys" placeholder="8A, 9A / A minor" value={draft.keys} onChange={event => set({ keys: event.target.value })} className={input} />
              <label htmlFor="assist-query">キーワード</label>
              <input id="assist-query" placeholder="曲名・アーティスト・アルバム" value={draft.query} onChange={event => set({ query: event.target.value })} className={input} />
            </div>
            <div className="space-y-1.5 text-[10px] text-slate-500">
              <label className="flex w-fit items-center gap-1.5 text-slate-300">
                <input type="checkbox" checked={draft.artistsOn} onChange={event => set({ artistsOn: event.target.checked })} className="accent-amber-300" />
                アーティストで絞り込む
              </label>
              {draft.artistsOn && <ArtistPicker artists={artists} selected={draft.artists} query={draft.artistQuery} sourceArtist={sourceArtist ?? null} onChange={patch => set(patch)} inputClassName={input} />}
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

/** Type to narrow the library's artists; Enter picks the highlighted one, or the typed text as is. */
function ArtistPicker({ artists, selected, query, sourceArtist, onChange, inputClassName }: {
  artists: AssistArtist[] | null;
  selected: string[];
  query: string;
  sourceArtist: string | null;
  onChange: (patch: { artists?: string[]; artistQuery?: string }) => void;
  inputClassName: string;
}) {
  const listId = useId();
  const [listOpen, setListOpen] = useState(false);
  const [highlight, setHighlight] = useState(0);
  // An empty Enter applies the form; it picks from the list only after the DJ moved through it.
  const [navigated, setNavigated] = useState(false);
  const options = useMemo(() => artists ? matchArtists(artists, query, selected) : [], [artists, query, selected]);
  const active = Math.min(highlight, Math.max(0, options.length - 1));
  const showList = listOpen && (artists === null || options.length > 0);
  const pick = (name: string) => {
    onChange({ artists: addArtist(selected, name), artistQuery: "" });
    setHighlight(0);
    setNavigated(false);
  };
  useEffect(() => {
    if (showList && navigated) document.getElementById(`${listId}-${active}`)?.scrollIntoView({ block: "nearest" });
  }, [showList, navigated, active, listId]);
  const canAddSource = !!sourceArtist && addArtist(selected, sourceArtist) !== selected;

  return (
    <div className="space-y-1.5">
      <div className="relative">
        <input
          role="combobox"
          aria-label="アーティストを追加"
          aria-expanded={showList}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={showList && options.length ? `${listId}-${active}` : undefined}
          placeholder={artists === null ? "アーティストを読み込み中…" : "名前を入力して選択（複数可）"}
          value={query}
          onChange={event => { onChange({ artistQuery: event.target.value }); setHighlight(0); setNavigated(false); setListOpen(true); }}
          onFocus={() => setListOpen(true)}
          onBlur={() => { setListOpen(false); setNavigated(false); }}
          onKeyDown={event => {
            // Enter that confirms a Japanese IME conversion is not a pick.
            if (event.nativeEvent.isComposing || event.keyCode === 229) return;
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              setListOpen(true);
              if (!options.length) return;
              // The first press lands on the top entry rather than skipping past it.
              setHighlight(navigated ? (active + (event.key === "ArrowDown" ? 1 : options.length - 1)) % options.length : 0);
              setNavigated(true);
            } else if (event.key === "Enter" && (query.trim() || (showList && navigated && options.length))) {
              // Picking is not applying: the form submits only on an Enter with nothing to pick.
              event.preventDefault();
              const highlighted = showList && options.length && (navigated || query.trim());
              pick(highlighted ? options[active].name : query);
            } else if (event.key === "Escape" && listOpen) {
              event.preventDefault();
              setListOpen(false);
            } else if (event.key === "Backspace" && !query && selected.length) {
              onChange({ artists: selected.slice(0, -1) });
            }
          }}
          className={inputClassName}
        />
        {showList && (
          // Dragging the list's scrollbar must not blur the input and close the list.
          <ul id={listId} role="listbox" aria-label="アーティスト一覧" onMouseDown={event => event.preventDefault()} className="absolute inset-x-0 top-full z-10 mt-0.5 max-h-48 overflow-y-auto rounded border border-slate-700 bg-slate-900 py-0.5 shadow-lg">
            {artists === null && <li className="px-2 py-1 text-[10px] text-slate-500">読み込み中…</li>}
            {options.map((artist, index) => (
              <li
                key={artist.name}
                id={`${listId}-${index}`}
                role="option"
                aria-selected={index === active}
                // Keep focus in the input so the list does not close before the pick.
                onMouseDown={event => { event.preventDefault(); pick(artist.name); }}
                onMouseEnter={() => { setHighlight(index); setNavigated(true); }}
                className={cn("flex cursor-pointer items-center justify-between gap-2 px-2 py-1 text-[11px]", index === active ? "bg-amber-300/10 text-amber-100" : "text-slate-300")}
              >
                <span className="truncate">{artist.name}</span>
                <span className="shrink-0 text-[9px] tabular-nums text-slate-500">{artist.tracks}曲</span>
              </li>
            ))}
          </ul>
        )}
      </div>
      {(selected.length > 0 || canAddSource) && (
        <div className="flex flex-wrap gap-1">
          {selected.map(name => <button type="button" key={name} aria-label={`${name} を外す`} onClick={() => onChange({ artists: selected.filter(item => item !== name) })} className="flex items-center gap-0.5 rounded bg-white/10 px-1.5 py-0.5 text-[10px] text-slate-200">{name}<X className="size-3" /></button>)}
          {canAddSource && <button type="button" onClick={() => pick(sourceArtist as string)} className="rounded border border-dashed border-white/15 px-1.5 py-0.5 text-[10px] text-slate-400 hover:bg-white/5">＋ 基準曲の {sourceArtist}</button>}
        </div>
      )}
      <p className="text-[9px] leading-relaxed text-slate-500">名前を含む曲が対象です（「feat.」や共作も含みます）。</p>
    </div>
  );
}
