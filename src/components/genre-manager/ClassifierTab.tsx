import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Bot, Brain, Loader2, RefreshCw, Sparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Progress } from "@/components/ui/progress";
import { ScrollArea } from "@/components/ui/scroll-area";
import { PlayButton } from "@/components/ui/PlayButton";
import {
  AnalysisMode,
  ClassifierStatus,
  ConfidenceLevel,
  genreService,
  PredictedTrack,
  PredictionPage,
} from "@/services/genres";
import { aiService, AiGenreSuggestion } from "@/services/ai";

type Filter = ConfidenceLevel | "all";

interface Choice {
  genre: string;
  subgenre: string | null;
  source: "classifier" | "ai";
}

const LEVEL_LABEL: Record<ConfidenceLevel, string> = { high: "高", medium: "中", low: "低" };
const LEVEL_CLASS: Record<ConfidenceLevel, string> = {
  high: "bg-green-500/15 text-green-700 dark:text-green-400",
  medium: "bg-amber-500/15 text-amber-700 dark:text-amber-400",
  low: "bg-red-500/15 text-red-700 dark:text-red-400",
};
const PAGE = 200;

const percent = (value: number | null | undefined) => (value == null ? "–" : `${Math.round(value * 100)}%`);

function label(choice: Choice, mode: AnalysisMode) {
  if (mode === "genre" || !choice.subgenre) return choice.genre;
  return `${choice.genre} / ${choice.subgenre}`;
}

/** Options for one track: classifier pairs first, then bare genres. */
function options(item: PredictedTrack): Choice[] {
  const seen = new Set<string>();
  const out: Choice[] = [];
  const push = (genre: string, subgenre: string | null) => {
    const key = `${genre}\u001f${subgenre ?? ""}`;
    if (seen.has(key)) return;
    seen.add(key);
    out.push({ genre, subgenre, source: "classifier" });
  };
  push(item.prediction.genre, item.prediction.subgenre);
  item.prediction.pair_candidates.forEach((c) => push(c.genre, c.subgenre ?? null));
  item.prediction.candidates.forEach((c) => push(c.genre, null));
  return out;
}

export function ClassifierTab({ mode = "both" }: { mode?: AnalysisMode }) {
  const [status, setStatus] = useState<ClassifierStatus | null>(null);
  const [page, setPage] = useState<PredictionPage | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [choices, setChoices] = useState<Record<number, Choice>>({});
  const [ai, setAi] = useState<Record<number, AiGenreSuggestion>>({});
  const [aiReady, setAiReady] = useState(false);
  const [asking, setAsking] = useState(false);
  const [message, setMessage] = useState<{ text: string; error: boolean } | null>(null);
  const [showDetails, setShowDetails] = useState(false);
  const wasTraining = useRef(false);

  const loadStatus = useCallback(async () => {
    try {
      const next = await genreService.getClassifierStatus();
      setStatus(next);
      return next;
    } catch {
      return null;
    }
  }, []);

  const loadPredictions = useCallback(
    async (limit = PAGE) => {
      setLoading(true);
      try {
        setPage(await genreService.getPredictions(filter, 0, limit));
      } catch {
        setMessage({ text: "候補を読み込めませんでした", error: true });
      } finally {
        setLoading(false);
      }
    },
    [filter]
  );

  useEffect(() => {
    loadStatus();
    aiService
      .getSettings()
      .then((s) => setAiReady(s.ready))
      .catch(() => setAiReady(false));
  }, [loadStatus]);

  useEffect(() => {
    loadPredictions();
  }, [loadPredictions]);

  // Poll while training; reload the list when it finishes.
  useEffect(() => {
    const training = status?.state === "training";
    if (!training && wasTraining.current) {
      wasTraining.current = false;
      loadPredictions();
    }
    if (!training) return;
    wasTraining.current = true;
    const timer = window.setTimeout(loadStatus, 1500);
    return () => window.clearTimeout(timer);
  }, [status, loadStatus, loadPredictions]);

  const items = page?.items ?? [];
  const choiceFor = (item: PredictedTrack): Choice =>
    choices[item.track.id] ?? { genre: item.prediction.genre, subgenre: item.prediction.subgenre, source: "classifier" };

  const train = async () => {
    setMessage(null);
    try {
      await genreService.trainClassifier();
    } catch {
      // Already training: the status shows it.
    }
    await loadStatus();
  };

  const toggle = (id: number) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const allSelected = items.length > 0 && items.every((i) => selected.has(i.track.id));
  const toggleAll = () => setSelected(allSelected ? new Set() : new Set(items.map((i) => i.track.id)));
  const selectHigh = () => setSelected(new Set(items.filter((i) => i.prediction.level === "high").map((i) => i.track.id)));

  const accept = async () => {
    const chosen = items.filter((i) => selected.has(i.track.id));
    if (!chosen.length) return;
    setMessage(null);
    try {
      const result = await genreService.acceptPredictions(
        chosen.map((item) => {
          const choice = choiceFor(item);
          return { track_id: item.track.id, genre: choice.genre, subgenre: mode === "genre" ? null : choice.subgenre };
        })
      );
      setSelected(new Set());
      setMessage({
        text: `${result.updated} 曲のジャンルを確定しました${result.retraining ? "。新しいラベルで学習し直しています" : ""}`,
        error: false,
      });
      await Promise.all([loadPredictions(), loadStatus()]);
    } catch {
      setMessage({ text: "確定できませんでした", error: true });
    }
  };

  const askAi = async () => {
    const ids = items.filter((i) => selected.has(i.track.id)).map((i) => i.track.id).slice(0, 100);
    if (!ids.length) return;
    setAsking(true);
    setMessage(null);
    try {
      const result = await aiService.suggestGenres(ids);
      const nextAi: Record<number, AiGenreSuggestion> = {};
      const nextChoices: Record<number, Choice> = {};
      let failed = 0;
      for (const suggestion of result.items) {
        nextAi[suggestion.track_id] = suggestion;
        if (suggestion.genre && !suggestion.error) {
          nextChoices[suggestion.track_id] = { genre: suggestion.genre, subgenre: suggestion.subgenre ?? null, source: "ai" };
        } else {
          failed += 1;
        }
      }
      setAi((prev) => ({ ...prev, ...nextAi }));
      setChoices((prev) => ({ ...prev, ...nextChoices }));
      const firstError = result.items.find((i) => i.error)?.error;
      setMessage(
        failed
          ? { text: `AI の回答を ${ids.length - failed} 曲分受け取りました（${failed} 曲は失敗: ${firstError ?? "不明"}）`, error: failed === ids.length }
          : { text: `AI の案を ${ids.length} 曲分表示しました。確認して「確定」してください`, error: false }
      );
    } catch (error) {
      setMessage({ text: error instanceof Error ? error.message : "AI に問い合わせできませんでした", error: true });
    } finally {
      setAsking(false);
    }
  };

  const metrics = status?.metrics;
  const training = status?.state === "training";
  const canTrain = !!status && !training && (status.usable_labels ?? 0) >= status.min_labels;
  const counts = page?.counts;
  const filters: { id: Filter; text: string }[] = useMemo(
    () => [
      { id: "all", text: `すべて${counts ? ` ${counts.high + counts.medium + counts.low}` : ""}` },
      { id: "high", text: `自信 高${counts ? ` ${counts.high}` : ""}` },
      { id: "medium", text: `中${counts ? ` ${counts.medium}` : ""}` },
      { id: "low", text: `低${counts ? ` ${counts.low}` : ""}` },
    ],
    [counts]
  );

  return (
    <div className="h-full flex flex-col">
      {/* Classifier status */}
      <div className="p-4 border-b space-y-3 shrink-0">
        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <h3 className="font-medium flex items-center gap-2">
              <Brain className="h-4 w-4" />
              自動分類
            </h3>
            <p className="text-sm text-muted-foreground">
              あなたが確定したジャンルから、この端末の中で学習します。タグのジャンルは使いません。
            </p>
          </div>
          <Button size="sm" onClick={train} disabled={!canTrain}>
            {training ? <Loader2 className="h-4 w-4 animate-spin mr-1" /> : <RefreshCw className="h-4 w-4 mr-1" />}
            学習
          </Button>
        </div>

        {training && (
          <div className="space-y-1">
            <Progress value={Math.round((status?.progress ?? 0) * 100)} />
            <p className="text-xs text-muted-foreground">
              学習中… {status?.training_labels ? `${status.training_labels.toLocaleString()} 曲のラベルから` : ""}
            </p>
          </div>
        )}

        {!training && status?.state === "untrained" && (
          <p className="text-sm">
            {canTrain
              ? `確定済みの曲が ${status.usable_labels?.toLocaleString()} 曲あります。「学習」で分類を始めましょう。`
              : `学習には、解析済みで確定したジャンルの曲が ${status?.min_labels ?? 20} 曲以上必要です（いま ${status?.usable_labels ?? 0} 曲）。`}
          </p>
        )}

        {!training && metrics && (
          <div className="text-sm flex flex-wrap gap-x-6 gap-y-1">
            <span>
              正解率 <strong>{percent(metrics.genre_accuracy)}</strong>
              <span className="text-muted-foreground">（上位3候補なら {percent(metrics.genre_top3_accuracy)}）</span>
            </span>
            {mode !== "genre" && (
              <span>
                サブジャンル <strong>{percent(metrics.subgenre_accuracy)}</strong>
              </span>
            )}
            <span>
              自信 高 の曲は <strong>{percent(metrics.high_confidence_accuracy)}</strong> 正解
            </span>
            {!!status?.new_labels && (
              <span className="text-muted-foreground">
                前回の学習から新しいラベル {status.new_labels} 曲（{status.auto_retrain_after} 曲で自動的に学習し直します）
              </span>
            )}
            <button
              type="button"
              className="text-xs text-muted-foreground underline underline-offset-2"
              onClick={() => setShowDetails((v) => !v)}
            >
              {showDetails ? "詳細を閉じる" : "詳細"}
            </button>
          </div>
        )}
        {status?.error && !training && <p className="text-xs text-destructive">前回の学習は失敗しました: {status.error}</p>}

        {showDetails && metrics && (
          <div className="text-xs text-muted-foreground space-y-2 border rounded-md p-3 bg-muted/20">
            <p>
              正解率は、確定済みの曲から各ジャンルの 20% を学習に使わずに取り分け、その曲で測った値です（
              {metrics.holdout_samples.toLocaleString()} 曲で評価、全 {metrics.samples.toLocaleString()} 曲で学習）。
              「自信 高」は確信度 {percent(status?.high_confidence)} 以上で、評価用の曲の {percent(metrics.high_confidence_coverage)} が該当しました。
              確信度のずれ（ECE）は {percent(metrics.calibration_error)}。
              {status?.trained_at && ` 学習日時: ${new Date(status.trained_at).toLocaleString()}`}
            </p>
            <div className="grid grid-cols-2 sm:grid-cols-4 gap-x-4 gap-y-0.5">
              {metrics.per_genre.map((g) => (
                <span key={g.genre} className="tabular-nums">
                  {g.genre}: {percent(g.recall)} <span className="opacity-60">({g.support})</span>
                </span>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Toolbar */}
      <div className="px-4 py-2 border-b flex flex-wrap items-center gap-2 shrink-0">
        {filters.map((f) => (
          <Button key={f.id} size="sm" variant={filter === f.id ? "secondary" : "ghost"} onClick={() => setFilter(f.id)}>
            {f.text}
          </Button>
        ))}
        <div className="flex-1" />
        <Button size="sm" variant="ghost" onClick={selectHigh} disabled={!items.length}>
          自信 高 を選択
        </Button>
        {aiReady && (
          <Button size="sm" variant="outline" onClick={askAi} disabled={asking || selected.size === 0}>
            {asking ? <Loader2 className="h-4 w-4 animate-spin mr-1" /> : <Bot className="h-4 w-4 mr-1" />}
            AI に聞く（{selected.size}）
          </Button>
        )}
        <Button size="sm" onClick={accept} disabled={selected.size === 0}>
          確定（{selected.size}）
        </Button>
      </div>
      {message && (
        <p className={`px-4 py-2 text-xs border-b ${message.error ? "text-destructive" : "text-green-700 dark:text-green-400"}`}>
          {message.text}
        </p>
      )}

      {/* Predictions */}
      <div className="flex-1 min-h-0">
        <ScrollArea className="h-full">
          <div className="p-2">
            {items.length > 0 && (
              <div className="flex items-center gap-3 px-2 py-1 text-xs text-muted-foreground">
                <Checkbox checked={allSelected} onCheckedChange={toggleAll} />
                <span>表示中の {items.length} 曲（全 {page?.total ?? 0} 曲）</span>
                {!!page?.without_features && <span>・解析待ちで候補なし {page.without_features} 曲</span>}
              </div>
            )}
            {items.map((item) => {
              const choice = choiceFor(item);
              const opts = options(item);
              const aiAnswer = ai[item.track.id];
              const selectValue = opts.findIndex((o) => o.genre === choice.genre && o.subgenre === choice.subgenre);
              return (
                <div key={item.track.id} className="flex items-center gap-3 p-2 rounded-md hover:bg-muted/50 group/item">
                  <Checkbox checked={selected.has(item.track.id)} onCheckedChange={() => toggle(item.track.id)} />
                  <PlayButton
                    track={item.track}
                    variant="ghost"
                    size="icon"
                    className="h-6 w-6 opacity-0 group-hover/item:opacity-100 transition-opacity"
                    iconClassName="h-3 w-3"
                  />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium">{item.track.title}</div>
                    <div className="truncate text-xs text-muted-foreground">
                      {item.track.artist}
                      {item.track.bpm ? ` ・ ${item.track.bpm.toFixed(0)} BPM` : ""}
                    </div>
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    {choice.source === "ai" && (
                      <span title={aiAnswer?.reason ?? ""} className="flex items-center gap-1 text-xs text-primary">
                        <Sparkles className="h-3 w-3" />
                        AI 案
                      </span>
                    )}
                    {aiAnswer?.error && <span className="text-xs text-destructive" title={aiAnswer.error}>AI 失敗</span>}
                    <select
                      className="h-7 max-w-[260px] rounded-md border bg-background px-2 text-xs"
                      value={selectValue >= 0 ? String(selectValue) : "custom"}
                      onChange={(e) => {
                        const picked = opts[Number(e.target.value)];
                        if (picked) setChoices((prev) => ({ ...prev, [item.track.id]: picked }));
                      }}
                    >
                      {selectValue < 0 && <option value="custom">{label(choice, mode)}</option>}
                      {opts.map((o, index) => (
                        <option key={index} value={index}>
                          {label(o, mode)}
                        </option>
                      ))}
                    </select>
                    <span
                      className={`text-xs px-1.5 py-0.5 rounded tabular-nums w-[64px] text-center ${LEVEL_CLASS[item.prediction.level]}`}
                      title="分類器の確信度"
                    >
                      {LEVEL_LABEL[item.prediction.level]} {percent(item.prediction.confidence)}
                    </span>
                  </div>
                </div>
              );
            })}

            {loading && (
              <div className="flex justify-center p-4">
                <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
              </div>
            )}
            {!loading && page && page.total > items.length && (
              <div className="flex justify-center p-2">
                <Button variant="ghost" size="sm" onClick={() => loadPredictions(items.length + PAGE)}>
                  さらに表示
                </Button>
              </div>
            )}
            {!loading && page && items.length === 0 && (
              <div className="text-center p-8 text-sm text-muted-foreground">
                {page.trained ? "未判定の曲はありません。" : "まだ学習していません。上の「学習」から始めてください。"}
              </div>
            )}
          </div>
        </ScrollArea>
      </div>
    </div>
  );
}
