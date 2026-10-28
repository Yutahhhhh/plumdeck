import { useRef, type CSSProperties } from "react";
import { cn } from "@/lib/utils";
import type { ChannelState, DeckId, EqBand, MetersPayload } from "@/types/dj-engine";
import { RotaryKnob } from "./RotaryKnob";
import { Fader } from "./SoftwareDeck";
import { COLOR_FX, CROSSFADER_ASSIGN, faderGain, faderPosition, formatColorAmount, formatDb, isKilled, type ColorFxEffect, type FaderCurve } from "./mixer-math";

export type MixerChannelHandlers = {
  onTrim?: (gain: number) => void;
  onEq?: (band: EqBand, gain: number) => void;
  /** COLOR knob (−1..1, centre bypass) for the selected Color FX. */
  onColor?: (amount: number) => void;
  onGain: (gain: number) => void;
  onPfl?: (enabled: boolean) => void;
  onOrientation?: (orientation: number) => void;
};

type Props = {
  decks: readonly DeckId[];
  channelState: (deck: DeckId) => ChannelState | undefined;
  handlers: (deck: DeckId) => MixerChannelHandlers;
  /** 使えない理由。undefined なら操作可。 */
  reason: (deck: DeckId, capability: string, feature: string, controller: boolean) => string | undefined;
  colorFx: ColorFxEffect;
  /** The host can switch Color FX types; otherwise COLOR is FILTER only. */
  colorFxSelectable: boolean;
  onColorFx: (effect: ColorFxEffect) => void;
  faderCurve: FaderCurve;
  /** Channel levels when the host publishes meters; null hides the meters. */
  meters?: MetersPayload | null;
};

const EQ_BANDS: { band: EqBand; label: string; key: "eqHigh" | "eqMid" | "eqLow" }[] = [
  { band: "high", label: "HI", key: "eqHigh" },
  { band: "mid", label: "MID", key: "eqMid" },
  { band: "low", label: "LOW", key: "eqLow" },
];

/**
 * Mixer panel between the decks: TRIM / HI / MID / LOW / COLOR, crossfader
 * assign, CUE and the channel fader. The tempo fader lives in the deck.
 * Toggled from the toolbar (closing it removes the knobs).
 */
export function MixerPanel({ decks, channelState, handlers, reason, colorFx, colorFxSelectable, onColorFx, faderCurve, meters }: Props) {
  // Value to return to when a killed band is released.
  const beforeKill = useRef(new Map<string, number>());
  const colorReason = reason(decks[0], colorFxSelectable ? "mixer.colorfx" : "mixer.filter", "COLOR FX", true);
  const effect = colorFxSelectable ? colorFx : "filter";
  return <div className="dj-mixer-panel" role="group" aria-label="ミキサーパネル">
    {colorFxSelectable && <label className="dj-mixer-colorfx" title={colorReason ?? "COLOR つまみにかける Color FX（全チャンネル共通）"}>
      <span>COLOR FX</span>
      <select aria-label="Color FX の種類" value={colorFx} disabled={Boolean(colorReason)} onChange={(event) => onColorFx(event.target.value as ColorFxEffect)}>
        {COLOR_FX.map((entry) => <option key={entry.effect} value={entry.effect}>{entry.label}</option>)}
      </select>
    </label>}
    <div className="dj-mixer-channels">
    {decks.map((deck) => {
      const channel = channelState(deck);
      const on = handlers(deck);
      const eqReason = reason(deck, "mixer.eq", "EQ", Boolean(on.onEq));
      const trimReason = reason(deck, "mixer.trim", "TRIM", Boolean(on.onTrim));
      const filterReason = reason(deck, colorFxSelectable ? "mixer.colorfx" : "mixer.filter", "COLOR", Boolean(on.onColor));
      const pflReason = reason(deck, "mixer.pfl", "CUE", Boolean(on.onPfl));
      const gainReason = reason(deck, "mixer.gain", "LEVEL", true);
      const assignReason = reason(deck, "mixer.crossfader", "クロスフェーダーのアサイン", Boolean(on.onOrientation));
      // `filter` telemetry drifts for non-filter types; the last set amount is exact.
      const color = channel?.colorFx && (channel.colorFx.effect === effect || !colorFxSelectable) ? channel.colorFx.amount : effect === "filter" ? channel?.filter ?? 0 : 0;
      const orientation = channel?.orientation ?? 1;
      const kill = (band: EqBand, key: "eqHigh" | "eqMid" | "eqLow") => {
        const gain = channel?.[key] ?? 1, memo = `${deck}:${band}`;
        if (isKilled(gain)) { on.onEq?.(band, beforeKill.current.get(memo) ?? 1); beforeKill.current.delete(memo); }
        else { beforeKill.current.set(memo, gain); on.onEq?.(band, 0); }
      };

      return <div className="dj-mixer-channel" key={deck} data-deck={deck} aria-label={`Deck ${deck} チャンネル`}>
        <div className="dj-mixer-knobs">
          <RotaryKnob label="TRIM" min={0} max={2} step={.02} fineStep={.005} defaultValue={1} center={1} value={channel?.trim ?? 1}
            disabled={Boolean(trimReason)} valueText={formatDb} onChange={on.onTrim} />
          {EQ_BANDS.map(({ band, label, key }) => <RotaryKnob key={band} label={label} min={0} max={4} step={.04} fineStep={.01} defaultValue={1} center={1} value={channel?.[key] ?? 1}
            disabled={Boolean(eqReason)} valueText={formatDb} onChange={(value) => on.onEq?.(band, value)}
            onLabelClick={eqReason ? undefined : () => kill(band, key)} labelActive={isKilled(channel?.[key])}
            labelTitle={isKilled(channel?.[key]) ? `${label} のキルを解除` : `${label} をキル（もう一度押すと戻ります）`} />)}
          <RotaryKnob className="dj-mixer-color" label="COLOR" min={-1} max={1} step={.02} fineStep={.005} defaultValue={0} value={color}
            disabled={Boolean(filterReason)} valueText={(value) => `${COLOR_FX.find((entry) => entry.effect === effect)?.label ?? "FILTER"} ${formatColorAmount(effect, value)}`} onChange={on.onColor} />
        </div>

        <div className="dj-mixer-assign" role="group" aria-label={`Deck ${deck} クロスフェーダーのアサイン`}>
          {CROSSFADER_ASSIGN.map((entry) => <button key={entry.orientation} type="button" className={cn(orientation === entry.orientation && "is-on")}
            disabled={Boolean(assignReason)} aria-pressed={orientation === entry.orientation} title={assignReason ?? entry.title}
            onClick={() => on.onOrientation?.(entry.orientation)}>{entry.label}</button>)}
        </div>

        <button type="button" className={cn("dj-mixer-cue", channel?.pfl && "is-on")} disabled={Boolean(pflReason)} aria-pressed={Boolean(channel?.pfl)}
          title={pflReason ?? "ヘッドホンでこのチャンネルをモニターする"} onClick={() => on.onPfl?.(!channel?.pfl)}>CUE</button>

        <div className="dj-mixer-sliders">
          <label className="dj-mixer-slider" title={gainReason ?? `LEVEL ${formatDb(channel?.gain ?? 0)}`}>
            <Fader label={`Deck ${deck} チャンネルフェーダー`} min={0} max={1} step={.005} value={faderPosition(channel?.gain ?? 0, faderCurve)} vertical
              disabled={Boolean(gainReason)} onChange={(position) => on.onGain(faderGain(position, faderCurve))} />
            <span className="dj-control-label">{deck}</span>
          </label>
          {meters && <div className="dj-mixer-meter" role="meter" aria-label={`Deck ${deck} レベル`} aria-valuemin={0} aria-valuemax={1}
            aria-valuenow={meters.channels[deck]?.peak ?? 0} style={{ "--level": Math.max(0, Math.min(1, meters.channels[deck]?.peak ?? 0)) } as CSSProperties}><i /></div>}
        </div>
      </div>;
    })}
    </div>
  </div>;
}
