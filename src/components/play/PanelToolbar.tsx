import type { ReactElement } from "react";
import { cn } from "@/lib/utils";

/** rekordbox ツールバーの表示/非表示トグル。並びとツールチップは実機に合わせている。 */
export type PanelId = "fx" | "sampler" | "mixer" | "recording";
export type PanelVisibility = Record<PanelId, boolean>;

export const PANEL_DEFAULTS: PanelVisibility = {
  fx: false, sampler: false, mixer: true, recording: false,
};

const PANELS: { id: PanelId; label: string; title: string; icon: ReactElement }[] = [
  {
    id: "fx", label: "FX", title: "BEAT FX パネルを表示/隠す",
    icon: <span className="dj-panel-glyph dj-panel-glyph--text">FX</span>,
  },
  {
    id: "sampler", label: "SAMPLER", title: "サンプラーパネルを表示/隠す",
    icon: <svg viewBox="0 0 12 12" aria-hidden><g fill="currentColor">
      {[0, 1, 2].flatMap((row) => [0, 1, 2].map((column) => <rect key={`${row}-${column}`} x={column * 4.5} y={row * 4.5} width="3" height="3" />))}
    </g></svg>,
  },
  {
    id: "mixer", label: "MIXER", title: "ミキサーパネルを表示/隠す",
    icon: <svg viewBox="0 0 12 12" aria-hidden><g stroke="currentColor" strokeWidth="1.2" fill="none">
      <path d="M2 0v12M6 0v12M10 0v12" />
      <path d="M0.5 3.5h3M4.5 7.5h3M8.5 2.5h3" strokeWidth="2.2" />
    </g></svg>,
  },
  {
    id: "recording", label: "RECORDING", title: "録音パネルを表示/隠す",
    icon: <svg viewBox="0 0 12 12" aria-hidden><circle cx="6" cy="6" r="5" fill="none" stroke="currentColor" strokeWidth="1.4" /><circle cx="6" cy="6" r="2" fill="currentColor" /></svg>,
  },
];

type Props = {
  visible: PanelVisibility;
  /** Panels this engine can fill. A toggle is shown only when its panel has content. */
  available: Record<PanelId, boolean>;
  onToggle: (panel: PanelId) => void;
};

export function PanelToolbar({ visible, available, onToggle }: Props) {
  return <div className="dj-panel-toolbar" role="group" aria-label="パネル表示切り換え">
    {PANELS.filter(panel => available[panel.id]).map((panel) => <button key={panel.id} type="button"
      className={cn("dj-panel-toggle", visible[panel.id] && "is-on")}
      aria-pressed={visible[panel.id]} aria-label={panel.label} title={panel.title}
      onClick={() => onToggle(panel.id)}>{panel.icon}</button>)}
  </div>;
}
