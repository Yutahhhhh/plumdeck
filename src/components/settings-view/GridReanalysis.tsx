import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { gridJobs, type GridJob, type GridJobPlan } from "@/services/grid-jobs";
import { getErrorDetail } from "@/services/api-client";

const time = (seconds: number) => `${Math.floor(seconds / 60)}分${Math.floor(seconds % 60)}秒`;

/** A small entry to the existing resumable selective-analysis queue. */
export function GridReanalysis() {
  const [outdated, setOutdated] = useState(true);
  const [plan, setPlan] = useState<GridJobPlan | null>(null);
  const [job, setJob] = useState<GridJob>({ status: "idle" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    let alive = true, timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try { const next = await gridJobs.status(); if (alive) setJob(next); }
      catch (e) { if (alive) setError(getErrorDetail(e)); }
      if (alive) timer = setTimeout(refresh, 2000);
    };
    void refresh();
    return () => { alive = false; clearTimeout(timer); };
  }, []);
  const active = job.status === "running" || job.status === "pausing";
  const ours = job.config?.features.length === 1 && job.config.features[0] === "rhythm";
  const run = async (operation: () => Promise<void>) => {
    setBusy(true); setError("");
    try { await operation(); } catch (e) { setError(getErrorDetail(e)); }
    finally { setBusy(false); }
  };
  const count = job.counts;
  const done = count ? count.completed + count.skipped + count.failed : 0;
  return <div className="space-y-3 rounded-lg border p-4">
    <div><p className="font-medium">グリッドを一括再解析</p>
      <p className="mt-1 text-xs text-muted-foreground">拍の位置だけを更新し、BPMは変更しません。rekordboxで解析済み（グリッドあり）の曲は対象外です。手動の保存済みグリッド、CUE、ループは保持します。完了後はデッキに曲を読み込み直してください。</p></div>
    <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={outdated} disabled={active || busy} onChange={e => { setOutdated(e.target.checked); setPlan(null); }} />未更新の曲だけ</label>
    <div className="flex flex-wrap items-center gap-2">
      <Button size="sm" variant="outline" disabled={active || busy} onClick={() => void run(async () => setPlan(await gridJobs.plan(outdated)))}>対象を確認</Button>
      {plan && <><span className="text-sm">{plan.selected_tracks.toLocaleString()}曲 / 全{plan.matched_tracks.toLocaleString()}曲{plan.rekordbox_grid_tracks ? `（rekordboxのグリッドがある${plan.rekordbox_grid_tracks.toLocaleString()}曲は対象外）` : ""}</span>
        <Button size="sm" disabled={active || busy || !plan.selected_tracks} onClick={() => void run(async () => { setJob(await gridJobs.start(outdated)); setPlan(null); })}>一括再解析を開始</Button></>}
      {ours && job.id && active && <Button size="sm" variant="outline" disabled={busy || job.status === "pausing"} onClick={() => void run(async () => setJob(await gridJobs.control(job.id!, "pause")))}>一時停止</Button>}
      {ours && job.id && job.status === "paused" && <Button size="sm" disabled={busy} onClick={() => void run(async () => setJob(await gridJobs.control(job.id!, "resume")))}>続きから再開</Button>}
      {ours && job.id && !active && !!count?.failed && <Button size="sm" variant="outline" disabled={busy} onClick={() => void run(async () => setJob(await gridJobs.control(job.id!, "retry")))}>失敗した曲を再試行</Button>}
    </div>
    {ours && count && <div className="space-y-1 text-xs" role="status" aria-live="polite">
      <p>{job.status === "pausing" ? "処理中の曲を終えて一時停止します" : active ? "解析中" : job.status === "paused" ? "一時停止中" : "処理終了"} · {done} / {job.total}曲（成功 {count.completed}・更新不要 {count.skipped}・失敗 {count.failed}）</p>
      <progress className="w-full" aria-label="グリッド再解析の進捗" value={done} max={Math.max(1, job.total ?? 0)} />
      <p>経過 {time(job.elapsed ?? 0)}{active && job.estimated_remaining_seconds != null ? ` · 残り約${time(job.estimated_remaining_seconds)}` : ""}</p>
      {job.current_tracks?.map(track => <p key={track.track_id} className="break-all">{track.filepath.split(/[\\/]/).pop()}：音源から拍の位置を検出中</p>)}
      {!!job.errors?.length && <details><summary>失敗の詳細</summary>{job.errors.map(item => <p key={item.track_id} className="break-all">{item.filepath.split(/[\\/]/).pop()}：{item.error}</p>)}</details>}
    </div>}
    {active && !ours && <p className="text-xs">別の解析が実行中です。完了後に開始できます。</p>}
    {(error || job.error) && <p role="alert" className="text-xs text-destructive">{error || job.error}</p>}
  </div>;
}
