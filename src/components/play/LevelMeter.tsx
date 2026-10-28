import { useEffect, useRef, useState, type CSSProperties } from "react";
import { cn } from "@/lib/utils";
import type { BusMeter } from "@/types/dj-engine";
import { meterPosition, nextPeakHold, type PeakHold } from "./mixer-math";

/** One bar on the dB scale with a falling peak-hold mark. */
export function LevelMeter({ peak, label, className }: { peak: number; label: string; className?: string }) {
  const hold = useRef<PeakHold | null>(null);
  const position = meterPosition(peak);
  hold.current = nextPeakHold(hold.current, position, performance.now());
  const decibels = peak > 0 ? Math.round(20 * Math.log10(peak)) : null;
  return <div className={cn("dj-mixer-meter", className)} role="meter" aria-label={label} aria-valuemin={-48} aria-valuemax={0}
    aria-valuenow={decibels ?? -48} aria-valuetext={decibels === null ? "無音" : `${decibels} dB`}
    style={{ "--level": position, "--hold": hold.current.position } as CSSProperties}><i /><b /></div>;
}

/** Master L/R meters with a clip lamp that stays lit for 2 s. */
export function MasterMeter({ meter }: { meter: BusMeter | null | undefined }) {
  const [clipAt, setClipAt] = useState<number | null>(null);
  useEffect(() => { if (meter?.clip) setClipAt(Date.now()); }, [meter]);
  useEffect(() => {
    if (clipAt === null) return;
    const timer = window.setTimeout(() => setClipAt(null), 2000);
    return () => window.clearTimeout(timer);
  }, [clipAt]);
  if (!meter) return null;
  const left = meter.peakL ?? meter.peak, right = meter.peakR ?? meter.peak;
  return <div className="dj-master-meter" title="MASTER L/R（−48〜0 dBFS）。CLIP は 0 dBFS に達したときに点灯します">
    <span className="dj-control-label">MASTER</span>
    <LevelMeter peak={left} label="MASTER L レベル" className="dj-meter--horizontal" />
    <LevelMeter peak={right} label="MASTER R レベル" className="dj-meter--horizontal" />
    <span className={cn("dj-clip", clipAt !== null && "is-on")} role="status" aria-label={clipAt !== null ? "クリップしています" : "クリップなし"}>CLIP</span>
  </div>;
}
