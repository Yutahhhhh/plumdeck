import { useEffect, useRef } from "react";
import { drawThreeBand } from "@/services/waveform/three-band";
import { useThreeBand } from "@/components/play/useThreeBand";

interface WaveformProps {
  /** Library track id; the 3-band overview comes from rekordbox or plumdeck's own analysis. */
  trackId: number | null;
  progress: number;
  duration: number;
  onSeek: (ratio: number) => void;
  isExpanded: boolean;
}

/** Mini player strip: the same 3-band overview as the Play decks, with the played part dimmed. */
export function Waveform({ trackId, progress, duration, onSeek, isExpanded }: WaveformProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const { wave } = useThreeBand(trackId, "overview");

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ratio = Math.min(2, window.devicePixelRatio || 1);
    const width = canvas.clientWidth || 1000, height = canvas.clientHeight || 32;
    canvas.width = Math.round(width * ratio); canvas.height = Math.round(height * ratio);
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, width, height);
    const played = Math.max(0, Math.min(1, progress / (duration || 1))) * width;
    if (!wave) {
      ctx.fillStyle = "#8888884d"; ctx.fillRect(0, height / 2 - 1, width, 2);
      ctx.fillStyle = "#2050D8"; ctx.fillRect(0, height / 2 - 1, played, 2);
      return;
    }
    const span = duration > 0 ? duration * 1000 : wave.durationMs;
    drawThreeBand(ctx, wave, { startMs: 0, spanMs: span, width, height, style: "overview", ratio });
    ctx.fillStyle = "#00000073"; ctx.fillRect(0, 0, played, height);
    ctx.fillStyle = "#ffffff"; ctx.fillRect(Math.max(0, played - 1), 0, 2, height);
  }, [wave, progress, isExpanded, duration]);

  return (
    <div
      className="absolute -top-4 left-0 right-0 h-8 cursor-pointer group z-20 flex items-end"
      onClick={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        onSeek((e.clientX - rect.left) / rect.width);
      }}
    >
      <canvas ref={canvasRef} className="w-full h-full block opacity-80 hover:opacity-100 transition-opacity" />
    </div>
  );
}
