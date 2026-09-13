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
    <div className="dj-junction-live-pair">
      <TrackCard label="現在再生中" track={current} />
      <TrackCard label="次の曲 · 先読み" track={next} />
    </div>
  </div>;
}

function TrackCard({ label, track }: { label: string; track?: JunctionBrowserTrack }) {
  return <section className="dj-junction-live-card" data-role={track?.role ?? "empty"}>
    <div className="dj-junction-live-card-icon">{track?.state === "receiving" || track?.state === "verifying" ? <Loader2 className="animate-spin" /> : <Disc3 />}</div>
    <div><small>{label}</small><strong>{track?.title || "セットされていません"}</strong><span>{track ? [track.artist, track.sourceDjName, `Deck ${track.sourceDeck}`].filter(Boolean).join(" · ") : "前のDJがセットすると自動で先読みします"}</span></div>
    {track && <div className="dj-junction-live-card-state"><b>{junctionTrackStatus(track)}</b><span>{formatTime(track.positionMs)} / {formatTime(track.duration * 1000)}</span></div>}
  </section>;
}
