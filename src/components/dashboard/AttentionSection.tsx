import { useCallback, useEffect, useState } from "react";
import { AlertCircle, CheckCircle2, ChevronDown, Clock, Loader2, RefreshCw, Sparkles, Waves, Wrench } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { toast } from "@/components/ui/toast";
import { libraryImportService, type AttentionKind, type LibraryAttention } from "@/services/library-import";

type Row = {
  kind: AttentionKind;
  icon: typeof AlertCircle;
  title: string;
  hint: string;
  action: string;
};

const ROWS: Row[] = [
  { kind: "unanalyzed", icon: Clock, title: "まだ解析していない曲", hint: "BPM や波形がまだ無いため、Play や推薦で使えません。", action: "解析する" },
  { kind: "failed", icon: AlertCircle, title: "取り込みに失敗した曲", hint: "読み込みや解析が途中で止まりました。もう一度試せます。", action: "再試行" },
  { kind: "timbre_default", icon: Waves, title: "音色が測れていない曲", hint: "エネルギーや明るさが初期値のままです。音色だけを測り直します。", action: "測り直す" },
  { kind: "outdated", icon: RefreshCw, title: "古い解析の曲", hint: "以前のバージョンで解析されています。新しい解析で更新します。", action: "更新する" },
  { kind: "embedding_pending", icon: Sparkles, title: "似た曲探しの準備中", hint: "似た曲の検索に使うデータを計算します。再生や推薦はこのままでも使えます。", action: "今すぐ計算" },
];

const JOB_LABELS: Record<string, string> = {
  timbre: "音色を測り直しています",
  embedding: "似た曲探しの準備をしています",
};

function jobLabel(features: string[]): string {
  if (features.length === 1 && JOB_LABELS[features[0]]) return JOB_LABELS[features[0]];
  return "解析を更新しています";
}

/** Dashboard 「要対応」: counts with one-click fixes (README §2.5). */
export function AttentionSection({ refreshKey }: { refreshKey: number }) {
  const [data, setData] = useState<LibraryAttention | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<AttentionKind | null>(null);
  const [showDetails, setShowDetails] = useState(false);

  const load = useCallback(async () => {
    try { setData(await libraryImportService.attention()); setError(null); }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
  }, []);

  useEffect(() => { void load(); }, [load, refreshKey]);
  useEffect(() => {
    const timer = setInterval(() => void load(), data?.job ? 3000 : 15000);
    const onChange = () => void load();
    window.addEventListener("plumdeck:imports-changed", onChange);
    window.addEventListener("plumdeck:import-tracks-updated", onChange);
    return () => {
      clearInterval(timer);
      window.removeEventListener("plumdeck:imports-changed", onChange);
      window.removeEventListener("plumdeck:import-tracks-updated", onChange);
    };
  }, [load, data?.job]);

  const fix = async (row: Row) => {
    setBusy(row.kind);
    try {
      const { started } = await libraryImportService.fixAttention(row.kind);
      if (started) toast.success(`${started.toLocaleString()}曲を${row.action === "再試行" ? "もう一度取り込んでいます" : "処理しています"}`, "進み具合は画面右下に表示されます。");
      else toast.info("対象の曲はありませんでした");
      await load();
    } catch (cause) {
      toast.error("開始できませんでした", cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const pending = data ? ROWS.filter((row) => data[row.kind] > 0) : [];
  const job = data?.job ?? null;

  return (
    <section className="rounded-xl border bg-card p-5 shadow-sm">
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <Wrench className="h-5 w-5 text-amber-500" />
          <h2 className="text-lg font-semibold">要対応</h2>
          {pending.length > 0 && <span className="rounded-full bg-amber-500/15 px-2 py-0.5 text-xs font-medium text-amber-600 dark:text-amber-400">{pending.length}</span>}
        </div>
      </div>

      {job && (
        <div className="mt-4 space-y-2 rounded-lg bg-muted/50 p-3" role="status">
          <div className="flex items-center justify-between text-sm">
            <span className="flex items-center gap-2"><Loader2 className="h-4 w-4 animate-spin text-primary" />{job.status === "paused" ? "一時停止中" : jobLabel(job.features)}</span>
            <span className="tabular-nums text-muted-foreground">{job.done.toLocaleString()} / {job.total.toLocaleString()}曲</span>
          </div>
          <Progress value={job.total ? (100 * job.done) / job.total : 0} />
        </div>
      )}

      {!data && !error && <div className="mt-4 flex items-center gap-2 text-sm text-muted-foreground"><Loader2 className="h-4 w-4 animate-spin" />ライブラリを確認しています…</div>}

      {error && (
        <div className="mt-4 flex items-center justify-between gap-3 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm">
          <span>ライブラリの状態を確認できませんでした。</span>
          <Button size="sm" variant="outline" onClick={() => void load()}>再試行</Button>
        </div>
      )}

      {data && pending.length === 0 && !job && (
        <div className="mt-4 flex items-center gap-2 text-sm text-muted-foreground">
          <CheckCircle2 className="h-4 w-4 text-emerald-500" />
          すべての曲が使える状態です。
        </div>
      )}

      {pending.length > 0 && (
        <ul className="mt-4 divide-y">
          {pending.map((row) => {
            const Icon = row.icon;
            return (
              <li key={row.kind} className="flex items-center gap-3 py-3 first:pt-0 last:pb-0">
                <div className="rounded-full bg-muted p-2 text-muted-foreground"><Icon className="h-4 w-4" /></div>
                <div className="min-w-0 flex-1">
                  <p className="text-sm font-medium">{row.title} <span className="tabular-nums">{data![row.kind].toLocaleString()}曲</span></p>
                  <p className="text-xs text-muted-foreground">{row.hint}</p>
                </div>
                <Button size="sm" variant="outline" disabled={busy !== null} onClick={() => void fix(row)}>
                  {busy === row.kind && <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />}
                  {row.action}
                </Button>
              </li>
            );
          })}
        </ul>
      )}

      {(error || (data && pending.length > 0)) && (
        <div className="mt-3">
          <button type="button" className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground" onClick={() => setShowDetails((v) => !v)}>
            詳細 <ChevronDown className={`h-3 w-3 transition-transform ${showDetails ? "rotate-180" : ""}`} />
          </button>
          {showDetails && (
            <div className="mt-2 space-y-1 rounded-md bg-muted/40 p-2 font-mono text-[11px] text-muted-foreground">
              {error && <p className="break-all">{error}</p>}
              {data && ROWS.map((row) => <p key={row.kind}>{row.kind}: {data[row.kind]}</p>)}
              {job && <p>job {job.id} · {job.status} · {job.features.join(",")} · failed {job.failed}</p>}
              <p>未解析・失敗は取り込みキュー、それ以外は解析ジョブで処理します。</p>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
