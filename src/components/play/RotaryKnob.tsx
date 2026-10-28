import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type WheelEvent } from "react";
import { cn } from "@/lib/utils";

type Props = {
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  fineStep?: number;
  defaultValue: number;
  /** Value that sits at 12 o'clock. Give it whenever unity is not the midpoint
   * of the range, so the pointer matches the hardware detent instead of the
   * arithmetic middle. Omit for a plain linear sweep. */
  center?: number;
  disabled?: boolean;
  valueText?: (value: number) => string;
  onChange?: (value: number) => void;
  className?: string;
  /** Makes the label a button (EQ kill). */
  onLabelClick?: () => void;
  labelActive?: boolean;
  labelTitle?: string;
};

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

export function RotaryKnob({ label, value, min, max, step = .01, fineStep = step / 5, defaultValue, center, disabled, valueText, onChange, className, onLabelClick, labelActive, labelTitle }: Props) {
  const pivot = center !== undefined && center > min && center < max ? center : null;
  // Travel is measured in sweep fraction, not value, so a pivoted knob still
  // follows the pointer at one uniform rate across both halves.
  const toRatio = (v: number) => max === min ? 0
    : pivot === null ? (v - min) / (max - min)
    : v <= pivot ? (v - min) / (pivot - min) / 2 : .5 + (v - pivot) / (max - pivot) / 2;
  const fromRatio = (r: number) => pivot === null ? min + r * (max - min)
    : r <= .5 ? min + r * 2 * (pivot - min) : pivot + (r - .5) * 2 * (max - pivot);
  const [draft, setDraft] = useState(value);
  const drag = useRef<{ pointerId: number; y: number; value: number } | null>(null);
  const draftRef = useRef(value);
  const frame = useRef<number | null>(null);
  const emitted = useRef(value);

  useEffect(() => {
    if (!drag.current) {
      draftRef.current = value;
      emitted.current = value;
      setDraft(value);
    }
  }, [value]);
  useEffect(() => () => { if (frame.current !== null) cancelAnimationFrame(frame.current); }, []);

  const quantize = (next: number, increment = step) => {
    const rounded = min + Math.round((next - min) / increment) * increment;
    return clamp(Number(rounded.toFixed(6)), min, max);
  };
  const commit = (next: number) => {
    const safe = quantize(next);
    draftRef.current = safe;
    setDraft(safe);
    if (!disabled && safe !== emitted.current) { emitted.current = safe; onChange?.(safe); }
  };
  const emitLive = (next: number) => {
    draftRef.current = next;
    setDraft(next);
    if (frame.current !== null) return;
    frame.current = requestAnimationFrame(() => { frame.current = null; commit(draftRef.current); });
  };
  const nudge = (direction: number, fine: boolean) => commit(draftRef.current + direction * (fine ? fineStep : step));
  const finishDrag = (event: PointerEvent<HTMLDivElement>, commitValue: boolean) => {
    if (!drag.current || drag.current.pointerId !== event.pointerId) return;
    const next = draftRef.current;
    drag.current = null;
    if (frame.current !== null) { cancelAnimationFrame(frame.current); frame.current = null; }
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
    if (commitValue) commit(next);
    else { draftRef.current = value; setDraft(value); }
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (disabled) return;
    if (event.key === "ArrowUp" || event.key === "ArrowRight") { event.preventDefault(); nudge(1, event.shiftKey); }
    else if (event.key === "ArrowDown" || event.key === "ArrowLeft") { event.preventDefault(); nudge(-1, event.shiftKey); }
    else if (event.key === "Home") { event.preventDefault(); commit(defaultValue); }
  };
  const ratio = toRatio(draft);
  const rotation = -135 + ratio * 270;

  return <div className={cn("dj-knob-control", disabled && "is-disabled", className)}>
    {onLabelClick
      ? <button type="button" className={cn("dj-knob-label dj-knob-label--button", labelActive && "is-on")} disabled={disabled} aria-pressed={Boolean(labelActive)} title={labelTitle} onClick={onLabelClick}>{label}</button>
      : <span className="dj-knob-label">{label}</span>}
    <div className="dj-knob" role="slider" tabIndex={disabled ? -1 : 0} aria-label={label} aria-disabled={disabled || undefined}
      aria-valuemin={min} aria-valuemax={max} aria-valuenow={draft} aria-valuetext={valueText?.(draft)}
      title={`${label}: ${valueText?.(draft) ?? draft}（ドラッグ / ホイール / 矢印、Shiftで微調整、Home・ダブルクリックでリセット）`}
      onPointerDown={(event) => {
        if (disabled || event.button !== 0) return;
        event.preventDefault();
        event.currentTarget.setPointerCapture(event.pointerId);
        drag.current = { pointerId: event.pointerId, y: event.clientY, value: draftRef.current };
      }}
      onPointerMove={(event) => {
        if (!drag.current || drag.current.pointerId !== event.pointerId) return;
        const sensitivity = event.shiftKey ? 720 : 180;
        const travel = (drag.current.y - event.clientY) / sensitivity;
        const next = quantize(fromRatio(clamp(toRatio(drag.current.value) + travel, 0, 1)), event.shiftKey ? fineStep : step);
        emitLive(next);
      }}
      onPointerUp={(event) => finishDrag(event, true)} onPointerCancel={(event) => finishDrag(event, true)} onLostPointerCapture={(event) => finishDrag(event, true)}
      onWheel={(event: WheelEvent<HTMLDivElement>) => { if (!disabled) { event.preventDefault(); nudge(event.deltaY < 0 ? 1 : -1, event.shiftKey); } }}
      onKeyDown={onKeyDown} onDoubleClick={() => { if (!disabled) commit(defaultValue); }}>
      <i style={{ transform: `rotate(${rotation}deg)` }} />
    </div>
    <output className="dj-knob-value">{valueText?.(draft) ?? draft}</output>
  </div>;
}
