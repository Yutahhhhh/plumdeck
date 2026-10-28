import { Circle, Square } from "lucide-react";
import { cn } from "@/lib/utils";
import type { RecordingState } from "@/types/dj-engine";

export const formatElapsed = (ms: number) => {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(seconds / 3600), minutes = Math.floor(seconds / 60) % 60;
  return `${hours ? `${hours}:` : ""}${String(minutes).padStart(hours ? 2 : 1, "0")}:${String(seconds % 60).padStart(2, "0")}`;
};

type Props = {
  recording?: RecordingState;
  directory: string;
  disabledReason?: string;
  onToggle: () => void;
};

/** Minimal recording panel: source, elapsed time, format, folder and REC. */
export function RecordingPanel({ recording, directory, disabledReason, onToggle }: Props) {
  const active = Boolean(recording?.active);
  const format = recording?.formats?.find((entry) => entry.name === recording.format);
  return <section className="dj-recording-panel" aria-label="録音">
    <span className="dj-recording-panel-title">RECORDING</span>
    <button type="button" className={cn("dj-recording-panel-rec", active && "is-recording")} disabled={Boolean(disabledReason) || recording?.stopping}
      aria-pressed={active} title={disabledReason ?? (active ? "録音を停止して保存" : "MASTER 出力の録音を開始")} onClick={onToggle}>
      {active ? <Square /> : <Circle />}{recording?.stopping ? "保存中…" : active ? "STOP" : "REC"}
    </button>
    <span className="dj-recording-panel-time dj-num" aria-label="録音時間">{formatElapsed(recording?.elapsedMs ?? 0)}</span>
    <dl>
      <div><dt>録音元</dt><dd>MASTER 出力（マイクを含む）</dd></div>
      <div><dt>形式</dt><dd>{format ? `${format.label}${format.lossless ? "（ロスレス）" : ""}` : recording?.format ?? "既定"}</dd></div>
      <div><dt>保存先</dt><dd title={directory || "既定の録音フォルダ"}>{directory || "既定の録音フォルダ"}</dd></div>
    </dl>
    {(disabledReason || recording?.error) && <small role={recording?.error ? "alert" : undefined}>{recording?.error ?? disabledReason}</small>}
  </section>;
}
