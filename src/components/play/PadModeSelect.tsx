import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { cn } from "@/lib/utils";
import { PAD_MODES, type PadMode } from "./PerformancePads";

type Props = {
  value: PadMode;
  /** Modes to offer; defaults to all. */
  modes?: typeof PAD_MODES;
  disabled?: boolean;
  deckId: string;
  onChange: (mode: PadMode) => void;
};

/**
 * rekordbox の `HOT CUE ∨` セレクタ。ネイティブ select だと macOS のポップアップが
 * そのまま出てデッキの見た目から外れるので、同じ形のリストボックスを自前で出す。
 */
export function PadModeSelect({ value, modes = PAD_MODES, disabled, deckId, onChange }: Props) {
  const [open, setOpen] = useState(false);
  const [cursor, setCursor] = useState(() => modes.findIndex((entry) => entry.mode === value));
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLUListElement>(null);
  // デッキ側が overflow:hidden なので、メニューは body に出して座標で置く。
  const [anchor, setAnchor] = useState({ left: 0, top: 0 });
  const label = modes.find((entry) => entry.mode === value)?.label ?? "HOT CUE";

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node;
      if (!root.current?.contains(target) && !menu.current?.contains(target)) setOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [open]);

  useEffect(() => { if (open) setCursor(modes.findIndex((entry) => entry.mode === value)); }, [open, value]);

  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const rect = trigger.current?.getBoundingClientRect();
      if (rect) {
        const height = menu.current?.offsetHeight ?? 240;
        const width = menu.current?.offsetWidth ?? 108;
        const top = rect.top >= height + 7 ? rect.top - height - 3 : rect.bottom + 3;
        setAnchor({
          left: Math.max(4, Math.min(rect.left, window.innerWidth - width - 4)),
          top: Math.max(4, Math.min(top, window.innerHeight - height - 4)),
        });
      }
    };
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => { window.removeEventListener("resize", place); window.removeEventListener("scroll", place, true); };
  }, [open]);

  const commit = (index: number) => {
    const entry = modes[index];
    if (!entry) return;
    onChange(entry.mode);
    setOpen(false);
  };

  return <div className="dj-pad-mode-select" ref={root}>
    <button type="button" ref={trigger} className="dj-pad-mode-trigger" disabled={disabled}
      aria-haspopup="listbox" aria-expanded={open} aria-label={`Deck ${deckId} PADモード`}
      onClick={() => setOpen((current) => !current)}
      onKeyDown={(event) => {
        if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); setOpen(true); }
      }}>
      <span>{label}</span>
      <svg viewBox="0 0 10 6" aria-hidden className="dj-pad-mode-chevron"><path d="M0 0h10L5 6z" fill="currentColor" /></svg>
    </button>
    {open && createPortal(<ul className="dj-pad-mode-menu" style={anchor} role="listbox" tabIndex={-1} aria-label="PADモード"
      ref={(node) => { menu.current = node; node?.focus(); }}
      onKeyDown={(event) => {
        if (event.key === "Escape") { event.preventDefault(); setOpen(false); return; }
        if (event.key === "ArrowDown") { event.preventDefault(); setCursor((index) => Math.min(modes.length - 1, index + 1)); return; }
        if (event.key === "ArrowUp") { event.preventDefault(); setCursor((index) => Math.max(0, index - 1)); return; }
        if (event.key === "Enter" || event.key === " ") { event.preventDefault(); commit(cursor); }
      }}>
      {modes.map((entry, index) => <li key={entry.mode} role="option" aria-selected={entry.mode === value}
        className={cn("dj-pad-mode-option", entry.mode === value && "is-current", index === cursor && "is-cursor")}
        onMouseEnter={() => setCursor(index)}
        onClick={() => commit(index)}>
        <i aria-hidden>{entry.mode === value ? "✓" : ""}</i>{entry.label}
      </li>)}
    </ul>, document.body)}
  </div>;
}
