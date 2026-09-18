import { useEffect, useRef, useState } from "react";
import { Bot, Pin, PinOff, RefreshCw, Sparkles, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { assistService, EMPTY_FILTERS, hasFilters, type AssistAgentSettings, type AssistFilters, type AssistIntent, type AssistRecommendations, type AssistSearchResult, type GenreScope } from "@/services/assist";
import { getErrorDetail } from "@/services/api-client";
import { useCompactWindow } from "./useCompactWindow";
import { useRekordboxDecks } from "./useRekordboxDecks";
import { StatusBanner } from "./StatusBanner";
import { startFileDrag } from "./native-drag";
import { CandidateCard } from "./CandidateCard";
import { FilterPanel } from "./FilterPanel";
import { useAgentBridge } from "./useAgentBridge";

import { useLoadedHistory } from "./useLoadedHistory";
import { MAX_EXCLUDED_TRACKS } from "./loaded-history";

const presets: { value: AssistIntent; label: string; detail: string }[] = [
  { value: "keep", label: "キープ", detail: "流れを保つ" },
  { value: "hype", label: "盛り上げる", detail: "熱量を上げる" },
  { value: "dance", label: "踊らせる", detail: "グルーヴへ" },
  { value: "calm", label: "落ち着かせる", detail: "クールダウン" },
  { value: "emotional", label: "エモく", detail: "切なく・深く" },
  { value: "bright", label: "明るく", detail: "開放感へ" },
  { value: "throwback", label: "時代を戻す", detail: "懐かしい曲" },
  { value: "wordplay", label: "ワードプレイ", detail: "言葉でつなぐ" },
];
const limits = [5, 8, 12, 20, 30, 50];

type Result =
  | { kind: "recommend"; key: string; value: AssistRecommendations }
  | { kind: "search"; key: string; value: AssistSearchResult };

export function AssistWorkspace() {
  const compact = useCompactWindow(true);
  const freeze = useRef(false);
  const [dragging, setDragging] = useState(false);
  const decks = useRekordboxDecks(true, dragging, freeze);
  const [slot, setSlot] = useState(1);
  const [intent, setIntent] = useState<AssistIntent>("keep");
  const [transition, setTransition] = useState(false);
  const [genreScope, setGenreScope] = useState<GenreScope>("any");
  const [limit, setLimit] = useState(8);
  const [filters, setFilters] = useState<AssistFilters>(EMPTY_FILTERS);
  const history = useLoadedHistory(decks.decks, dragging, freeze);
  const [result, setResult] = useState<Result | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dragMessage, setDragMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  const generation = useRef(0);
  const selected = decks.decks.find(deck => deck.slot === slot);
  const sourceId = selected?.status === "resolved" ? selected.track?.id : undefined;
  const excluded = history.excludedIds.join(",");
  const historyFull = history.excludedIds.length > MAX_EXCLUDED_TRACKS;
  const filtered = hasFilters(filters);
  const filterKey = JSON.stringify(filters);
  const requestKey = JSON.stringify([sourceId, intent, transition, genreScope, limit, filterKey, excluded, revision]);

  useEffect(() => {
    const current = ++generation.current;
    if (freeze.current) return;
    setError(null);
    setBusy(false);
    // Without a resolved deck there is nothing to mix from, so the conditions
    // alone become a search.
    if (historyFull || (!sourceId && !filtered)) { setResult(null); return; }
    // visibleResult already hides changed requests; retain same-request results
    // while rechecking after a drag, so the list never flashes empty.
    setBusy(true);
    const excludeTrackIds = excluded.split(",").filter(Boolean).map(Number);
    const request: Promise<Result> = sourceId
      ? assistService.recommendations({ sourceTrackId: sourceId, intent, transition, genreScope, excludeTrackIds, limit, filters })
        .then(value => ({ kind: "recommend" as const, key: requestKey, value }))
      : assistService.search({ filters, limit, excludeTrackIds })
        .then(value => ({ kind: "search" as const, key: requestKey, value }));
    void request
      .then(value => { if (generation.current === current && !freeze.current) setResult(value); })
      .catch(failure => { if (generation.current === current && !freeze.current) { setResult(null); setError(getErrorDetail(failure)); } })
      .finally(() => { if (generation.current === current && !freeze.current) setBusy(false); });
    return () => { generation.current += 1; };
    // `filters` itself is tracked through filterKey (and requestKey).
  }, [sourceId, intent, transition, genreScope, limit, filterKey, excluded, revision, dragging, historyFull, requestKey, filtered]);

  const refresh = () => {
    if (freeze.current) return;
    generation.current += 1;
    setResult(null);
    decks.refresh();
    setRevision(value => value + 1);
  };
  const drag = async (filepath: string, label: string) => {
    if (freeze.current) return;
    freeze.current = true;
    generation.current += 1;
    setDragging(true);
    setDragMessage(null);
    try {
      const outcome = await startFileDrag(filepath, label);
      setDragMessage(outcome.message ?? null);
    } finally {
      decks.refresh();
      freeze.current = false;
      setDragging(false);
    }
  };
  const slots = Array.from(new Set([1, 2, ...(decks.snapshot?.decks.map(deck => deck.slot) ?? [])])).sort();
  const observations = decks.snapshot?.decks ?? [];
  const observed = observations.find(deck => deck.slot === slot);
  const sourceTitle = selected?.track?.title ?? observed?.title;
  const sourceArtist = selected?.track?.artist ?? observed?.artist;
  const visible = result?.key === requestKey ? result : null;
  const recommended = visible?.kind === "recommend" ? visible.value : null;
  const candidates = visible?.value.candidates ?? [];
  const presetLabel = presets.find(item => item.value === intent)?.label ?? "";

  const presetOf = (value: AssistIntent) => presets.find(item => item.value === value)?.label ?? value;
  const applyAgentSettings = (settings: AssistAgentSettings) => {
    if (freeze.current) return false;
    if (settings.intent) setIntent(settings.intent);
    if (settings.transition !== undefined) setTransition(settings.transition);
    if (settings.genre_scope) setGenreScope(settings.genre_scope);
    if (settings.limit) setLimit(Math.min(50, Math.max(1, settings.limit)));
    if (settings.filters) setFilters({ ...EMPTY_FILTERS, ...settings.filters });
    return true;
  };
  const agent = useAgentBridge({
    deck_slot: slot,
    source_track_id: sourceId ?? null,
    intent,
    transition,
    genre_scope: genreScope,
    limit,
    filters,
    exclude_track_ids: history.excludedIds,
    shown_track_ids: candidates.map(track => track.id),
  }, applyAgentSettings);
  const agentChange = agent.lastSettings && [
    agent.lastSettings.intent ? `プリセット「${presetOf(agent.lastSettings.intent)}」` : null,
    agent.lastSettings.transition !== undefined ? `トランジション${agent.lastSettings.transition ? "オン" : "オフ"}` : null,
    agent.lastSettings.filters ? "絞り込み条件" : null,
    agent.lastSettings.limit ? `${agent.lastSettings.limit}件` : null,
  ].filter(Boolean).join("・");

  return (
    <main className="flex h-full min-h-0 w-full flex-col bg-[#0c1018] text-slate-100">
      <div className="flex shrink-0 items-center justify-between border-b border-white/5 px-4 py-2">
        <div><div className="flex items-center gap-2 text-sm font-semibold"><Sparkles className="size-4 text-amber-300" /> 次の一曲</div></div>
        <div className="flex gap-1">
          <button
            type="button"
            aria-label={compact.alwaysOnTop ? "ウィンドウの固定を解除" : "ウィンドウを最前面に固定"}
            aria-pressed={compact.alwaysOnTop}
            title={compact.alwaysOnTop ? "最前面の固定を解除します" : "他のウィンドウより手前に表示します"}
            disabled={!compact.supported || dragging}
            onClick={() => compact.setAlwaysOnTop(!compact.alwaysOnTop)}
            className={cn("flex items-center gap-1.5 rounded-md border px-2.5 py-1.5 text-[11px] font-medium disabled:opacity-30", compact.alwaysOnTop ? "border-amber-300/40 bg-amber-300/10 text-amber-300" : "border-slate-700 text-slate-400 hover:bg-white/5")}
          >
            {compact.alwaysOnTop ? <PinOff className="size-3.5" /> : <Pin className="size-3.5" />}
            {compact.alwaysOnTop ? "固定解除" : "最前面に固定"}
          </button>
          <button type="button" aria-label="再読み込み" title="再読み込み" disabled={dragging} onClick={refresh} className="rounded-md p-2 text-slate-400 hover:bg-white/5 disabled:opacity-30"><RefreshCw className={cn("size-4", busy && "animate-spin")} /></button>
        </div>
      </div>
      <div className="min-h-0 flex-1 space-y-2 overflow-y-auto px-3 py-2">
        <fieldset disabled={dragging}><StatusBanner snapshot={decks.snapshot} libraryError={decks.resolution?.library_error ?? null} readError={decks.error} windowMessage={compact.message} onRefresh={refresh} /></fieldset>
        <section aria-label="基準デッキ" className="shrink-0 rounded-xl border border-white/10 bg-white/[0.025] p-2.5">
          <div className="mb-1 flex items-center justify-between"><span className="text-[10px] text-slate-400">基準デッキ</span><div className="flex gap-1">{slots.map(value => <button type="button" key={value} disabled={dragging} aria-pressed={slot === value} onClick={() => setSlot(value)} className={cn("rounded px-3 py-1 text-xs transition-colors disabled:opacity-50", slot === value ? "bg-amber-300 text-slate-950" : "bg-white/5 text-slate-400 hover:bg-white/10")}>{value}</button>)}</div></div>
          <p className="truncate text-sm font-semibold">{sourceTitle || (decks.loading ? "デッキを確認中…" : "曲が読み込まれていません")}</p>
          <div className="mt-1 flex items-center justify-between gap-2"><p className="truncate text-[11px] text-slate-400">{sourceArtist || "—"}</p>
          {sourceTitle && <p className="shrink-0 text-[10px] tabular-nums text-slate-400">{observed?.tempo_bpm ?? observed?.track_bpm ?? selected?.track?.bpm ?? "—"} BPM <span className="mx-2 text-slate-700">/</span>{observed?.display_key ?? selected?.track?.key ?? "—"}</p>}</div>
          {selected?.message && <p className="mt-2 text-[11px] text-amber-200">{selected.message}</p>}
          {!sourceId && sourceTitle && !selected && <p className="mt-2 text-[11px] text-slate-500">ライブラリの曲と照合中…</p>}
        </section>

        {agent.list && (
          <section aria-label="エージェントの提案" className="space-y-2 rounded-xl border border-sky-300/25 bg-sky-300/[0.04] p-2.5">
            <div className="flex items-start justify-between gap-2">
              <div className="min-w-0">
                <p className="flex items-center gap-1.5 text-[11px] font-semibold text-sky-100"><Bot className="size-3.5 shrink-0 text-sky-300" /><span className="truncate">{agent.list.title}</span></p>
                <p className="text-[9px] text-sky-200/60">{agent.list.agent_name ?? "AIエージェント"}の提案 · {agent.list.tracks.length}曲</p>
              </div>
              <button type="button" aria-label="提案を閉じる" title="提案を閉じる" disabled={dragging} onClick={() => void agent.dismiss()} className="rounded p-1 text-slate-400 hover:bg-white/5 hover:text-slate-200 disabled:opacity-40"><X className="size-3.5" /></button>
            </div>
            {agent.list.note && <p className="whitespace-pre-wrap text-[10px] leading-relaxed text-slate-300">{agent.list.note}</p>}
            {agent.list.tracks.map((track, index) => <CandidateCard key={`agent-${agent.list?.revision}-${track.id}`} track={track} badge={String(index + 1).padStart(2, "0")} dragging={dragging} freeze={freeze} onDrag={(filepath, label) => void drag(filepath, label)} />)}
            {agent.list.rejected.length > 0 && <p className="text-[10px] text-slate-500">デッキに載せられない {agent.list.rejected.length} 曲は表示していません（{agent.list.rejected[0].reason}）</p>}
          </section>
        )}
        {agentChange && <p role="status" className="flex items-center gap-1.5 text-[10px] text-sky-200/80"><Bot className="size-3 shrink-0" />{agent.lastSettings?.agent_name ?? "AIエージェント"}が{agentChange}に変更しました</p>}
        <fieldset disabled={dragging} className="space-y-2">
          <legend className="mb-1 text-[10px] text-slate-400">フロアをどうしたい？</legend>
          <div className="grid grid-cols-4 gap-1.5">{presets.map(item => <button type="button" key={item.value} aria-pressed={intent === item.value} onClick={() => setIntent(item.value)} className={cn("rounded-lg border px-1 py-1.5 text-center", intent === item.value ? "border-amber-300/50 bg-amber-300/10 text-amber-200" : "border-white/10 text-slate-400 hover:bg-white/5")}><span className="block text-[11px] font-medium leading-tight">{item.label}</span><span className="mt-0.5 block text-[9px] opacity-70">{item.detail}</span></button>)}</div>
          <div className="flex items-center justify-between gap-2">
            <label className={cn("flex items-center gap-1.5 text-[10px]", intent === "wordplay" ? "text-slate-600" : "text-slate-300")} title="倍テン・ハーフテンの曲と、タイトルに「100-128 Transition」などとあるテンポ変化曲から探します">
              <input type="checkbox" checked={transition && intent !== "wordplay"} disabled={intent === "wordplay"} onChange={event => setTransition(event.target.checked)} className="accent-amber-300" />
              トランジション（テンポを移る）
            </label>
            <div className="flex items-center gap-1.5">
              <select aria-label="ジャンルの範囲" value={genreScope} onChange={event => setGenreScope(event.target.value as GenreScope)} className="rounded border border-slate-700 bg-slate-900 px-1.5 py-1 text-[10px] text-slate-300"><option value="any">ジャンル指定なし</option><option value="same_genre">同一ジャンル</option><option value="same_subgenre">同一サブジャンル</option></select>
              <select aria-label="おすすめの件数" value={limit} onChange={event => setLimit(Number(event.target.value))} className="rounded border border-slate-700 bg-slate-900 px-1.5 py-1 text-[10px] text-slate-300">{limits.map(value => <option key={value} value={value}>{value}件</option>)}</select>
            </div>
          </div>
        </fieldset>
        <FilterPanel value={filters} onChange={setFilters} disabled={dragging} />
        <div className="flex items-center justify-between text-[10px]"><span className="text-slate-500">ロード済み {history.excludedIds.length}曲を除外</span><button type="button" disabled={dragging} onClick={() => { if (freeze.current) return; generation.current += 1; setResult(null); history.reset(); setRevision(value => value + 1); }} className="text-slate-400 underline underline-offset-2 hover:text-slate-200 disabled:opacity-40">履歴をリセット</button></div>
        {history.message && <p role="status" className="text-[10px] text-slate-400">{history.message}</p>}
        {historyFull && <p role="alert" className="text-[11px] text-amber-200">除外履歴が10,000曲を超えました。おすすめを再開するには履歴をリセットしてください。</p>}
        <section aria-label="おすすめの曲" aria-busy={busy} className="space-y-2">
          <div className="flex justify-between text-[10px] text-slate-500"><span>{visible?.kind === "search" ? "条件に合う曲" : `おすすめ（${presetLabel}${recommended?.transition ? "・トランジション" : ""}）`} {visible ? `· ${candidates.length}` : ""}</span><span>{dragging ? "ドラッグ中 · 候補を固定" : "曲をデッキの曲名へドラッグ"}</span></div>
          {recommended && <p className="text-[9px] text-slate-600" title="この観点の採点の内訳">採点：{recommended.basis}</p>}
          {visible && visible.value.filters.length > 0 && <p className="text-[10px] text-slate-500">絞り込み：{visible.value.filters.join(" / ")}</p>}
          {recommended && recommended.caveats.length > 0 && <div role="note" className="space-y-0.5 rounded-lg border border-amber-300/20 bg-amber-300/5 px-2.5 py-1.5 text-[10px] leading-relaxed text-amber-100/80">{recommended.caveats.map((caveat, i) => <p key={i}>・{caveat}</p>)}</div>}
          {error && <p role="alert" className="rounded-lg bg-rose-950/40 p-3 text-xs text-rose-200">候補を取得できませんでした。条件を確認するか、再読み込みでお試しください。<span className="mt-1 block text-[10px] opacity-70">{error}</span></p>}
          {dragMessage && <p role="status" className="text-xs text-amber-200">{dragMessage}</p>}
          {busy && !visible && <p className="py-6 text-center text-xs text-slate-500">次の一曲を探しています…</p>}
          {!busy && !error && !sourceId && !filtered && <p className="py-5 text-center text-xs text-slate-500">基準デッキの曲が確認できると候補が表示されます。デッキなしでも「条件で絞り込む」で検索できます。</p>}
          {candidates.map((track, index) => <CandidateCard key={track.id} track={track} badge={String(index + 1).padStart(2, "0")} dragging={dragging} freeze={freeze} onDrag={(filepath, label) => void drag(filepath, label)} />)}
          {visible && !candidates.length && <p className="py-4 text-center text-xs text-slate-400">条件に合う曲がありません。プリセットや条件を変えてお試しください。</p>}
          {visible?.value.notes.map((note, i) => <p key={i} className="text-[10px] leading-relaxed text-slate-500">{note}</p>)}
        </section>
        {recommended && recommended.alternatives.length > 0 && (
          <section aria-label="他のプリセットなら" className="space-y-2 pt-1">
            <p className="text-[10px] text-slate-500">他のプリセットなら</p>
            {recommended.alternatives.map(item => <CandidateCard key={`${item.intent}-${item.track.id}`} track={item.track} badge={item.label.replace(/なら$/, "")} dragging={dragging} freeze={freeze} onDrag={(filepath, label) => void drag(filepath, label)} />)}
          </section>
        )}
        <details className="text-[10px] text-slate-500">
          <summary className="w-fit cursor-pointer hover:text-slate-300">AIエージェントから操作する</summary>
          <p className="pt-1 leading-relaxed">Claude Code や Codex に plumdeck の MCP を登録すると（MCP 画面を参照）、「もっと盛り上げたい、2000年代で」「Drake みたいな雰囲気で」のような依頼から、この画面の条件を変えたり、エージェントが選んだ曲をここに表示したりできます。表示される曲は rekordbox に登録済みでファイルがあるものだけです。</p>
        </details>
      </div>
    </main>
  );
}
