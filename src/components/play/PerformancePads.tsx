import { padFxParams, padPreset, BEAT_LOOP_PAGES, BEAT_JUMP_PAGES, keyPad } from "@/services/midi/pad-presets";
import { emptyPadFxStack, pressPad, releaseAll, releasePad, type PadFxCommand, type PadFxStack } from "./pad-fx-stack";
import { padColor } from "@/services/midi/pad-colors";
export { CUE_COLORS } from "@/services/midi/pad-colors";
import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
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
  hotCueDisabledReason?: string;
  beatLoopDisabledReason?: string;
  beatJumpDisabledReason?: string;
  padFxDisabledReason?: string;
  onHotCue?: (index: number, clear: boolean) => void;
  onBeatLoop?: (beats: number) => void;
  onBeatJump?: (beats: number) => void;
  onFx?: (effect: FxEffect, enabled: boolean, mix: number, depth: number, beats?: number) => void;
  /** The host takes per-pad beats (`mixer.fx.beats`): send and show them. */
  fxBeats?: boolean;
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

/** Held pads are keyed per bank so a release after a bank switch still targets the right effect. */
const fxKey = (mode: 1 | 5, slot: number) => (mode === 5 ? 16 : 0) + (slot & 15);
const fxSlot = (key: number) => padPreset(key >= 16 ? 5 : 1, key & 15);

export function PerformancePads(props: Props) {
  const fxStack = useRef<PadFxStack>(emptyPadFxStack);
  const [fxHeld, setFxHeld] = useState<readonly number[]>([]);
  const onFxRef = useRef(props.onFx);
  const fxBeatsRef = useRef(Boolean(props.fxBeats));
  useEffect(() => { onFxRef.current = props.onFx; fxBeatsRef.current = Boolean(props.fxBeats); });
  // One effect slot per deck on this host: the last pressed pad sounds and
  // releasing it hands the slot back to a pad still held.
  const applyFx = (step: { stack: PadFxStack; commands: PadFxCommand[] }) => {
    fxStack.current = step.stack;
    setFxHeld(step.stack.held);
    for (const command of step.commands) {
      const preset = fxSlot(command.slot);
      const params = padFxParams(preset, fxBeatsRef.current);
      onFxRef.current?.(preset.effect, command.enabled, params.mix, params.depth, params.beats);
    }
  };
  const startFx = (key: number) => { if (!props.padFxDisabledReason) applyFx(pressPad(fxStack.current, key)); };
  const stopFx = (key: number) => applyFx(releasePad(fxStack.current, key));
  const stopAllFx = useRef(() => {});
  stopAllFx.current = () => applyFx(releaseAll(fxStack.current));
  useEffect(() => () => stopAllFx.current(), []);
  useEffect(() => { stopAllFx.current(); }, [props.mode, props.padFxDisabledReason]);
  useEffect(() => {
    const releaseEverything = () => stopAllFx.current();
    const onVisibility = () => { if (document.hidden) releaseEverything(); };
    window.addEventListener("blur", releaseEverything);
    document.addEventListener("visibilitychange", onVisibility);
    return () => { window.removeEventListener("blur", releaseEverything); document.removeEventListener("visibilitychange", onVisibility); };
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

  if (props.mode === "padfx" || props.mode === "padfx2") {
    const bank = props.mode === "padfx2" ? 5 : 1;
    const sounding = fxHeld.length ? fxHeld[fxHeld.length - 1] : null;
    return <PadGrid label="Momentary pad effects">{Array.from({ length: 8 }, (_, index) => {
      const slot = index + (props.page ?? 0) * 8;
      const key = fxKey(bank, slot);
      const preset = padPreset(bank, slot);
      const beats = props.fxBeats && preset.beats ? formatBeats(preset.beats) : null;
      const disabledReason = props.padFxDisabledReason;
      const pointerStart = (event: PointerEvent<HTMLButtonElement>) => { if (event.button === 0) { event.currentTarget.setPointerCapture(event.pointerId); startFx(key); } };
      const keyStart = (event: KeyboardEvent<HTMLButtonElement>) => { if (!event.repeat && (event.key === " " || event.key === "Enter")) { event.preventDefault(); startFx(key); } };
      const keyStop = (event: KeyboardEvent<HTMLButtonElement>) => { if (event.key === " " || event.key === "Enter") stopFx(key); };
      const describe = `${preset.label}${preset.variant ? ` ${preset.variant}` : ""}${beats ? ` ${beats} 拍` : ""}`;
      return <button key={`${bank}:${slot}`} type="button" style={{ "--pad-color": padColor(bank, slot) } as CSSProperties}
        className={cn("dj-performance-pad dj-performance-pad--fx", sounding === key && "is-on", fxHeld.includes(key) && sounding !== key && "is-held")}
        aria-pressed={fxHeld.includes(key)} disabled={Boolean(disabledReason)}
        title={disabledReason ?? `${describe}（押している間だけ有効。同時に押すと最後に押したものが鳴り、離すと押したままのものに戻ります）`}
        onPointerDown={pointerStart} onPointerUp={() => stopFx(key)} onPointerCancel={() => stopFx(key)} onLostPointerCapture={() => stopFx(key)} onBlur={() => stopFx(key)} onKeyDown={keyStart} onKeyUp={keyStop}>
        {beats ? <><strong className="dj-fx-pad-beats dj-num">{beats}</strong><small>{preset.label}</small></> : <><strong className="dj-fx-pad-name">{preset.label}</strong>{preset.variant && <small>{preset.variant}</small>}</>}
      </button>;
    })}</PadGrid>;
  }

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
