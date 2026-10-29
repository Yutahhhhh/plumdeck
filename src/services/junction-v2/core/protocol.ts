// Junction v2 control messages on the "jlink" data channel (JSON, one object per
// message). Star topology: every guest talks only to the host.
// Shared verbatim with PlumDeck Lite. No compatibility with the old Junction.
import { isSessionState, type ParticipantKind, type SessionState } from "./turns.ts";

export const PROTOCOL = "plumdeck-jlink/2";
export const CONTROL_CHANNEL = "jlink";

export type GuestRequest =
  | { action: "next" }
  | { action: "releasePrevious" }
  | { action: "setNext"; target: string | null };

/**
 * Latency the guest adds between hearing its J feed and sending it back up,
 * in milliseconds. Used by the host only as a prior for the splice search
 * window; the splice itself is measured on the audio (see program.ts).
 */
export interface GuestReport {
  /** WebRTC jitter buffer on the guest's inbound J feed. */
  inboundJitterMs: number | null;
  /** J feed arrival -> J deck -> mix -> uplink sender, inside the guest. */
  loopMs: number | null;
  /** The J deck is audible in this guest's mix (level, crossfader). */
  jAudible: boolean;
}

export type GuestMessage =
  | { t: "hello"; protocol: string; name: string; kind: Exclude<ParticipantKind, "host"> }
  | { t: "request"; request: GuestRequest }
  | { t: "ping"; id: number; sentAt: number }
  | { t: "report"; report: GuestReport };

export type HostMessage =
  | { t: "welcome"; protocol: string; you: string; state: SessionState }
  | { t: "state"; state: SessionState }
  | { t: "pong"; id: number; sentAt: number }
  | { t: "error"; message: string };

const isObject = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object";
const finiteOrNull = (v: unknown) => v === null || (typeof v === "number" && Number.isFinite(v));

export function parseGuestMessage(raw: string): GuestMessage | null {
  let v: unknown;
  try { v = JSON.parse(raw); } catch { return null; }
  if (!isObject(v)) return null;
  switch (v.t) {
    case "hello":
      return typeof v.protocol === "string" && typeof v.name === "string" && (v.kind === "app" || v.kind === "lite")
        ? { t: "hello", protocol: v.protocol, name: v.name.slice(0, 40), kind: v.kind }
        : null;
    case "request": {
      const r = v.request;
      if (!isObject(r)) return null;
      if (r.action === "next" || r.action === "releasePrevious") return { t: "request", request: { action: r.action } };
      if (r.action === "setNext" && (r.target === null || typeof r.target === "string")) return { t: "request", request: { action: "setNext", target: r.target } };
      return null;
    }
    case "ping":
      return typeof v.id === "number" && typeof v.sentAt === "number" ? { t: "ping", id: v.id, sentAt: v.sentAt } : null;
    case "report": {
      const r = v.report;
      return isObject(r) && finiteOrNull(r.inboundJitterMs) && finiteOrNull(r.loopMs) && typeof r.jAudible === "boolean"
        ? { t: "report", report: { inboundJitterMs: r.inboundJitterMs as number | null, loopMs: r.loopMs as number | null, jAudible: r.jAudible } }
        : null;
    }
    default:
      return null;
  }
}

export function parseHostMessage(raw: string): HostMessage | null {
  let v: unknown;
  try { v = JSON.parse(raw); } catch { return null; }
  if (!isObject(v)) return null;
  switch (v.t) {
    case "welcome":
      return typeof v.protocol === "string" && typeof v.you === "string" && isSessionState(v.state)
        ? { t: "welcome", protocol: v.protocol, you: v.you, state: v.state }
        : null;
    case "state":
      return isSessionState(v.state) ? { t: "state", state: v.state } : null;
    case "pong":
      return typeof v.id === "number" && typeof v.sentAt === "number" ? { t: "pong", id: v.id, sentAt: v.sentAt } : null;
    case "error":
      return typeof v.message === "string" ? { t: "error", message: v.message } : null;
    default:
      return null;
  }
}

/**
 * Manual signaling: an invite/answer code is the SDP (with all ICE candidates
 * gathered) as base64url JSON, prefixed so a pasted chat message is recognised.
 */
const CODE_PREFIX = "JLINK2.";

export function encodeCode(description: { type: string; sdp?: string }): string {
  const json = JSON.stringify({ type: description.type, sdp: description.sdp ?? "" });
  const bytes = new TextEncoder().encode(json);
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return CODE_PREFIX + btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function decodeCode(code: string, expected: "offer" | "answer"): { type: "offer" | "answer"; sdp: string } {
  const trimmed = code.trim().replace(/\s+/g, "");
  if (!trimmed.startsWith(CODE_PREFIX)) throw new Error("Junctionのコードではありません");
  const b64 = trimmed.slice(CODE_PREFIX.length).replace(/-/g, "+").replace(/_/g, "/");
  let parsed: unknown;
  try {
    const binary = atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4));
    parsed = JSON.parse(new TextDecoder().decode(Uint8Array.from(binary, (c) => c.charCodeAt(0))));
  } catch {
    throw new Error("コードが壊れています。もう一度コピーしてください");
  }
  if (!isObject(parsed) || parsed.type !== expected || typeof parsed.sdp !== "string" || !parsed.sdp) {
    throw new Error(expected === "offer" ? "招待コードではありません" : "応答コードではありません");
  }
  return { type: expected, sdp: parsed.sdp };
}
