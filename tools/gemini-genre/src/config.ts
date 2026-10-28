import { fileURLToPath } from "node:url";
import path from "node:path";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, "..");

export type Mode = "genre" | "subgenre" | "both";

export interface Config {
  /** plumdeck backend base URL (the local FastAPI server). */
  plumdeckApi: string;
  /** Gemini Gem URL to drive. */
  gemUrl: string;
  /** Which field(s) to classify. */
  mode: Mode;
  /** Tracks per Gemini request. "数百件" — a few hundred. */
  batchSize: number;
  /** JSON input or readable one-track-per-line input. */
  promptFormat: "json" | "lines";
  /** Continue the same Gem conversation between batches. */
  reuseChat: boolean;
  /** Hard cap on how many tracks to process this run (0 = no cap). */
  limit: number;
  /** Path to a JSON array of track ids; restrict the run to those (still must be unknown). */
  idsFile: string;
  /** Where per-batch JSON is written. */
  outDir: string;
  /** Persistent Chrome profile dir so the Google login survives runs. */
  userDataDir: string;
  /** Run Chrome headless. Default false — Google login needs a real window. */
  headless: boolean;
  /** Use the installed Google Chrome instead of bundled Chromium. */
  useChrome: boolean;
  /** Per-batch response timeout (ms). */
  responseTimeoutMs: number;
  /** Just open the browser so you can log into Google, then exit. */
  loginOnly: boolean;
  /** Skip the browser; POST already-saved out/<mode>/results.json back to plumdeck. */
  applyOnly: boolean;
  /** After each batch is saved, also POST it to plumdeck to update the DB. */
  apply: boolean;
  /** Let apply overwrite genres that are already verified. */
  overwrite: boolean;
}

function flag(args: string[], name: string): boolean {
  return args.includes(`--${name}`);
}

function opt(args: string[], name: string): string | undefined {
  const eq = args.find((a) => a.startsWith(`--${name}=`));
  if (eq) return eq.slice(name.length + 3);
  const i = args.indexOf(`--${name}`);
  if (i >= 0 && args[i + 1] && !args[i + 1].startsWith("--")) return args[i + 1];
  return undefined;
}

export function loadConfig(argv: string[]): Config {
  const args = argv.slice(2);
  const env = process.env;

  const mode = (opt(args, "mode") ?? env.GEMINI_GENRE_MODE ?? "genre") as Mode;
  if (!["genre", "subgenre", "both"].includes(mode)) {
    throw new Error(`invalid --mode "${mode}" (genre | subgenre | both)`);
  }

  const outDir =
    opt(args, "out") ?? env.GEMINI_GENRE_OUT ?? path.join(ROOT, "out");
  const promptFormat = opt(args, "prompt-format") ?? "json";
  if (promptFormat !== "json" && promptFormat !== "lines") {
    throw new Error(`invalid --prompt-format "${promptFormat}" (json | lines)`);
  }

  return {
    plumdeckApi: (
      opt(args, "api") ??
      env.PLUMDECK_API ??
      "http://localhost:8001"
    ).replace(/\/$/, ""),
    gemUrl:
      opt(args, "gem") ??
      env.GEMINI_GEM_URL ??
      "https://gemini.google.com/gem/0d9581d5a139",
    mode,
    batchSize: Number(opt(args, "batch") ?? env.GEMINI_GENRE_BATCH ?? 200),
    promptFormat,
    reuseChat: flag(args, "reuse-chat"),
    limit: Number(opt(args, "limit") ?? env.GEMINI_GENRE_LIMIT ?? 0),
    idsFile: opt(args, "ids-file") ?? env.GEMINI_GENRE_IDS_FILE ?? "",
    outDir,
    userDataDir:
      opt(args, "profile") ??
      env.GEMINI_GENRE_PROFILE ??
      path.join(ROOT, ".auth"),
    headless: flag(args, "headless") || env.GEMINI_GENRE_HEADLESS === "1",
    useChrome: !flag(args, "chromium") && env.GEMINI_GENRE_CHROMIUM !== "1",
    responseTimeoutMs: Number(
      opt(args, "timeout") ?? env.GEMINI_GENRE_TIMEOUT ?? 240_000,
    ),
    loginOnly: flag(args, "login-only"),
    applyOnly: flag(args, "apply-only"),
    apply: flag(args, "apply"),
    overwrite: flag(args, "overwrite"),
  };
}

export function describe(cfg: Config): string {
  return [
    `  api       ${cfg.plumdeckApi}`,
    `  gem       ${cfg.gemUrl}`,
    `  mode      ${cfg.mode}`,
    `  batch     ${cfg.batchSize}`,
    `  input     ${cfg.promptFormat}`,
    `  chat      ${cfg.reuseChat ? "continued" : "new per batch"}`,
    `  limit     ${cfg.limit || "(all)"}`,
    `  out       ${cfg.outDir}`,
    `  profile   ${cfg.userDataDir}`,
    `  browser   ${cfg.useChrome ? "chrome" : "chromium"}${cfg.headless ? " headless" : ""}`,
    `  apply     ${cfg.apply ? (cfg.overwrite ? "yes (overwrite)" : "yes") : "no (JSON only)"}`,
  ].join("\n");
}
