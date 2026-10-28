// Dev-only fixture page for the startup screen. main.tsx loads it only when
// import.meta.env.DEV and the URL has ?preview=startup, so production builds
// drop it entirely.
//   ?preview=startup&state=progress|slow|error|live&step=0-5&theme=light|dark|system
import { useEffect, useState } from "react";
import { ThemeProvider } from "@/components/theme-provider";
import { LoadingScreen } from "@/components/LoadingScreen";
import type { StartupProgress } from "@/hooks/useStartupProgress";

// Mirrors the milestones backend/main.py and backend/server.py report.
const STAGES: Array<Pick<StartupProgress, "stage" | "label">> = [
  { stage: "launch", label: "解析サービスを起動しています" },
  { stage: "settings", label: "設定・保存先を読み込んでいます" },
  { stage: "services", label: "ライブラリ・解析・再生の機能を読み込んでいます" },
  { stage: "database", label: "楽曲データベースを準備しています" },
  { stage: "queue", label: "前回の解析・取り込み状態を復元しています" },
  { stage: "ready", label: "起動処理が完了しました。接続を確認しています" },
];

type PreviewState = "progress" | "slow" | "error" | "live";
type PreviewTheme = "light" | "dark" | "system";

const params = new URLSearchParams(window.location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  allowed.includes(value as T) ? (value as T) : fallback;
const state = pick<PreviewState>(params.get("state"), ["progress", "slow", "error", "live"], "progress");
const theme = pick<PreviewTheme>(params.get("theme"), ["light", "dark", "system"], "system");
const firstStep = Math.min(5, Math.max(0, Number.parseInt(params.get("step") ?? "", 10) || 0));

const fixture = (step: number, error?: string): StartupProgress => ({ ...STAGES[step], completed: step, total: 5, error });

function Preview() {
  const [step, setStep] = useState(firstStep);
  const [seconds, setSeconds] = useState(state === "slow" ? 22 : state === "error" ? 9 : 4);
  useEffect(() => {
    if (state !== "live") return;
    const clock = setInterval(() => setSeconds(value => value + 1), 1000);
    const stages = setInterval(() => setStep(value => Math.min(5, value + 1)), 2500);
    return () => { clearInterval(clock); clearInterval(stages); };
  }, []);
  const error = state === "error" ? "解析サービスが終了しました（終了コード: Some(1)）" : undefined;
  const connectionError = state === "slow" || state === "error"
    ? "接続を再試行中: Failed to fetch"
    : "";
  return (
    <LoadingScreen
      progress={fixture(step, error)}
      seconds={seconds}
      connectionError={connectionError}
      onRetry={() => {
        // Like useStartupProgress, a retry restarts the hook's own elapsed counter.
        console.info("[startup preview] retry requested");
        setSeconds(0);
      }}
    />
  );
}

export function StartupPreview() {
  // A dedicated storage key keeps the preview from changing the app's saved theme.
  return (
    <ThemeProvider defaultTheme={theme} storageKey="plumdeck-startup-preview-theme">
      <Preview />
    </ThemeProvider>
  );
}
