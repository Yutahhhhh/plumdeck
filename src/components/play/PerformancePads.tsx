import { padPreset, BEAT_LOOP_PAGES, BEAT_JUMP_PAGES, keyPad } from "@/services/midi/pad-presets";
import { padColor } from "@/services/midi/pad-colors";
export { CUE_COLORS } from "@/services/midi/pad-colors";
import { useEffect, useRef, type CSSProperties, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { cn } from "@/lib/utils";
import { formatBeats, sameBeats } from "./loop-math";
import type { PadEffect } from "@/types/dj-engine";
import type { MemoryCue } from "@/services/dj-engine/memory-cues";

/** rekordbox の PAD モードセレクタと同じ並び・同じ表記。 */
export type PadMode =
  | "hotcue" | "padfx" | "padfx2" | "slicer" | "beatjump" | "beatloop" | "keyboard"
  | "keyshift" | "seqcall" | "actcensr" | "stems" | "memorycue" | "sampler";
export type FxEffect = PadEffect;

/**
 * Pad modes in the order of the DJ software's mode selector. `capability`
 * hides a mode until the engine implements it, so no selectable mode is dead.
 */
export const PAD_MODES: { mode: PadMode; label: string; capability?: string }[] = [
  { mode: "hotcue", label: "HOT CUE" },
  { mode: "padfx", label: "PAD FX 1", capability: "mixer.fx" },
  { mode: "padfx2", label: "PAD FX 2", capability: "mixer.fx" },
  { mode: "slicer", label: "SLICER", capability: "deck.slicer" },
  { mode: "beatjump", label: "BEAT JUMP" },
  { mode: "beatloop", label: "BEAT LOOP" },
  { mode: "keyboard", label: "KEYBOARD", capability: "deck.key" },
  { mode: "keyshift", label: "KEY SHIFT", capability: "deck.key" },
  { mode: "seqcall", label: "SEQ. CALL", capability: "deck.sequencer" },
  { mode: "actcensr", label: "CENSOR", capability: "deck.censor" },
  { mode: "stems", label: "STEMS", capability: "deck.stems" },
  { mode: "memorycue", label: "MEMORY CUE" },
  { mode: "sampler", label: "SAMPLER", capability: "sampler" },
];

/** Modes this engine can drive. The current mode stays listed so it can be left. */
export const availablePadModes = (capabilities: readonly string[], current?: PadMode) =>
  PAD_MODES.filter((entry) => !entry.capability || capabilities.includes(entry.capability) || entry.mode === current);

/**
 * この音声エンジンに対応する op が無いモード。rekordbox 同様セレクタには出すが、
 * 押せない理由をパッドに出して「見えるのに無反応」を避ける。
 */
export const UNBACKED_PAD_MODES: Record<string, string> = {
  slicer: "SLICER はこの音声エンジン未対応",
  seqcall: "SEQ. CALL はこの音声エンジン未対応",
  actcensr: "CENSOR はこの音声エンジン未対応",
  stems: "STEMS はこの音声エンジン未対応",
};

/** パッドと波形で同じ色を使う。ずれると現場で見間違える。 */
/** エンジンの下限が 1/8 拍なので、rekordbox の 1/64〜 は出さずここから並べる。 */
const FX: { effect: FxEffect; label: string; param: string }[] = [
  { effect: "echo", label: "ECHO", param: "1/2" },
  { effect: "reverb", label: "REVERB", param: "50" },
  { effect: "flanger", label: "FLANGER", param: "16" },
  { effect: "filter", label: "FILTER", param: "—" },
  { effect: "phaser", label: "PHASER", param: "1/4" },
  { effect: "autopan", label: "PAN", param: "1/2" },
  { effect: "bitcrusher", label: "CRUSH", param: "—" },
  { effect: "distortion", label: "DIST", param: "—" },
];
const SLICER_SLOTS = [1, 2, 3, 4, 5, 6, 7, 8] as const;
const STEM_PADS = ["VOCAL", "INST", "BASS", "DRUMS", "FX VOCAL", "FX INST", "FX BASS", "FX DRUMS"] as const;
const CENSR_PADS = ["REV ROLL", "TRANS", "ECHO", "V.BRAKE", "IN", "OUT", "CLEAR", "ON/OFF"] as const;

const fraction = formatBeats;

type Props = {
  mode: PadMode;
  page?: number;
  onKey?: (semitones: number, keyboard: boolean) => void;
  hotCues?: (number | null)[];
  cueColors?: (string|null)[];
  formatTime: (ms: number) => string;
  /** キューが乗っている小節・拍。量子化されたか目で確かめられるようにする。 */
  describeCue?: (ms: number) => string | undefined;
  /** Length of the active loop in beats; highlights the matching pad. */
  selectedLoopBeats?: number;
  fxMix: number;
  activeFx?: FxEffect;
  hotCueDisabledReason?: string;
  beatLoopDisabledReason?: string;
  beatJumpDisabledReason?: string;
  padFxDisabledReason?: string;
  onHotCue?: (index: number, clear: boolean) => void;
  onBeatLoop?: (beats: number) => void;
  onBeatJump?: (beats: number) => void;
  onFx?: (effect: FxEffect, enabled: boolean, mix: number, depth: number) => void;
  memoryCues?: readonly MemoryCue[];
  /** Position of the memory cue last called; DELETE removes it. */
  selectedMemory?: number | null;
  memoryDisabledReason?: string;
  onMemoryCall?: (index: number) => void;
};

/** Physical pad order: 1–4 on the top row, 5–8 on the bottom row. */
function PadGrid({ label, children }: { label: string; children: ReactNode }) {
  return <div className="dj-pad-grid" aria-label={label}>{children}</div>;
}

function DeadPad({ text, reason, muted }: { text: string; reason: string; muted?: boolean }) {
  return <button className={cn("dj-performance-pad", muted && "dj-performance-pad--empty")} disabled title={reason}>
    <b>{text}</b>
  </button>;
}

export function PerformancePads(props: Props) {
  const activeFx = useRef(new Set<FxEffect>());
  const onFxRef = useRef(props.onFx);
  const fxMixRef = useRef(props.fxMix);
  useEffect(() => { onFxRef.current = props.onFx; });
  useEffect(() => {
    fxMixRef.current = props.fxMix;
    for (const effect of activeFx.current) onFxRef.current?.(effect, true, props.fxMix, props.fxMix);
  }, [props.fxMix]);
  const stopFx = (effect: FxEffect) => {
    if (!activeFx.current.delete(effect)) return;
    onFxRef.current?.(effect, false, fxMixRef.current, fxMixRef.current);
  };
  const startFx = (effect: FxEffect) => {
    if (props.padFxDisabledReason || activeFx.current.has(effect)) return;
    for (const active of activeFx.current) {
      activeFx.current.delete(active);
      onFxRef.current?.(active, false, fxMixRef.current, fxMixRef.current);
    }
    activeFx.current.add(effect);
    onFxRef.current?.(effect, true, fxMixRef.current, fxMixRef.current);
  };
  useEffect(() => () => {
    for (const effect of activeFx.current) onFxRef.current?.(effect, false, fxMixRef.current, fxMixRef.current);
    activeFx.current.clear();
  }, []);
  useEffect(() => {
    if (["padfx", "padfx2"].includes(props.mode) && !props.padFxDisabledReason) return;
    for (const effect of activeFx.current) onFxRef.current?.(effect, false, fxMixRef.current, fxMixRef.current);
    activeFx.current.clear();
  }, [props.mode, props.padFxDisabledReason]);
  useEffect(() => {
    const releaseAll = () => {
      for (const effect of activeFx.current) onFxRef.current?.(effect, false, fxMixRef.current, fxMixRef.current);
      activeFx.current.clear();
    };
    const onVisibility = () => { if (document.hidden) releaseAll(); };
    window.addEventListener("blur", releaseAll);
    document.addEventListener("visibilitychange", onVisibility);
    return () => { window.removeEventListener("blur", releaseAll); document.removeEventListener("visibilitychange", onVisibility); };
  }, []);

  const unbacked = UNBACKED_PAD_MODES[props.mode];

  if (props.mode === "hotcue") return <PadGrid label="Hot cues">{Array.from({ length: 8 }, (_, pad) => {
    const index = pad + (props.page ?? 0) * 8;
    const cue = props.hotCues?.[index];
    const set = cue !== null && cue !== undefined;
    const label = String.fromCharCode(65 + index);
    const where = set ? props.describeCue?.(cue) : undefined;
    const help = props.hotCueDisabledReason
      ?? (set ? `${where ? `${where} · ` : ""}ジャンプして再生` : "現在位置に設定");
    return <div key={index} className={cn("dj-performance-pad", "dj-performance-pad--cue", set && "is-set")} style={{ "--cue-color": padColor(0,index,props.cueColors?.[index]) } as CSSProperties}>
      <button type="button" className="dj-cue-hit" disabled={Boolean(props.hotCueDisabledReason)}
        title={`${label}: ${help}`} onClick={(event) => props.onHotCue?.(index, event.shiftKey)}>
        <b>{label}</b><span className="dj-num">{set ? props.formatTime(cue) : ""}</span>
      </button>
      {/* 削除は Shift+クリックでもできるが、見えないと消せないので × も出す。 */}
      {set && <button type="button" className="dj-cue-clear" disabled={Boolean(props.hotCueDisabledReason)}
        aria-label={`ホットキュー ${label} を削除`} title={`ホットキュー ${label} を削除`}
        onClick={() => props.onHotCue?.(index, true)}>×</button>}
    </div>;
  })}</PadGrid>;

  if (props.mode === "beatloop") return <PadGrid label="Beat loops">{BEAT_LOOP_PAGES[props.page ?? 0].map((beats) => <button key={beats}
    className={cn("dj-performance-pad", "dj-performance-pad--value", sameBeats(props.selectedLoopBeats, beats) && "is-selected")} disabled={Boolean(props.beatLoopDisabledReason)}
    aria-pressed={sameBeats(props.selectedLoopBeats, beats)}
    title={props.beatLoopDisabledReason ?? (sameBeats(props.selectedLoopBeats, beats) ? "ループを抜けます" : props.selectedLoopBeats ? `開始点を保ったまま ${fraction(beats)} 拍に変更` : `${fraction(beats)} 拍のループを現在位置から張ります`)} onClick={() => props.onBeatLoop?.(beats)}><b>{fraction(beats)}</b></button>)}</PadGrid>;

  if (props.mode === "beatjump") return <PadGrid label="Beat jumps">{BEAT_JUMP_PAGES[props.page??0].map((beats,index)=><button key={index}
    className="dj-performance-pad dj-performance-pad--value" disabled={Boolean(props.beatJumpDisabledReason)}
    style={{"--pad-color":padColor(2,index)} as CSSProperties}
    aria-label={`${Math.abs(beats)}拍${beats<0?"戻る":"進む"}`} onClick={()=>props.onBeatJump?.(beats)}><b>{beats<0?"◀ ":""}{Math.abs(beats)}{beats>0?" ▶":""}</b></button>)}</PadGrid>;

  if (props.mode === "padfx" || props.mode === "padfx2") return <PadGrid label="Momentary pad effects">{Array.from({ length: 8 }, (_, index) => {
    const preset = padPreset(props.mode === "padfx2" ? 5 : 1,index+(props.page??0)*8);
    const fx = FX.find(fx=>fx.effect===preset.effect) ?? {effect:preset.effect,label:preset.effect.toUpperCase(),param:String(preset.depth)};
    if (!fx) return <DeadPad key={index} text="" reason="このスロットは未割り当て" muted />;
    const disabledReason = props.padFxDisabledReason;
    const pointerStart = (event: PointerEvent<HTMLButtonElement>) => { if (event.button === 0) { event.currentTarget.setPointerCapture(event.pointerId); startFx(fx.effect); } };
    const keyStart = (event: KeyboardEvent<HTMLButtonElement>) => { if (!event.repeat && (event.key === " " || event.key === "Enter")) startFx(fx.effect); };
    const keyStop = (event: KeyboardEvent<HTMLButtonElement>) => { if (event.key === " " || event.key === "Enter") stopFx(fx.effect); };
    return <button key={`${props.page}:${index}`} style={{"--pad-color":padColor(props.mode==="padfx2"?5:1,index+(props.page??0)*8)} as CSSProperties} className={cn("dj-performance-pad dj-performance-pad--fx", props.activeFx === fx.effect && "is-on")} aria-pressed={props.activeFx === fx.effect} disabled={Boolean(disabledReason)} title={disabledReason ?? `${fx.label}（押している間だけ有効）`}
      onPointerDown={pointerStart} onPointerUp={() => stopFx(fx.effect)} onPointerCancel={() => stopFx(fx.effect)} onLostPointerCapture={() => stopFx(fx.effect)} onBlur={() => stopFx(fx.effect)} onKeyDown={keyStart} onKeyUp={keyStop}>
      <b>{fx.label}</b><span className="dj-num">{fx.param}</span>
    </button>;
  })}</PadGrid>;

  // 以下は rekordbox の見た目だけ再現し、対応する op が無いので押せない。
  if (props.mode === "keyshift" || props.mode === "keyboard") return <PadGrid label={props.mode === "keyboard" ? "Keyboard" : "Key shift"}>{Array.from({length:8}, (_, i) => keyPad(props.page ?? 1,i)).map((semitones,index) =>
    <button key={index} className="dj-performance-pad" disabled={semitones===null || Boolean(props.hotCueDisabledReason) || !props.onKey} onClick={()=>semitones!==null && props.onKey?.(semitones, props.mode === "keyboard")}><b>{semitones===null?"—":`${semitones>0?"+":""}${semitones}`}</b></button>)}</PadGrid>;

  if (props.mode === "slicer") return <PadGrid label="Slicer">{SLICER_SLOTS.map((slot) =>
    <DeadPad key={slot} text={String(slot)} reason={unbacked!} />)}</PadGrid>;

  if (props.mode === "stems") return <PadGrid label="Stems">{STEM_PADS.map((stem) =>
    <DeadPad key={stem} text={stem} reason={unbacked!} />)}</PadGrid>;

  if (props.mode === "actcensr") return <PadGrid label="Active censor">{CENSR_PADS.map((pad) =>
    <DeadPad key={pad} text={pad} reason={unbacked!} />)}</PadGrid>;


  if (props.mode === "memorycue") return <PadGrid label="Memory cues">{Array.from({ length: 8 }, (_, pad) => {
    const index = pad + (props.page ?? 0) * 8;
    const cue = props.memoryCues?.[index];
    if (!cue) return <DeadPad key={index} text="" reason={props.memoryDisabledReason ?? "空き：MEMORY で現在位置（ループ中はループ）を保存"} muted />;
    const where = props.describeCue?.(cue.positionMs) ?? props.formatTime(cue.positionMs);
    const selected = props.selectedMemory !== null && props.selectedMemory !== undefined && Math.abs(props.selectedMemory - cue.positionMs) <= 1;
    return <button key={index} type="button" className={cn("dj-performance-pad dj-performance-pad--memory", cue.endMs !== undefined && "is-loop", selected && "is-selected")}
      disabled={Boolean(props.memoryDisabledReason)} aria-pressed={selected}
      title={props.memoryDisabledReason ?? `${where}${cue.endMs !== undefined ? " · ループ" : ""}：呼び出して停止（選択中のものを DELETE で削除）`}
      onClick={() => props.onMemoryCall?.(index)}>
      <b>{cue.endMs !== undefined ? "LOOP" : "CUE"} {index + 1}</b><span className="dj-num">{props.formatTime(cue.positionMs)}</span>
    </button>;
  })}</PadGrid>;

  // SEQ. CALL は rekordbox でも空欄から始まる。
  return <PadGrid label="Sequence call">
    {Array.from({ length: 8 }, (_, index) => <DeadPad key={index} text="" reason={unbacked!} muted />)}
  </PadGrid>;
}
