import { useEffect, useRef, useState, type ReactNode } from "react";
import { Check, ChevronRight, CircleAlert, Copy, Loader2, RotateCcw, RotateCw } from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import type { StartupProgress } from "@/hooks/useStartupProgress";
import { Button } from "@/components/ui/button";
import {
  STARTUP_SLOW_HINT, STARTUP_SLOW_SECONDS, advanceStartupBar, formatElapsed, startupDetailsText, startupMessage,
  startupPhrases, startupRatio,
} from "@/lib/startup-presentation";
import { version } from "../../package.json";
import "./loading-screen.css";

const REDUCED_MOTION = "(prefers-reduced-motion: reduce)";
const RETRY_FEEDBACK_MS = 1500;

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

/**
 * Elapsed startup time that never goes back. useStartupProgress resets its
 * `seconds` to 0 on every retry, which would hide the "slow" reassurance (and
 * the button the user just pressed) for another 15 seconds. Counting from when
 * this screen mounted keeps the contract of the hook unchanged.
 */
function useElapsedSeconds(seconds: number) {
  const [sinceMount, setSinceMount] = useState(0);
  useEffect(() => {
    const mountedAt = Date.now();
    const timer = setInterval(() => setSinceMount(Math.floor((Date.now() - mountedAt) / 1000)), 1000);
    return () => clearInterval(timer);
  }, []);
  const [peak, setPeak] = useState(seconds);
  const current = Math.max(seconds, sinceMount);
  if (current > peak) setPeak(current);
  return Math.max(peak, current);
}

/** Flips back to "idle" a moment after a one-shot action, so its label can say what happened. */
function useTransient<T extends string>(idle: T, ms: number) {
  const [value, setValue] = useState<T>(idle);
  useEffect(() => {
    if (value === idle) return;
    const timer = setTimeout(() => setValue(idle), ms);
    return () => clearTimeout(timer);
  }, [value, idle, ms]);
  return [value, setValue] as const;
}

type RetryState = { phase: "checking" | "done"; duringError: boolean } | null;

export function LoadingScreen({ progress, seconds, connectionError, onRetry }: {
  progress: StartupProgress; seconds: number; connectionError: string; onRetry: () => void;
}) {
  const error = progress.error?.trim() || "";
  const elapsed = useElapsedSeconds(seconds);
  const slow = !error && elapsed >= STARTUP_SLOW_SECONDS;
  const reducedMotion = usePrefersReducedMotion();
  const shown = useStartupBar(progress.completed, progress.total, !error, reducedMotion);
  const message = startupMessage(progress);
  const restartButton = useRef<HTMLButtonElement>(null);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [retryState, setRetryState] = useState<RetryState>(null);
  const [restart, setRestart] = useState<"idle" | "restarting" | "failed">("idle");
  const [copied, setCopied] = useTransient<"idle" | "copied" | "failed">("idle", 2000);
  const retrying = retryState?.phase === "checking";

  useEffect(() => {
    if (error) restartButton.current?.focus();
  }, [error]);

  useEffect(() => {
    if (!retrying) return;
    const timer = setTimeout(() => setRetryState(state => state && { ...state, phase: "done" }), RETRY_FEEDBACK_MS);
    return () => clearTimeout(timer);
  }, [retrying]);

  const retry = () => {
    if (retrying) return;
    setRetryState({ phase: "checking", duringError: Boolean(error) });
    onRetry();
  };

  // A failed sidecar is not started again by onRetry, so the restart is the action that actually recovers.
  const restartApp = async () => {
    setRestart("restarting");
    try {
      if (isTauri()) await relaunch();
      else window.location.reload();
    } catch (cause) {
      console.error("Relaunch from the startup screen failed:", cause);
      setRestart("failed");
    }
  };

  const copyDetails = async () => {
    try {
      await navigator.clipboard.writeText(startupDetailsText({ version, progress, elapsedSeconds: elapsed, connectionError }));
      setCopied("copied");
    } catch {
      setCopied("failed");
    }
  };

  const errorNotice = restart === "failed"
    ? "再起動できませんでした。アプリを終了して、もう一度開いてください。"
    : error && retryState?.phase === "done" && retryState.duringError
      ? "まだ起動できません。アプリを再起動してください。"
      : "";

  return (
    <main className="startup-screen relative flex min-h-screen w-full flex-col items-center overflow-hidden bg-background px-4 text-foreground">
      {/*
        The brand sits at a fixed distance from the top and everything below it only grows downwards,
        so neither a state change nor opening 詳細 moves the logo. Taller content scrolls the page.
      */}
      <div className="relative z-10 flex w-full max-w-[22rem] flex-col items-center pt-[clamp(2.5rem,calc(50vh_-_10rem),20rem)]">
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

        <div className="startup-enter-late mt-12 flex w-full flex-col items-center">
          {error ? (
            <div className="flex w-full flex-col items-center text-center">
              <div role="alert">
                <p className="flex items-center justify-center gap-2 text-base font-semibold">
                  <CircleAlert aria-hidden="true" className="startup-error-icon h-[18px] w-[18px]" />
                  起動できませんでした
                </p>
                <p className="mt-2 text-sm leading-relaxed text-foreground/70">
                  <Phrases text="準備の途中で問題が発生しました。" />
                  <br />
                  <Phrases text="アプリを再起動して、もう一度お試しください。" />
                </p>
              </div>
              <div className="mt-6 flex flex-wrap items-center justify-center gap-2">
                <Button
                  ref={restartButton}
                  className="min-w-36 rounded-full"
                  onClick={() => void restartApp()}
                  aria-busy={restart === "restarting"}
                >
                  {restart === "restarting"
                    ? <Loader2 aria-hidden="true" className="animate-spin motion-reduce:animate-none" />
                    : <RotateCcw aria-hidden="true" />}
                  {restart === "restarting" ? "再起動しています…" : "アプリを再起動"}
                </Button>
                <Button variant="outline" className="min-w-24 rounded-full" onClick={retry} aria-busy={retrying}>
                  {retrying && <Loader2 aria-hidden="true" className="animate-spin motion-reduce:animate-none" />}
                  {retrying ? "確認しています…" : "再試行"}
                </Button>
              </div>
              <p role="status" aria-live="polite" className="mt-3 min-h-5 text-xs leading-relaxed text-foreground/70">
                {errorNotice && <Phrases text={errorNotice} />}
              </p>
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
                  <p className="startup-fade mt-3 text-xs leading-relaxed text-foreground/70">
                    <Phrases text="少し時間がかかっています。" />
                    <br />
                    <Phrases text={STARTUP_SLOW_HINT} />
                  </p>
                )}
              </div>
            </>
          )}

          {(error || slow) && (
            <StartupDetails
              className={error ? "mt-3" : "mt-6"}
              open={detailsOpen}
              onOpenChange={setDetailsOpen}
              reducedMotion={reducedMotion}
              actions={(
                <>
                  {!error && (
                    <Button variant="outline" size="sm" className="rounded-full" onClick={retry} aria-busy={retrying}>
                      {retrying
                        ? <Loader2 aria-hidden="true" className="animate-spin motion-reduce:animate-none" />
                        : <RotateCw aria-hidden="true" />}
                      {retrying ? "確認しています…" : "接続を再確認"}
                    </Button>
                  )}
                  <Button variant="ghost" size="sm" className="rounded-full text-foreground/75" onClick={() => void copyDetails()}>
                    {copied === "copied" ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                    {copied === "copied" ? "コピーしました" : copied === "failed" ? "コピーできませんでした" : "詳細をコピー"}
                  </Button>
                  <span role="status" className="sr-only">
                    {copied === "copied" ? "詳細をコピーしました" : copied === "failed" ? "詳細をコピーできませんでした" : ""}
                  </span>
                </>
              )}
            >
              {error && <DetailRow term="エラー"><Phrases text={error} /></DetailRow>}
              {progress.label && <DetailRow term="処理"><Phrases text={progress.label} /></DetailRow>}
              <DetailRow term="状態">
                <span className="font-mono">{progress.stage || "—"}</span>
                <span className="text-foreground/60"> · {progress.completed}/{progress.total}</span>
              </DetailRow>
              <DetailRow term="経過時間">{formatElapsed(elapsed)}</DetailRow>
              {connectionError && <DetailRow term="接続"><Phrases text={connectionError} /></DetailRow>}
            </StartupDetails>
          )}
        </div>
      </div>

      <p className="relative z-10 mt-auto pb-5 pt-10 text-[11px] tracking-wide text-foreground/60 tabular-nums">v{version}</p>
    </main>
  );
}

/** Lets Japanese copy wrap only between phrases instead of mid-word. */
function Phrases({ text }: { text: string }) {
  return startupPhrases(text).map((part, index) => (
    part === " " ? " " : <span key={index} className="inline-block max-w-full">{part}</span>
  ));
}

function StartupDetails({ className, open, onOpenChange, reducedMotion, actions, children }: {
  className: string; open: boolean; onOpenChange: (open: boolean) => void; reducedMotion: boolean;
  actions: ReactNode; children: ReactNode;
}) {
  return (
    <details
      open={open}
      onToggle={event => {
        const details = event.currentTarget;
        onOpenChange(details.open);
        // The logo stays put, so on short windows the opened panel can start below the fold.
        if (details.open) details.scrollIntoView({ block: "nearest", behavior: reducedMotion ? "auto" : "smooth" });
      }}
      className={`startup-fade group w-full text-left ${className}`}
    >
      <summary className="mx-auto flex w-fit cursor-pointer list-none items-center gap-1 rounded-md px-2 py-1 text-xs text-foreground/70 transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
        <ChevronRight aria-hidden="true" className="h-3.5 w-3.5 transition-transform group-open:rotate-90 motion-reduce:transition-none" />
        詳細
      </summary>
      <div className="mt-3 rounded-lg border bg-card/70 p-3 text-xs leading-relaxed backdrop-blur-sm">
        <dl className="space-y-2">{children}</dl>
        <div className="mt-3 flex flex-wrap items-center gap-1.5">{actions}</div>
      </div>
    </details>
  );
}

function DetailRow({ term, children }: { term: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[4.5rem_1fr] gap-2">
      <dt className="text-foreground/60">{term}</dt>
      <dd className="min-w-0 [overflow-wrap:anywhere]">{children}</dd>
    </div>
  );
}
