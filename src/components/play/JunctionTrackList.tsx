import { Disc3, Loader2, Radio } from "lucide-react";
import type { JunctionBrowserTrack } from "@/services/junction/tracks";
import { junctionTrackStatus } from "@/services/junction/tracks";
import { formatTime } from "./SoftwareDeck";

/**
 * Status for the single Junction Live source. Current/next are transport and
 * prefetch details, not selectable playlist rows; the draggable source itself
 * lives beside Collection in the browser tree.
 */
export function JunctionTrackList({ tracks }: { tracks: JunctionBrowserTrack[] }) {
  const current = tracks.find(track => track.role === "current");
  const next = tracks.find(track => track.role === "next");
  return <div className="dj-junction-live-panel" data-junction-live-panel>
    <header><Radio /><div><strong>Junction Live</strong><span>Junction Program出力の表示専用モニター</span></div></header>
    <p>左の「Junction Live」を任意のデッキへドラッグしてください。デッキには波形だけを表示し、実デッキやJunction Program出力は変更しません。</p>
    <TrackCard track={current} next={next} />
  </div>;
}

/** One source, one row: what sounds now, with the prefetched next track as a detail. */
function TrackCard({ track, next }: { track?: JunctionBrowserTrack; next?: JunctionBrowserTrack }) {
  return <section className="dj-junction-live-card" data-role={track?.role ?? "empty"}>
    <div className="dj-junction-live-card-icon">{track?.state === "receiving" || track?.state === "verifying" ? <Loader2 className="animate-spin" /> : <Disc3 />}</div>
    <div><small>再生中</small><strong>{track?.title || "セットされていません"}</strong><span>{track ? [track.artist, track.sourceDjName, `Deck ${track.sourceDeck}`].filter(Boolean).join(" · ") : "前のDJがセットすると自動で先読みします"}</span>
      {next && <span>次：{next.title || "タイトル未設定"}（{junctionTrackStatus(next)}）</span>}</div>
    {track && <div className="dj-junction-live-card-state"><b>{junctionTrackStatus(track)}</b><span>{formatTime(track.positionMs)} / {formatTime(track.duration * 1000)}</span></div>}
  </section>;
}
