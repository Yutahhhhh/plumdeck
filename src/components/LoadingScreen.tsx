import { useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronRight, CircleAlert, Loader2, RotateCw } from "lucide-react";
import type { StartupProgress } from "@/hooks/useStartupProgress";
import { Button } from "@/components/ui/button";
import {
  STARTUP_SLOW_SECONDS, advanceStartupBar, formatElapsed, startupMessage, startupRatio,
} from "@/lib/startup-presentation";
import { version } from "../../package.json";
import "./loading-screen.css";

const REDUCED_MOTION = "(prefers-reduced-motion: reduce)";

function usePrefersReducedMotion() {
  const [reduced, setReduced] = useState(() => window.matchMedia?.(REDUCED_MOTION).matches ?? false);
  useEffect(() => {
    const query = window.matchMedia?.(REDUCED_MOTION);
    if (!query) return;
    const update = () => setReduced(query.matches);
    update();
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);
  return reduced;
}

function useStartupBar(completed: number, total: number, running: boolean, reducedMotion: boolean) {
  const [shown, setShown] = useState(() => startupRatio(completed, total));
  useEffect(() => {
    setShown(current => advanceStartupBar(current, completed, total, false));
    if (!running || reducedMotion) return;
    const timer = setInterval(() => setShown(current => advanceStartupBar(current, completed, total, true)), 400);
    return () => clearInterval(timer);
  }, [completed, total, running, reducedMotion]);
  return shown;
}

export function LoadingScreen({ progress, seconds, connectionError, onRetry }: {
  progress: StartupProgress; seconds: number; connectionError: string; onRetry: () => void;
}) {
  const error = progress.error?.trim() || "";
  const slow = !error && seconds >= STARTUP_SLOW_SECONDS;
  const reducedMotion = usePrefersReducedMotion();
  const shown = useStartupBar(progress.completed, progress.total, !error, reducedMotion);
  const message = startupMessage(progress);
  const retryButton = useRef<HTMLButtonElement>(null);
  const [retrying, setRetrying] = useState(false);

  useEffect(() => {
    if (error) retryButton.current?.focus();
  }, [error]);

  useEffect(() => {
    if (!retrying) return;
    const timer = setTimeout(() => setRetrying(false), 1500);
    return () => clearTimeout(timer);
  }, [retrying]);

  const retry = () => {
    setRetrying(true);
    onRetry();
  };

  return (
    <main
      className="startup-screen flex min-h-screen w-full flex-col items-center overflow-hidden bg-background px-4 text-foreground"
    >
      <div className="relative z-10 flex w-full max-w-[22rem] flex-1 flex-col items-center justify-center pt-10">
        <div className="startup-enter relative mb-6 grid place-items-center">
          <div aria-hidden="true" className="startup-halo" />
          <div aria-hidden="true" className="startup-grooves" />
          <img
            src="/plumdeck-logo.svg"
            alt=""
            width={88}
            height={88}
            draggable={false}
            className="startup-logo relative h-[88px] w-[88px] select-none"
          />
        </div>
        <h1 className="startup-enter text-[1.875rem] font-semibold leading-none tracking-tight">plumdeck</h1>

        {/* Fixed height for every state so the logo never jumps when the content below changes. */}
        <div className="startup-enter-late mt-12 flex min-h-[12rem] w-full flex-col items-center">
          {error ? (
            <div className="flex w-full flex-col items-center text-center">
              <div role="alert">
                <p className="flex items-center justify-center gap-2 text-base font-semibold">
                  <CircleAlert aria-hidden="true" className="startup-error-icon h-[18px] w-[18px]" />
                  起動できませんでした
                </p>
                <p className="mt-2 text-sm leading-relaxed text-muted-foreground">
                  <Phrases text="準備の途中で問題が発生しました。" />
                  <br />
                  <Phrases text="解決しない場合は、アプリを再起動してください。" />
                </p>
              </div>
              <Button ref={retryButton} className="mt-6 min-w-32 rounded-full" onClick={retry} aria-busy={retrying}>
                {retrying ? <Loader2 aria-hidden="true" className="animate-spin" /> : <RotateCw aria-hidden="true" />}
                {retrying ? "確認しています…" : "再試行"}
              </Button>
            </div>
          ) : (
            <>
              <div
                role="progressbar"
                aria-label="起動の進行状況"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={Math.round(startupRatio(progress.completed, progress.total) * 100)}
                aria-valuetext={message}
                className="h-1 w-full max-w-[16rem] overflow-hidden rounded-full bg-foreground/10"
              >
                <div
                  className="startup-bar-fill"
                  style={{ transform: `translateX(-${(1 - Math.max(0.025, shown)) * 100}%)` }}
                />
              </div>
              <div role="status" aria-live="polite" className="mt-4 w-full text-center">
                <p key={message} className="startup-fade text-sm text-foreground/75">{message}</p>
                {slow && (
                  <p className="startup-fade mt-3 text-xs leading-relaxed text-foreground/65">
                    <Phrases text="少し時間がかかっています。" />
                    <br />
                    <Phrases text="ライブラリが大きいと、準備に時間がかかることがあります。" />
                  </p>
                )}
              </div>
            </>
          )}

          {(error || slow) && (
            <StartupDetails
              action={!error && (
                <Button variant="outline" size="sm" className="rounded-full" onClick={retry} aria-busy={retrying}>
                  {retrying ? <Loader2 aria-hidden="true" className="animate-spin" /> : <RotateCw aria-hidden="true" />}
                  {retrying ? "確認しています…" : "接続を再確認"}
                </Button>
              )}
            >
              <DetailRow term="状態">
                <span className="font-mono">{progress.stage || "—"}</span>
                <span className="text-muted-foreground"> · {progress.completed}/{progress.total}</span>
              </DetailRow>
              {progress.label && <DetailRow term="処理">{progress.label}</DetailRow>}
              <DetailRow term="経過時間">{formatElapsed(seconds)}</DetailRow>
              {error && <DetailRow term="エラー"><span className="font-mono">{error}</span></DetailRow>}
              {connectionError && <DetailRow term="接続"><span className="font-mono">{connectionError}</span></DetailRow>}
            </StartupDetails>
          )}
        </div>
      </div>

      <p className="relative z-10 pb-5 pt-2 text-[11px] tracking-wide text-muted-foreground tabular-nums">v{version}</p>
    </main>
  );
}

/** Lets Japanese copy wrap only between phrases (after 、 and 。) instead of mid-word. */
function Phrases({ text }: { text: string }) {
  return (text.match(/[^、。]+[、。]?/g) ?? [text]).map((part, index) => (
    <span key={index} className="inline-block">{part}</span>
  ));
}

function StartupDetails({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return (
    <details className="startup-fade group mt-6 w-full text-left">
      <summary className="mx-auto flex w-fit cursor-pointer list-none items-center gap-1 rounded-md px-2 py-1 text-xs text-foreground/65 transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
        <ChevronRight aria-hidden="true" className="h-3.5 w-3.5 transition-transform group-open:rotate-90 motion-reduce:transition-none" />
        詳細
      </summary>
      <div className="mt-3 rounded-lg border bg-card/70 p-3 text-xs backdrop-blur-sm">
        <dl className="space-y-2">{children}</dl>
        {action && <div className="mt-3">{action}</div>}
      </div>
    </details>
  );
}

function DetailRow({ term, children }: { term: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[4.5rem_1fr] gap-2">
      <dt className="text-muted-foreground">{term}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}
