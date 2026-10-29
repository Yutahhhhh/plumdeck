import { useCallback, useEffect, useMemo, useState } from "react";
import { ChevronDown, Disc3, Loader2, Search } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { toast } from "@/components/ui/toast";
import { libraryImportService, type RekordboxUnregistered } from "@/services/library-import";

const LIST_LIMIT = 500;

/**
 * 「rekordbox にある未登録の曲」: shown only when rekordbox's library is
 * readable and has tracks plumdeck does not. Imported tracks take BPM, grid
 * and key from rekordbox.
 */
export function RekordboxImportCard({ refreshKey }: { refreshKey: number }) {
  const [data, setData] = useState<RekordboxUnregistered | null>(null);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [query, setQuery] = useState("");
  const [showDetails, setShowDetails] = useState(false);

  const load = useCallback(async () => {
    try { setData(await libraryImportService.rekordboxUnregistered()); }
    catch { setData(null); }
  }, []);
  useEffect(() => { void load(); }, [load, refreshKey]);
  useEffect(() => {
    window.addEventListener("plumdeck:import-tracks-updated", load);
    return () => window.removeEventListener("plumdeck:import-tracks-updated", load);
  }, [load]);

  const filtered = useMemo(() => {
    const tracks = data?.tracks ?? [];
    const needle = query.trim().toLowerCase();
    return needle ? tracks.filter((t) => `${t.title ?? ""} ${t.artist ?? ""}`.toLowerCase().includes(needle)) : tracks;
  }, [data, query]);

  if (!data?.available || data.total === 0) return null;

  const run = async (ids?: string[]) => {
    setBusy(true);
    try {
      const { started } = await libraryImportService.importRekordbox(ids);
      toast.success(`${started.toLocaleString()}曲を rekordbox から追加しています`, "BPM・グリッド・キーは rekordbox の値を使います。");
      setOpen(false); setSelected(new Set());
      await load();
    } catch (cause) {
      toast.error("rekordbox から追加できませんでした", cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };
  const toggle = (id: string) => setSelected((current) => {
    const next = new Set(current);
    if (next.has(id)) next.delete(id); else next.add(id);
    return next;
  });
  const visible = filtered.slice(0, LIST_LIMIT);
  const allVisibleSelected = visible.length > 0 && visible.every((t) => selected.has(t.id));

  return (
    <section className="flex flex-col gap-4 rounded-xl border bg-card p-5 shadow-sm sm:flex-row sm:items-center">
      <div className="rounded-full bg-sky-500/10 p-3 text-sky-500"><Disc3 className="h-6 w-6" /></div>
      <div className="min-w-0 flex-1">
        <h2 className="font-semibold">rekordbox にある未登録の曲 <span className="tabular-nums">{data.total.toLocaleString()}曲</span></h2>
        <p className="text-sm text-muted-foreground">rekordbox で解析済みの BPM・グリッド・キーをそのまま使って追加できます。</p>
      </div>
      <div className="flex gap-2">
        <Button variant="outline" size="sm" onClick={() => setOpen(true)} disabled={busy}>選んで追加</Button>
        <Button size="sm" onClick={() => void run()} disabled={busy}>
          {busy && <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />}すべて追加
        </Button>
      </div>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="flex max-h-[85vh] flex-col sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>rekordbox から曲を選ぶ</DialogTitle>
            <DialogDescription>追加したい曲を選んでください。rekordbox のライブラリは読み取るだけで、変更しません。</DialogDescription>
          </DialogHeader>
          <div className="relative">
            <Search className="absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
            <Input className="pl-9" placeholder="曲名・アーティストで絞り込み" value={query} onChange={(e) => setQuery(e.target.value)} />
          </div>
          <div className="flex items-center justify-between text-xs text-muted-foreground">
            <label className="flex cursor-pointer items-center gap-2">
              <Checkbox checked={allVisibleSelected} onCheckedChange={() => setSelected((current) => {
                const next = new Set(current);
                visible.forEach((t) => (allVisibleSelected ? next.delete(t.id) : next.add(t.id)));
                return next;
              })} />
              表示中をすべて選択
            </label>
            <span>{selected.size.toLocaleString()}曲を選択中{filtered.length > LIST_LIMIT ? `・先頭 ${LIST_LIMIT}曲を表示` : ""}</span>
          </div>
          <ul className="min-h-0 flex-1 divide-y overflow-y-auto rounded-md border">
            {visible.map((track) => (
              <li key={track.id}>
                <label className="flex cursor-pointer items-center gap-3 px-3 py-2 hover:bg-muted/50">
                  <Checkbox checked={selected.has(track.id)} onCheckedChange={() => toggle(track.id)} />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm">{track.title || track.filepath.split(/[/\\]/).pop()}</p>
                    <p className="truncate text-xs text-muted-foreground">{track.artist || "アーティスト不明"}</p>
                  </div>
                  <span className="shrink-0 text-xs tabular-nums text-muted-foreground">{track.bpm ? `${track.bpm.toFixed(1)} BPM` : ""}{track.key ? ` · ${track.key}` : ""}</span>
                </label>
              </li>
            ))}
            {visible.length === 0 && <li className="p-6 text-center text-sm text-muted-foreground">該当する曲はありません</li>}
          </ul>
          <div>
            <button type="button" className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground" onClick={() => setShowDetails((v) => !v)}>
              詳細 <ChevronDown className={`h-3 w-3 transition-transform ${showDetails ? "rotate-180" : ""}`} />
            </button>
            {showDetails && <p className="mt-1 text-[11px] text-muted-foreground">ファイルが見つかる曲だけを表示しています（ストリーミングの曲は除きます）。取り込みは「曲を追加」と同じキューで進み、キーも rekordbox の値を使います。</p>}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>キャンセル</Button>
            <Button disabled={busy || selected.size === 0} onClick={() => void run([...selected])}>
              {busy && <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />}{selected.size.toLocaleString()}曲を追加
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
