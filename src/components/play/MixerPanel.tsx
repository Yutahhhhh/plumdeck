import { cn } from "@/lib/utils";
import type { ChannelState, DeckId, EqBand } from "@/types/dj-engine";
import { RotaryKnob } from "./RotaryKnob";
import { Fader } from "./SoftwareDeck";

export type MixerChannelHandlers = {
  onTrim?: (gain: number) => void;
  onEq?: (band: EqBand, gain: number) => void;
  onFilter?: (value: number) => void;
  onGain: (gain: number) => void;
  onPfl?: (enabled: boolean) => void;
};

type Props = {
  decks: readonly DeckId[];
  channelState: (deck: DeckId) => ChannelState | undefined;
  handlers: (deck: DeckId) => MixerChannelHandlers;
  /** 使えない理由。undefined なら操作可。 */
  reason: (deck: DeckId, capability: string, feature: string, controller: boolean) => string | undefined;
};

/**
 * rekordbox の「ミキサーパネル」。デッキ間に立つ縦のチャンネルストリップで、
 * TRIM / HIGH / MID / LOW / FILTER・CUE・チャンネルフェーダー・テンポを持つ。
 * ツールバーのミキサーアイコンで開閉する（閉じるとつまみ自体が消える）。
 */
export function MixerPanel({ decks, channelState, handlers, reason }: Props) {
  return <div className="dj-mixer-panel" role="group" aria-label="ミキサーパネル">
    {decks.map((deck) => {
      const channel = channelState(deck) as (ChannelState & { trim?: number; filter?: number }) | undefined;
      const on = handlers(deck);
      const eqReason = reason(deck, "mixer.eq", "EQ", Boolean(on.onEq));
      const trimReason = reason(deck, "mixer.trim", "TRIM", Boolean(on.onTrim));
      const filterReason = reason(deck, "mixer.filter", "FILTER", Boolean(on.onFilter));
      const pflReason = reason(deck, "mixer.pfl", "CUE", Boolean(on.onPfl));
      const gainReason = reason(deck, "mixer.gain", "LEVEL", true);

      return <div className="dj-mixer-channel" key={deck} data-deck={deck} aria-label={`Deck ${deck} チャンネル`}>
        <div className="dj-mixer-knobs">
        <RotaryKnob label="TRIM" min={0} max={2} step={.02} fineStep={.005} defaultValue={1} value={channel?.trim ?? 1}
          disabled={Boolean(trimReason)} valueText={(value) => `${value.toFixed(2)}×`} onChange={on.onTrim} />
        <RotaryKnob label="HIGH" min={0} max={4} step={.04} fineStep={.01} defaultValue={1} center={1} value={channel?.eqHigh ?? 1}
          disabled={Boolean(eqReason)} valueText={(value) => `${value.toFixed(2)}×`} onChange={(value) => on.onEq?.("high", value)} />
        <RotaryKnob label="MID" min={0} max={4} step={.04} fineStep={.01} defaultValue={1} center={1} value={channel?.eqMid ?? 1}
          disabled={Boolean(eqReason)} valueText={(value) => `${value.toFixed(2)}×`} onChange={(value) => on.onEq?.("mid", value)} />
        <RotaryKnob label="LOW" min={0} max={4} step={.04} fineStep={.01} defaultValue={1} center={1} value={channel?.eqLow ?? 1}
          disabled={Boolean(eqReason)} valueText={(value) => `${value.toFixed(2)}×`} onChange={(value) => on.onEq?.("low", value)} />
        <RotaryKnob label="FILTER" min={-1} max={1} step={.02} fineStep={.005} defaultValue={0} value={channel?.filter ?? 0}
          disabled={Boolean(filterReason)} valueText={(value) => value === 0 ? "OFF" : `${value > 0 ? "HPF" : "LPF"} ${Math.abs(value * 100).toFixed(0)}%`} onChange={on.onFilter} />
        </div>

        <button type="button" className={cn("dj-mixer-cue", channel?.pfl && "is-on")} disabled={Boolean(pflReason)} aria-pressed={Boolean(channel?.pfl)}
          title={pflReason ?? "ヘッドホンでこのチャンネルをモニターする"} onClick={() => on.onPfl?.(!channel?.pfl)}>CUE</button>

        <div className="dj-mixer-sliders">
          <label className="dj-mixer-slider">
            <Fader label={`Deck ${deck} チャンネルフェーダー`} min={0} max={1} value={channel?.gain ?? 0} vertical
              disabled={Boolean(gainReason)} onChange={on.onGain} />
            <span className="dj-control-label">LEVEL</span>
          </label>
        </div>
      </div>;
    })}
  </div>;
}
