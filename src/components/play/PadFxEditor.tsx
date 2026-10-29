import { useSyncExternalStore } from "react";
import { RotateCcw } from "lucide-react";
import { defaultPadPreset, padOverride, padOverrideOf, padOverridesVersion, padPreset, PAD_FX_BEATS, PAD_FX_EXTENDED, PAD_FX_TYPES, setPadOverride, subscribePadOverrides, type PadFxType, type PadOverride } from "@/services/midi/pad-presets";
import { formatBeats } from "./loop-math";

/**
 * PAD FX inline edit view (rekordbox's pad editor): per pad, the FX type,
 * beat length and parameter. Banks 1 / 2 are shared by every deck and input
 * and saved; the pads read the edit at once.
 */
export function PadFxEditor({ bank, page, capabilities, deckId }: { bank: 1 | 5; page: number; capabilities: readonly string[]; deckId: string }) {
  useSyncExternalStore(subscribePadOverrides, padOverridesVersion);
  const extendedHost = capabilities.includes(PAD_FX_EXTENDED);
  return <div className="dj-padfx-editor" role="table" aria-label={`Deck ${deckId} PAD FX ${bank === 5 ? 2 : 1} の編集`}>
    {Array.from({ length: 8 }, (_, index) => {
      const slot = index + page * 8;
      const edited = padOverride(bank, slot);
      const current: PadOverride = edited ?? padOverrideOf(padPreset(bank, slot, capabilities));
      const isColor = current.type.startsWith("color:");
      const save = (next: Partial<PadOverride>) => setPadOverride(bank, slot, { ...current, ...next });
      const fallback = defaultPadPreset(bank, slot, capabilities);
      return <div key={slot} role="row" className={edited ? "is-edited" : undefined}>
        <span role="cell" className="dj-padfx-index dj-num">{slot + 1}</span>
        <select role="cell" aria-label={`パッド ${slot + 1} の FX`} value={current.type} onChange={(event) => {
          const type = event.target.value as PadFxType;
          save({ type, beats: type.startsWith("color:") ? null : current.beats ?? 0.5 });
        }}>
          {PAD_FX_TYPES.map((entry) => <option key={entry.type} value={entry.type} disabled={entry.extended && !extendedHost}>
            {entry.label}{entry.extended && !extendedHost ? "（この音声エンジン未対応）" : ""}</option>)}
        </select>
        <select role="cell" aria-label={`パッド ${slot + 1} の拍`} value={current.beats ?? ""} disabled={isColor}
          title={isColor ? "COLOR FX は拍を持ちません" : extendedHost ? "拍の長さ" : "拍の長さ（この音声エンジンでは効きません）"}
          onChange={(event) => save({ beats: event.target.value === "" ? null : Number(event.target.value) })}>
          <option value="">—</option>
          {PAD_FX_BEATS.map((beats) => <option key={beats} value={beats}>{formatBeats(beats)}</option>)}
        </select>
        <label role="cell" className="dj-padfx-param" title={isColor && current.type === "color:filter" ? "50 未満でローパス、50 超でハイパス" : "かかり具合（0〜100）"}>
          <input type="range" min={0} max={100} step={1} value={current.param} aria-label={`パッド ${slot + 1} のパラメータ`}
            onChange={(event) => save({ param: Number(event.target.value) })} />
          <b className="dj-num">{current.param}</b>
        </label>
        <button role="cell" type="button" className="dj-padfx-reset" disabled={!edited}
          title={edited ? `既定（${fallback.label}${fallback.beats ? ` ${formatBeats(fallback.beats)}` : ""}）に戻す` : "既定のまま"}
          aria-label={`パッド ${slot + 1} を既定に戻す`} onClick={() => setPadOverride(bank, slot, null)}><RotateCcw /></button>
      </div>;
    })}
  </div>;
}
