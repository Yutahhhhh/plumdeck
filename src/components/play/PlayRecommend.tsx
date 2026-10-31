import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Disc3, History, Loader2, RefreshCw } from "lucide-react";
import { assistService, EMPTY_FILTERS, type AssistCandidate, type AssistFilters, type AssistRecommendations, type GenreScope } from "@/services/assist";
import { getErrorDetail } from "@/services/api-client";
import type { Track } from "@/types";
import { cn } from "@/lib/utils";
import { RELATION_LABELS, filterByKey, formatKey, relationOf, setKeyFilterEnabled, trafficLevel, useKeyDisplay } from "@/lib/key-display";
import { PRESETS, presetLabel } from "@/components/assist/presets";
import { FilterPanel } from "@/components/assist/FilterPanel";
import { loadTempoTolerance, parseTempoTolerance, saveTempoTolerance, TEMPO_TOLERANCES, type TempoTolerance } from "@/components/assist/tempo-tolerance";
import { artworkUrl, useTrackVisuals } from "./useTrackVisuals";
import { keyCellStyle, deltaBpm } from "./VirtualTrackList";
import { usePlayTrackDrag } from "./PlayDragDrop";
import { RECOMMEND_MAX, RECOMMEND_PAGE, cautionsOf, historyBadge, nextRecommendLimit, parseRecommendSettings, recommendExcludeIds, scorePercent, type RecommendSettings } from "./play-recommend";
import "./play-recommend.css";

const SETTINGS_STORAGE = "plumdeck.play.recommend";
const readSettings = () => { try { return parseRecommendSettings(localStorage.getItem(SETTINGS_STORAGE)); } catch { return parseRecommendSettings(null); } };
const writeSettings = (value: RecommendSettings) => { try { localStorage.setItem(SETTINGS_STORAGE, JSON.stringify(value)); } catch { /* this session only */ } };

/**
 * Play's RECOMMEND panel on the Assist engine (POST /assist/recommendations):
 * presets, transition, the owner's rekordbox HISTORY, reasons and scores, with
 * picks that only need their file (Play loads them itself).
 */
export function PlayRecommend({ seedTrackId, masterKey, masterBpm, playedIds, onLoad }: {
  seedTrackId: number | null;
  masterKey: number | null;
  masterBpm: number | null;
  playedIds?: ReadonlySet<number>;
  onLoad: (track: Track) => void;
}) {
  const [settings, setSettingsState] = useState<RecommendSettings>(readSettings);
  const setSettings = (patch: Partial<RecommendSettings>) => setSettingsState(old => { const next = { ...old, ...patch }; writeSettings(next); return next; });
  const { intent, transition, genreScope, excludePlayed } = settings;
  const [tempoTolerance, setTempoTolerance] = useState<TempoTolerance>(loadTempoTolerance);
  const [filters, setFilters] = useState<AssistFilters>(EMPTY_FILTERS);
  const [revision, setRevision] = useState(0);
  const [page, setPage] = useState({ base: "", limit: RECOMMEND_PAGE });
  const [result, setResult] = useState<{ base: string; limit: number; value: AssistRecommendations } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showDetails, setShowDetails] = useState(false);
  const generation = useRef(0);
  const scrollRef = useRef<HTMLDivElement>(null);
  const keyDisplay = useKeyDisplay();

  const excludeIds = recommendExcludeIds(playedIds, excludePlayed, seedTrackId);
  const excludeKey = excludeIds.join(",");
  const filterKey = JSON.stringify(filters);
  const base = JSON.stringify([seedTrackId, intent, transition, genreScope, tempoTolerance, filterKey, excludeKey, revision]);
  const limit = page.base === base ? page.limit : RECOMMEND_PAGE;

  useEffect(() => {
    const current = ++generation.current;
    setError(null);
    if (!seedTrackId) { setResult(null); setBusy(false); return; }
    setBusy(true);
    void assistService.recommendations({
      sourceTrackId: seedTrackId, intent, transition, genreScope, limit, filters, tempoTolerance,
      excludeTrackIds: excludeKey ? excludeKey.split(",").map(Number) : [], availability: "file",
    }).then(value => { if (generation.current === current) setResult({ base, limit, value }); })
      .catch(failure => { if (generation.current === current) setError(getErrorDetail(failure)); })
      .finally(() => { if (generation.current === current) setBusy(false); });
    return () => { generation.current += 1; };
    // `filters` is tracked through filterKey (inside base).
  }, [base, limit]);

  // A new seed or new conditions start again at the top of the list.
  const resultBase = result?.base;
  useEffect(() => { if (scrollRef.current) scrollRef.current.scrollTop = 0; }, [seedTrackId, intent, transition, genreScope, tempoTolerance, filterKey]);

  const value = result?.value ?? null;
  const stale = resultBase !== base;
  const raw = value?.candidates ?? [];
  const candidates = keyDisplay.filter ? filterByKey(raw, masterKey) : raw;
  const hasMore = !stale && result !== null && result.limit < RECOMMEND_MAX && raw.length >= result.limit;
  const loadMore = () => { if (hasMore && !busy) setPage({ base, limit: nextRecommendLimit(limit) }); };
  const tempoLocked = transition || intent === "wordplay";
  const label = value ? `${presetLabel(value.intent)}${value.transition ? "・トランジション" : value.tempo_tolerance ? `・±${value.tempo_tolerance}%` : ""}` : presetLabel(intent);

  return (
    <div className="dj-reco">
      <div className="dj-recommend-subtitle">
        <span title="おすすめの方針">NEXT · {label}</span>
        <button className={cn("dj-button", keyDisplay.filter && "is-on")} title="キー相性で絞り込む（同じキー・±1・平行調。キー不明は最後）" onClick={() => setKeyFilterEnabled(!keyDisplay.filter)}>KEY</button>
        <button className="dj-reco-icon" aria-label="再読み込み" title="再読み込み" disabled={!seedTrackId} onClick={() => setRevision(v => v + 1)}><RefreshCw className={cn(busy && "animate-spin")} /></button>
        <b>{candidates.length}</b>
      </div>
      <div className="dj-reco-controls">
        <div className="dj-reco-presets" role="group" aria-label="フロアをどうしたい？">
          {PRESETS.map(item => <button key={item.value} type="button" aria-pressed={intent === item.value} title={item.detail} className={cn(intent === item.value && "is-on")} onClick={() => setSettings({ intent: item.value })}>{item.label}</button>)}
        </div>
        <div className="dj-reco-line">
          <label className={cn(intent === "wordplay" && "is-disabled")} title="倍テン・ハーフテンの曲と、タイトルに「100-128 Transition」などとあるテンポ変化曲から探します">
            <input type="checkbox" checked={transition && intent !== "wordplay"} disabled={intent === "wordplay"} onChange={event => setSettings({ transition: event.target.checked })} />トランジション
          </label>
          <button type="button" className="dj-reco-more" aria-expanded={showDetails} onClick={() => setShowDetails(v => !v)}>{showDetails ? <ChevronDown /> : <ChevronRight />}条件</button>
        </div>
        {showDetails && <div className="dj-reco-options">
          <select aria-label="テンポの許容幅" title="基準曲の BPM からどこまで離れた曲を候補にするか（Assist と共通）。トランジションとワードプレイには適用されません" value={tempoTolerance ?? ""} disabled={tempoLocked} onChange={event => { const next = parseTempoTolerance(event.target.value || null); setTempoTolerance(next); saveTempoTolerance(next); }}>
            <option value="">テンポ幅：標準</option>
            {TEMPO_TOLERANCES.map(v => <option key={v} value={v}>テンポ ±{v}%</option>)}
          </select>
          <select aria-label="ジャンルの範囲" value={genreScope} onChange={event => setSettings({ genreScope: event.target.value as GenreScope })}>
            <option value="any">同系統を優先</option><option value="same_genre">同一ジャンル</option><option value="same_subgenre">同一サブジャンル</option>
          </select>
          <label title="このセッションで再生した曲を候補から外します"><input type="checkbox" checked={excludePlayed} onChange={event => setSettings({ excludePlayed: event.target.checked })} />再生済みを除外{playedIds?.size ? `（${playedIds.size}）` : ""}</label>
          <FilterPanel value={filters} onChange={setFilters} disabled={false} />
        </div>}
      </div>
      <div ref={scrollRef} className={cn("dj-recommend-scroll", stale && busy && "is-stale")} aria-busy={busy} onScroll={event => { const node = event.currentTarget; if (node.scrollHeight - node.scrollTop - node.clientHeight < 120) loadMore(); }}>
        {!seedTrackId ? <div className="dj-library-message">デッキに曲をロードしてください</div>
          : error ? <div className="dj-library-message"><button className="dj-button" title={error} onClick={() => setRevision(v => v + 1)}>候補を取得できませんでした · 再試行</button></div>
          : !value && busy ? <div className="dj-library-message"><Loader2 className="animate-spin" />次の一曲を探しています…</div>
          : null}
        {seedTrackId && !error && candidates.map(track => <RecoItem key={track.id} track={track} baseKey={masterKey} masterBpm={masterBpm} onLoad={onLoad} />)}
        {seedTrackId && !error && value && !stale && !candidates.length && <div className="dj-library-message">{raw.length ? "キー相性に合う候補がありません" : value.notes[0] ?? "この条件では候補がありません。プリセットや条件を変えてお試しください"}</div>}
        {seedTrackId && !error && value && <div className="dj-page-state">{busy && !stale ? <Loader2 className="animate-spin" /> : hasMore ? <button onClick={loadMore}>さらに表示</button> : null}</div>}
        {seedTrackId && !error && value && value.alternatives.length > 0 && <>
          <div className="dj-reco-section">他のプリセットなら</div>
          {value.alternatives.map(item => <RecoItem key={`${item.intent}-${item.track.id}`} track={item.track} badge={item.label.replace(/なら$/, "")} baseKey={masterKey} masterBpm={masterBpm} onLoad={onLoad} />)}
        </>}
        {seedTrackId && !error && value && <details className="dj-reco-about">
          <summary>詳細</summary>
          <p>採点：{value.basis}</p>
          {value.filters.length > 0 && <p>絞り込み：{value.filters.join(" / ")}</p>}
          {value.caveats.map((caveat, i) => <p key={`c${i}`} className="is-caution">・{caveat}</p>)}
          {value.notes.map((note, i) => <p key={`n${i}`}>{note}</p>)}
          <p>自分の rekordbox 履歴でこの曲の次によくかけた曲ほど上位に来ます。「〇〇っぽい曲」などの依頼は MCP のチャットエージェントからどうぞ。</p>
        </details>}
      </div>
    </div>
  );
}

function RecoItem({ track, badge, baseKey, masterBpm, onLoad }: { track: AssistCandidate; badge?: string; baseKey: number | null; masterBpm: number | null; onLoad: (track: Track) => void }) {
  const [open, setOpen] = useState(false);
  const { data } = useTrackVisuals(track.id);
  const keys = useKeyDisplay();
  const relation = relationOf(baseKey, track.key);
  const drag = usePlayTrackDrag(`play-reco-${badge ?? "main"}-${track.id}`, track);
  const delta = deltaBpm(track.bpm, masterBpm);
  const score = scorePercent(track.score);
  const history = historyBadge(track.history);
  const cautions = cautionsOf(track);
  return (
    <div className={cn("dj-reco-item", open && "is-open")}>
      <div ref={drag.setNodeRef} {...drag.attributes} {...drag.listeners} className={cn("dj-recommend-row", drag.isDragging && "is-dragging")} onDoubleClick={() => track.filepath && onLoad(track)} title={track.summary}>
        <button type="button" className="dj-reco-toggle" aria-expanded={open} aria-label={open ? "理由を閉じる" : "理由を見る"} onPointerDown={event => event.stopPropagation()} onClick={() => setOpen(v => !v)}>{open ? <ChevronDown /> : <ChevronRight />}</button>
        <div className="dj-recommend-cover">{data?.artwork ? <img src={artworkUrl(data.artwork)} alt="" /> : <Disc3 />}</div>
        <div className="dj-recommend-track">
          <strong>{badge && <i className="dj-reco-badge">{badge}</i>}{track.title || "—"}</strong>
          <span>{track.artist || "—"}</span>
          <small>{track.bpm?.toFixed(1) || "—"}{delta !== null && <em className={cn("dj-reco-delta", Math.abs(delta) > 6 && "is-far")}> {delta > 0 ? "+" : ""}{delta.toFixed(1)}</em>} <b style={keyCellStyle(trafficLevel(relation, keys.level2))}>{formatKey(track.key, keys.notation)}</b>{relation && RELATION_LABELS[relation] ? <em> {RELATION_LABELS[relation]}</em> : null}</small>
        </div>
        <div className="dj-reco-score">
          {score !== null && <span title="このプリセットへの合い具合">{score}</span>}
          {history && <small title="自分の rekordbox 履歴"><History />{history}</small>}
          {!history && cautions.length > 0 && <small className="is-caution" title={cautions.map(reason => reason.text).join("\n")}>注意</small>}
        </div>
        <button disabled={!track.filepath} onClick={() => onLoad(track)} title="アクティブデッキにロード">↗</button>
      </div>
      {open && <div className="dj-reco-detail">
        {track.agent_reason && <p className="is-agent">{track.agent_reason}</p>}
        <p>{track.summary}</p>
        {track.reasons.map((reason, i) => <p key={i} className={cn(reason.tone === "caution" && "is-caution", reason.tone === "good" && "is-good")}>{reason.text}</p>)}
        {track.wordplay?.transition_notes && <p className="is-wordplay">{track.wordplay.transition_notes}</p>}
      </div>}
    </div>
  );
}
