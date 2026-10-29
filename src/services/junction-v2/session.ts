// Junction v2 in the desktop app: host and guest sessions.
//
// Topology: star. Each guest has one RTCPeerConnection to the host (one Opus
// sendrecv track + the "jlink" data channel). The host relays to each guest
// the J feed chosen by the turn state (core/turns.ts) and assembles the program
// from the on-air uplink (core/program.ts), which the engine plays on the
// program device (OBS / interface / BlackHole) at a fixed delay.
//
// Audio inside this WebView (one AudioContext at 48 kHz):
//   engine uplink --ws--> jlink-source (elastic) --+--> capture slot 0 (host)
//                                                   +--> uplink track (guest)
//   remote track -> MediaStreamSource --+--> capture slot k
//                                        +--> per-guest feed gains (host)
//   capture batches --> program assembler --ws--> engine program device (host)
//                   --> own J feed slot   --ws--> engine JUNCTION deck
import workletUrl from "./worklet.ts?worker&url";
import { ProgramAssembler, type SplicePlan } from "./core/program.ts";
import { LinkPeer, type LinkStats } from "./core/peer.ts";
import {
  PROTOCOL, decodeCode, encodeCode, parseGuestMessage, parseHostMessage,
  type GuestReport, type GuestRequest, type HostMessage,
} from "./core/protocol.ts";
import {
  ReleaseWatch, TurnError, acceptBroadcast, addParticipant, advance, emptySession, releasePrevious, routeFor, routes,
  removeParticipant, setConnected, setNextPick, setOrder, stopSet, type ParticipantKind, type SessionState,
} from "./core/turns.ts";
import { EngineLink, type EngineLinkState } from "./engine-link";
import { shareMusicsApi } from "@/services/junction/share-musics/client";

const SAMPLE_RATE = 48000;
const SLOTS = 8;
const BATCH = 1024;
export const PROGRAM_DELAY_SECONDS = 3;

export type LinkPhase = "idle" | "starting" | "hosting" | "joining" | "guest";

export interface MemberInfo {
  connection: RTCPeerConnectionState | "invited";
  rttMs: number | null;
  jitterMs: number | null;
  /** Manual invite code waiting for its answer. */
  inviteCode?: string;
  via: "share" | "code";
}

export interface PendingMember { memberId: string; name: string; client: string }

export interface LinkSnapshot {
  phase: LinkPhase;
  role: "host" | "guest" | null;
  me: string | null;
  sessionName: string;
  session: SessionState | null;
  members: Record<string, MemberInfo>;
  pending: PendingMember[];
  /** Guest, manual exchange: the answer code to send back to the host. */
  answerCode: string | null;
  lastSplice: (SplicePlan & { atMs: number }) | null;
  programDelayMs: number | null;
  engine: EngineLinkState | null;
  error: string | null;
  shareSessionId: string | null;
}

const initial: LinkSnapshot = {
  phase: "idle", role: null, me: null, sessionName: "", session: null, members: {}, pending: [],
  answerCode: null, lastSplice: null, programDelayMs: null, engine: null, error: null, shareSessionId: null,
};

let snapshot: LinkSnapshot = initial;
const listeners = new Set<() => void>();
function patch(next: Partial<LinkSnapshot>) {
  snapshot = { ...snapshot, ...next };
  for (const listener of listeners) listener();
}
export const linkStore = {
  get: () => snapshot,
  subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
};

const message = (cause: unknown) => (cause instanceof Error ? cause.message : String(cause));
const randomId = (prefix: string) => `${prefix}${Array.from(crypto.getRandomValues(new Uint8Array(6)), (b) => b.toString(16).padStart(2, "0")).join("")}`;

/** Splits a capture batch into per-slot stereo blocks. */
function slotBlock(data: Float32Array, slot: number, frames: number) {
  const left = new Float32Array(frames), right = new Float32Array(frames);
  const base = slot * 2 * BATCH;
  for (let i = 0; i < frames; i += 1) { left[i] = data[base + 2 * i]; right[i] = data[base + 2 * i + 1]; }
  return { left, right };
}

class AudioRig {
  readonly ctx: AudioContext;
  readonly capture: AudioWorkletNode;
  readonly uplink: AudioWorkletNode;
  sourceFillMs = 0;

  private constructor(ctx: AudioContext, slots: number) {
    this.ctx = ctx;
    this.capture = new AudioWorkletNode(ctx, "jlink-capture", {
      numberOfInputs: slots, numberOfOutputs: 1, outputChannelCount: [2],
      processorOptions: { slots, batch: BATCH },
    });
    // The capture node must be pulled to run; its output is silent.
    this.capture.connect(ctx.destination);
    this.uplink = new AudioWorkletNode(ctx, "jlink-source", { numberOfInputs: 0, numberOfOutputs: 1, outputChannelCount: [2] });
    this.uplink.port.onmessage = (event: MessageEvent<{ fill: number }>) => { this.sourceFillMs = (event.data.fill / SAMPLE_RATE) * 1000; };
  }

  static async create(slots: number): Promise<AudioRig> {
    const ctx = new AudioContext({ sampleRate: SAMPLE_RATE, latencyHint: "playback" });
    await ctx.audioWorklet.addModule(workletUrl);
    if (ctx.state !== "running") await ctx.resume().catch(() => undefined);
    return new AudioRig(ctx, slots);
  }

  pushUplink(left: Float32Array, right: Float32Array) {
    this.uplink.port.postMessage({ left, right }, [left.buffer, right.buffer]);
  }

  close() { void this.ctx.close().catch(() => undefined); }
}

// ---------------------------------------------------------------------------
// Host

interface GuestLink {
  id: string;
  peer: LinkPeer;
  slot: number;
  dest: MediaStreamAudioDestinationNode;
  source: MediaStreamAudioSourceNode | null;
  feeds: Map<string, GainNode>;
  feedFrom: string | null;
  report: GuestReport | null;
  stats: LinkStats | null;
  via: "share" | "code";
  inviteCode?: string;
  shareMemberId?: string;
  named: boolean;
}

class HostSession {
  readonly hostId = "host";
  state: SessionState;
  private readonly rig: AudioRig;
  private readonly engine = new EngineLink();
  private readonly program = new ProgramAssembler({ delaySeconds: PROGRAM_DELAY_SECONDS });
  private readonly guests = new Map<string, GuestLink>();
  private readonly slots: (string | null)[] = Array(SLOTS).fill(null);
  private readonly releaseWatch = new ReleaseWatch();
  private timers: number[] = [];
  private share: { sessionId: string; afterSignalId: number; offered: Set<string> } | null = null;
  private lastSplice: (SplicePlan & { atMs: number }) | null = null;
  private stopped = false;

  constructor(rig: AudioRig, name: string, hostPlays: boolean) {
    this.rig = rig;
    this.state = emptySession({ id: this.hostId, name, kind: "host", connected: true }, hostPlays);
    this.slots[0] = this.hostId;
    rig.uplink.connect(rig.capture, 0, 0);
    rig.capture.port.onmessage = (event: MessageEvent<{ frame: number; frames: number; data: Float32Array }>) => this.onCapture(event.data);
    this.engine.onUplink = (_frame, left, right) => rig.pushUplink(left, right);
  }

  async start() {
    await this.engine.start();
    await this.engine.setUplink(routeFor(this.state, this.hostId).uplink);
    this.timers.push(window.setInterval(() => void this.poll(), 1000));
    this.publish();
  }

  // ---- audio ----
  private onCapture({ frame, frames, data }: { frame: number; frames: number; data: Float32Array }) {
    const blocks: Record<string, { left: Float32Array; right: Float32Array }> = {};
    this.slots.forEach((id, slot) => { if (id) blocks[id] = slotBlock(data, slot, frames); });
    this.program.ingest(frame, frames, blocks);
    const out = this.program.render(frames);
    this.engine.sendProgram(frame, out.left, out.right);
    const feed = routeFor(this.state, this.hostId).feedFrom;
    const block = feed ? blocks[feed] : undefined;
    if (block) this.engine.sendInput(frame, block.left, block.right);
  }

  private sourceNode(id: string): AudioNode | null {
    if (id === this.hostId) return this.rig.uplink;
    return this.guests.get(id)?.source ?? null;
  }

  /** Routes `from`'s audio into guest `g`'s outgoing track with a short ramp. */
  private setFeed(g: GuestLink, from: string | null) {
    if (g.feedFrom === from) return;
    g.feedFrom = from;
    const t = this.rig.ctx.currentTime;
    for (const [source, gain] of g.feeds) {
      if (source !== from) gain.gain.setTargetAtTime(0, t, 0.01);
    }
    if (!from) return;
    let gain = g.feeds.get(from);
    if (!gain) {
      gain = this.rig.ctx.createGain();
      gain.gain.value = 0;
      gain.connect(g.dest);
      g.feeds.set(from, gain);
      this.sourceNode(from)?.connect(gain);
    }
    gain.gain.setTargetAtTime(1, t, 0.01);
  }

  /** A guest's remote stream (re)appeared: wire it into capture and feeds. */
  private attachSource(g: GuestLink, stream: MediaStream) {
    g.source?.disconnect();
    g.source = this.rig.ctx.createMediaStreamSource(stream);
    g.source.connect(this.rig.capture, 0, g.slot);
    for (const other of this.guests.values()) other.feeds.get(g.id) && g.source.connect(other.feeds.get(g.id)!);
  }

  // ---- state ----
  private change(next: SessionState) {
    const prev = this.state;
    if (next === prev) return;
    this.state = next;
    const r = routes(next);
    void this.engine.setUplink(r[this.hostId]?.uplink ?? "local").catch(() => undefined);
    for (const g of this.guests.values()) this.setFeed(g, r[g.id]?.feedFrom ?? null);
    if (prev.onAir !== next.onAir) {
      if (!next.onAir) this.program.setSource(null);
      else {
        const plan = this.program.handoff(next.onAir, this.priorFrames(next.onAir));
        this.lastSplice = { ...plan, atMs: Date.now() };
      }
    }
    this.broadcast({ t: "state", state: next });
    this.publish();
  }

  private act(fn: (s: SessionState) => SessionState) {
    try { this.change(fn(this.state)); patch({ error: null }); }
    catch (cause) { patch({ error: message(cause) }); }
  }

  /** Timestamp prior of the relay loop host -> id -> host, in frames. */
  private priorFrames(id: string): number | null {
    const ms = (v: number) => Math.round((v / 1000) * SAMPLE_RATE);
    if (id === this.hostId) {
      const input = this.engine.lastState?.input.latencyMs ?? 120;
      return ms(input + this.rig.sourceFillMs + (BATCH / SAMPLE_RATE) * 1000 + 20);
    }
    const g = this.guests.get(id);
    if (!g?.stats?.rttMs || !g.report) return null;
    const parts = [g.stats.rttMs, g.stats.jitterBufferMs ?? 0, g.report.inboundJitterMs ?? 0, g.report.loopMs ?? 0];
    return ms(parts.reduce((a, b) => a + b, 0));
  }

  private broadcast(msg: HostMessage) { for (const g of this.guests.values()) g.peer.send(msg); }

  next() { this.act((s) => advance(s, this.hostId, this.hostId)); }
  release() { this.act((s) => releasePrevious(s, this.hostId, this.hostId)); }
  stopSet() { this.act(stopSet); }
  setOrder(order: string[]) { this.act((s) => setOrder(s, order)); }
  setNext(id: string | null) { this.act((s) => setNextPick(s, id)); }
  setHostPlays(plays: boolean) {
    this.act((s) => setOrder(s, plays ? (s.order.includes(this.hostId) ? s.order : [this.hostId, ...s.order]) : s.order.filter((id) => id !== this.hostId)));
  }

  // ---- guests ----
  private freeSlot(): number {
    const slot = this.slots.findIndex((id, index) => index > 0 && id === null);
    if (slot < 0) throw new Error(`同時に接続できるのは${SLOTS - 1}人までです`);
    return slot;
  }

  private createGuest(id: string, via: "share" | "code", shareMemberId?: string): GuestLink {
    const slot = this.freeSlot();
    const dest = this.rig.ctx.createMediaStreamDestination();
    const g: GuestLink = {
      id, slot, dest, source: null, feeds: new Map(), feedFrom: null, report: null, stats: null, via, shareMemberId, named: false,
      peer: new LinkPeer({
        initiator: true,
        sendTrack: dest.stream.getAudioTracks()[0] ?? null,
        onTrack: (stream) => this.attachSource(g, stream),
        onMessage: (raw) => this.onGuestMessage(g, raw),
        onState: (state) => {
          if (state === "connected") this.act((s) => setConnected(s, id, true));
          if (state === "failed" || state === "disconnected" || state === "closed") this.act((s) => setConnected(s, id, false));
          this.publish();
        },
      }),
    };
    this.slots[slot] = id;
    this.guests.set(id, g);
    // Placeholder until the guest says hello; not in the order yet.
    this.change(addParticipant(this.state, { id, name: "接続中のDJ", kind: "app", connected: false }, false));
    return g;
  }

  private onGuestMessage(g: GuestLink, raw: string) {
    const msg = parseGuestMessage(raw);
    if (!msg) return;
    switch (msg.t) {
      case "hello": {
        if (msg.protocol !== PROTOCOL) { g.peer.send({ t: "error", message: "plumdeckまたはPlumDeck Liteを最新版にしてください" }); return; }
        const kind: ParticipantKind = msg.kind;
        this.change(addParticipant(this.state, { id: g.id, name: msg.name || "DJ", kind, connected: true }, !g.named));
        g.named = true;
        g.peer.send({ t: "welcome", protocol: PROTOCOL, you: g.id, state: this.state });
        this.setFeed(g, routeFor(this.state, g.id).feedFrom);
        return;
      }
      case "request": {
        const r: GuestRequest = msg.request;
        try {
          if (r.action === "next") this.change(advance(this.state, g.id, this.hostId));
          else if (r.action === "releasePrevious") this.change(releasePrevious(this.state, g.id, this.hostId));
          else if (r.action === "setNext") {
            if (g.id !== this.state.onAir) throw new TurnError("次のDJを選べるのはON AIR中のDJかホストです");
            this.change(setNextPick(this.state, r.target));
          }
        } catch (cause) { g.peer.send({ t: "error", message: message(cause) }); }
        return;
      }
      case "ping": g.peer.send({ t: "pong", id: msg.id, sentAt: msg.sentAt }); return;
      case "report": {
        g.report = msg.report;
        if (g.id === this.state.onAir && this.releaseWatch.observe(this.state, msg.report.jAudible, Date.now())) {
          this.change(releasePrevious(this.state, this.hostId, this.hostId));
        }
        return;
      }
    }
  }

  removeGuest(id: string) {
    const g = this.guests.get(id);
    if (!g) return;
    g.peer.close();
    g.source?.disconnect();
    for (const gain of g.feeds.values()) gain.disconnect();
    for (const other of this.guests.values()) { other.feeds.get(id)?.disconnect(); other.feeds.delete(id); if (other.feedFrom === id) other.feedFrom = null; }
    this.slots[g.slot] = null;
    this.guests.delete(id);
    this.change(removeParticipant(this.state, id));
    for (const other of this.guests.values()) this.setFeed(other, routeFor(this.state, other.id).feedFrom);
  }

  async inviteByCode(): Promise<string> {
    const g = this.createGuest(randomId("c-"), "code");
    const offer = await g.peer.createOffer();
    g.inviteCode = encodeCode(offer);
    this.publish();
    return g.inviteCode;
  }

  async acceptAnswerCode(id: string, code: string) {
    const g = this.guests.get(id);
    if (!g) throw new Error("この招待は取り消されています");
    await g.peer.acceptAnswer(decodeCode(code, "answer"));
    g.inviteCode = undefined;
    this.publish();
  }

  // ---- share-musics signaling ----
  async publishOnShare(name: string, hostName: string) {
    const created = await shareMusicsApi.createSession(name, hostName);
    this.share = { sessionId: created.sessionId, afterSignalId: 0, offered: new Set() };
    patch({ shareSessionId: created.sessionId });
  }

  async approve(memberId: string, approved: boolean) {
    if (!this.share) return;
    await shareMusicsApi.approve(this.share.sessionId, memberId, approved);
    await this.pollShare();
  }

  private async pollShare() {
    const share = this.share;
    if (!share || this.stopped) return;
    const view = await shareMusicsApi.getSession(share.sessionId, share.afterSignalId);
    const pending: PendingMember[] = [];
    for (const m of view.session.members) {
      if (m.role !== "guest") continue;
      if (m.status === "pending") pending.push({ memberId: m.peerId, name: m.displayName, client: m.client });
      if (m.status === "approved" && !share.offered.has(m.peerId)) {
        share.offered.add(m.peerId);
        const g = this.createGuest(m.peerId, "share", m.peerId);
        const offer = await g.peer.createOffer();
        await shareMusicsApi.signal(share.sessionId, m.peerId, "offer", offer);
      }
      if (m.status === "left" && this.guests.has(m.peerId)) this.removeGuest(m.peerId);
    }
    for (const s of view.signals) {
      share.afterSignalId = Math.max(share.afterSignalId, s.id);
      if (s.kind === "answer" && s.payload.sdp) await this.guests.get(s.senderPeerId)?.peer.acceptAnswer({ type: "answer", sdp: s.payload.sdp });
    }
    patch({ pending });
  }

  private async poll() {
    if (this.stopped) return;
    if (this.share) await this.pollShare().catch((cause) => patch({ error: `PlumDeck Liteとの通信: ${message(cause)}` }));
    for (const g of this.guests.values()) g.stats = await g.peer.stats().catch(() => g.stats);
    await this.engine.state().catch(() => undefined);
    // The host as on-air DJ: release the previous DJ when its own J deck is faded out.
    if (this.state.onAir === this.hostId && this.engine.lastState?.deck.available) {
      if (this.releaseWatch.observe(this.state, this.engine.lastState.deck.audible !== false, Date.now())) {
        this.change(releasePrevious(this.state, this.hostId, this.hostId));
      }
    }
    this.publish();
  }

  openProgram(deviceId: string) { return this.engine.openProgram(deviceId).then(() => this.publish()); }
  closeProgram() { return this.engine.closeProgram().then(() => this.publish()); }
  setInput(settings: Parameters<EngineLink["setInput"]>[0]) { return this.engine.setInput(settings).then(() => this.publish()); }

  publish() {
    const members: Record<string, MemberInfo> = {};
    for (const g of this.guests.values()) {
      members[g.id] = {
        connection: g.inviteCode ? "invited" : g.peer.state, rttMs: g.stats?.rttMs ?? null,
        jitterMs: g.stats?.jitterBufferMs ?? null, inviteCode: g.inviteCode, via: g.via,
      };
    }
    patch({
      phase: "hosting", role: "host", me: this.hostId, session: this.state, members, lastSplice: this.lastSplice,
      programDelayMs: this.program.source ? (this.program.delay / SAMPLE_RATE) * 1000 : null,
      engine: this.engine.lastState,
    });
  }

  async stop() {
    this.stopped = true;
    for (const t of this.timers) clearInterval(t);
    this.broadcast({ t: "error", message: "ホストがセッションを終了しました" });
    for (const id of [...this.guests.keys()]) this.guests.get(id)?.peer.close();
    if (this.share) await shareMusicsApi.leave(this.share.sessionId).catch(() => undefined);
    await this.engine.stop();
    this.rig.close();
  }
}

// ---------------------------------------------------------------------------
// Guest

class GuestSession {
  state: SessionState | null = null;
  me: string | null = null;
  private readonly rig: AudioRig;
  private readonly engine = new EngineLink();
  readonly peer: LinkPeer;
  private readonly dest: MediaStreamAudioDestinationNode;
  private source: MediaStreamAudioSourceNode | null = null;
  private timers: number[] = [];
  private pingId = 0;
  private stopped = false;
  private readonly name: string;
  private share: { sessionId: string; afterSignalId: number; answered: boolean } | null = null;

  constructor(rig: AudioRig, name: string) {
    this.rig = rig;
    this.name = name;
    this.dest = rig.ctx.createMediaStreamDestination();
    rig.uplink.connect(this.dest);
    this.engine.onUplink = (_frame, left, right) => rig.pushUplink(left, right);
    rig.capture.port.onmessage = (event: MessageEvent<{ frame: number; frames: number; data: Float32Array }>) => {
      const { frame, frames, data } = event.data;
      if (!this.source) return;
      const block = slotBlock(data, 0, frames);
      this.engine.sendInput(frame, block.left, block.right);
    };
    this.peer = new LinkPeer({
      initiator: false,
      sendTrack: this.dest.stream.getAudioTracks()[0] ?? null,
      onTrack: (stream) => {
        this.source?.disconnect();
        this.source = rig.ctx.createMediaStreamSource(stream);
        this.source.connect(rig.capture, 0, 0);
      },
      onMessage: (raw) => this.onHostMessage(raw),
      onOpen: () => this.peer.send({ t: "hello", protocol: PROTOCOL, name: this.name, kind: "app" }),
      onState: (state) => {
        if (state === "failed") patch({ error: "ホストとの接続が切れました" });
        this.publish();
      },
    });
  }

  async start() {
    await this.engine.start();
    await this.engine.setUplink("local");
    this.timers.push(window.setInterval(() => void this.tick(), 1000));
  }

  async acceptInviteCode(code: string): Promise<string> {
    const answer = await this.peer.acceptOffer(decodeCode(code, "offer"));
    const answerCode = encodeCode(answer);
    patch({ answerCode });
    return answerCode;
  }

  async joinShare(sessionId: string) {
    await shareMusicsApi.joinSession(sessionId, this.name);
    this.share = { sessionId, afterSignalId: 0, answered: false };
    patch({ shareSessionId: sessionId });
  }

  private async pollShare() {
    const share = this.share;
    if (!share || share.answered || this.stopped) return;
    const view = await shareMusicsApi.getSession(share.sessionId, share.afterSignalId);
    if (view.me.status === "rejected") { patch({ error: "ホストに参加を断られました" }); share.answered = true; return; }
    for (const s of view.signals) {
      share.afterSignalId = Math.max(share.afterSignalId, s.id);
      if (s.kind === "offer" && s.payload.sdp && !share.answered) {
        const answer = await this.peer.acceptOffer({ type: "offer", sdp: s.payload.sdp });
        await shareMusicsApi.signal(share.sessionId, s.senderPeerId, "answer", answer);
        share.answered = true;
      }
    }
  }

  private onHostMessage(raw: string) {
    const msg = parseHostMessage(raw);
    if (!msg) return;
    if (msg.t === "welcome") { this.me = msg.you; this.applyState(msg.state); patch({ answerCode: null }); }
    else if (msg.t === "state") this.applyState(msg.state);
    else if (msg.t === "error") patch({ error: msg.message });
  }

  private applyState(incoming: SessionState) {
    const next = acceptBroadcast(this.state, incoming);
    if (next === this.state) return;
    this.state = next;
    if (this.me) void this.engine.setUplink(routeFor(next, this.me).uplink).catch(() => undefined);
    this.publish();
  }

  request(request: GuestRequest) {
    if (!this.peer.send({ t: "request", request })) patch({ error: "ホストに届きませんでした。接続を確認してください" });
  }

  setInput(settings: Parameters<EngineLink["setInput"]>[0]) { return this.engine.setInput(settings).then(() => this.publish()); }

  private async tick() {
    if (this.stopped) return;
    if (this.share) await this.pollShare().catch((cause) => patch({ error: `PlumDeck Liteとの通信: ${message(cause)}` }));
    this.peer.send({ t: "ping", id: ++this.pingId, sentAt: Date.now() });
    const stats = await this.peer.stats().catch(() => null);
    const engine = await this.engine.state().catch(() => null);
    const loopMs = engine ? engine.input.latencyMs + this.rig.sourceFillMs + (BATCH / SAMPLE_RATE) * 1000 + 20 : null;
    this.peer.send({ t: "report", report: { inboundJitterMs: stats?.jitterBufferMs ?? null, loopMs, jAudible: engine?.deck.audible !== false } });
    this.publish(stats);
  }

  publish(stats?: LinkStats | null) {
    const members: Record<string, MemberInfo> = {};
    if (stats !== undefined) members.host = { connection: this.peer.state, rttMs: stats?.rttMs ?? null, jitterMs: stats?.jitterBufferMs ?? null, via: this.share ? "share" : "code" };
    patch({ phase: this.me ? "guest" : "joining", role: "guest", me: this.me, session: this.state, engine: this.engine.lastState, ...(stats !== undefined ? { members } : {}) });
  }

  async stop() {
    this.stopped = true;
    for (const t of this.timers) clearInterval(t);
    this.peer.close();
    if (this.share) await shareMusicsApi.leave(this.share.sessionId).catch(() => undefined);
    await this.engine.stop();
    this.rig.close();
  }
}

// ---------------------------------------------------------------------------
// Public API used by the UI

let host: HostSession | null = null;
let guest: GuestSession | null = null;

async function guarded<T>(fn: () => Promise<T>): Promise<T> {
  try {
    const result = await fn();
    patch({ error: null });
    return result;
  } catch (cause) {
    patch({ error: message(cause) });
    throw cause;
  }
}

export const junctionLink = {
  active: () => host !== null || guest !== null,

  startHost: (options: { sessionName: string; djName: string; hostPlays: boolean; share: boolean }) => guarded(async () => {
    if (host || guest) throw new Error("すでにJunctionに参加しています");
    patch({ ...initial, phase: "starting", role: "host", sessionName: options.sessionName });
    try {
      const rig = await AudioRig.create(SLOTS);
      host = new HostSession(rig, options.djName, options.hostPlays);
      await host.start();
      if (options.share) await host.publishOnShare(options.sessionName, options.djName);
    } catch (cause) {
      await host?.stop().catch(() => undefined);
      host = null;
      patch({ ...initial, error: message(cause) });
      throw cause;
    }
  }),

  startGuest: (options: { djName: string; inviteCode?: string; shareSessionId?: string; sessionName?: string }) => guarded(async () => {
    if (host || guest) throw new Error("すでにJunctionに参加しています");
    patch({ ...initial, phase: "starting", role: "guest", sessionName: options.sessionName ?? "" });
    try {
      const rig = await AudioRig.create(1);
      guest = new GuestSession(rig, options.djName);
      await guest.start();
      patch({ phase: "joining" });
      if (options.inviteCode) await guest.acceptInviteCode(options.inviteCode);
      else if (options.shareSessionId) await guest.joinShare(options.shareSessionId);
    } catch (cause) {
      await guest?.stop().catch(() => undefined);
      guest = null;
      patch({ ...initial, error: message(cause) });
      throw cause;
    }
  }),

  leave: () => guarded(async () => {
    const h = host, g = guest;
    host = null; guest = null;
    await h?.stop();
    await g?.stop();
    patch({ ...initial });
  }),

  /** NEXT: host acts directly, guests ask the host. */
  next: () => { if (host) host.next(); else guest?.request({ action: "next" }); },
  releasePrevious: () => { if (host) host.release(); else guest?.request({ action: "releasePrevious" }); },
  setNext: (id: string | null) => { if (host) host.setNext(id); else guest?.request({ action: "setNext", target: id }); },

  // Host only
  stopSet: () => host?.stopSet(),
  setOrder: (order: string[]) => host?.setOrder(order),
  setHostPlays: (plays: boolean) => host?.setHostPlays(plays),
  inviteByCode: () => guarded(async () => { if (!host) throw new Error("ホストではありません"); return host.inviteByCode(); }),
  acceptAnswerCode: (id: string, code: string) => guarded(async () => { if (!host) throw new Error("ホストではありません"); await host.acceptAnswerCode(id, code); }),
  cancelInvite: (id: string) => host?.removeGuest(id),
  kick: (id: string) => host?.removeGuest(id),
  approve: (memberId: string, approved: boolean) => guarded(async () => { await host?.approve(memberId, approved); }),
  openProgram: (deviceId: string) => guarded(async () => { await host?.openProgram(deviceId); }),
  closeProgram: () => guarded(async () => { await host?.closeProgram(); }),

  // Both: this machine's JUNCTION deck
  setInput: (settings: Parameters<EngineLink["setInput"]>[0]) => guarded(async () => { await (host ?? guest)?.setInput(settings); }),
};
