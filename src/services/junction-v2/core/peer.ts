// One host<->guest WebRTC link: a single sendrecv Opus audio transceiver plus
// the "jlink" control data channel. Shared verbatim with PlumDeck Lite.
// The host always creates the offer; ICE candidates are gathered completely
// before the description is handed out (no trickle), so one offer/answer pair
// is the whole signaling exchange, whether it travels through share-musics or
// as a pasted code.
import { CONTROL_CHANNEL } from "./protocol.ts";

export const ICE_SERVERS: RTCIceServer[] = [
  { urls: ["stun:stun.l.google.com:19302", "stun:stun.cloudflare.com:3478"] },
];

/**
 * Music, not speech: stereo, high bitrate, no DTX/FEC-driven bandwidth drops.
 * Applied to every Opus payload in a description.
 */
export function tuneOpus(sdp: string): string {
  const payloads = [...sdp.matchAll(/^a=rtpmap:(\d+) opus\/48000\/2\s*$/gim)].map((m) => m[1]);
  let out = sdp;
  for (const pt of payloads) {
    const params = "stereo=1;sprop-stereo=1;maxaveragebitrate=192000;maxplaybackrate=48000;useinbandfec=1;usedtx=0;cbr=0";
    const fmtp = new RegExp(`^a=fmtp:${pt} ([^\\r\\n]*)$`, "im");
    const found = out.match(fmtp);
    if (found) {
      const keep = found[1].split(";").map((v) => v.trim())
        .filter((v) => v && !/^(stereo|sprop-stereo|maxaveragebitrate|maxplaybackrate|useinbandfec|usedtx|cbr)=/i.test(v));
      out = out.replace(fmtp, `a=fmtp:${pt} ${[...keep, params].join(";")}`);
    } else {
      out = out.replace(new RegExp(`^(a=rtpmap:${pt} opus/48000/2\\s*)$`, "im"), `$1\r\na=fmtp:${pt} ${params}`);
    }
  }
  return out;
}

function gathered(pc: RTCPeerConnection, timeoutMs = 8000): Promise<void> {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    // Gathering can stall on a dead STUN server; whatever is gathered by the
    // timeout is usually enough (host + one srflx candidate).
    const timer = setTimeout(done, timeoutMs);
    function done() {
      clearTimeout(timer);
      pc.removeEventListener("icegatheringstatechange", check);
      resolve();
    }
    function check() { if (pc.iceGatheringState === "complete") done(); }
    pc.addEventListener("icegatheringstatechange", check);
  });
}

export interface LinkPeerOptions {
  initiator: boolean;
  /** Track this side sends (the uplink, or the J feed on the host). */
  sendTrack: MediaStreamTrack | null;
  onTrack?: (stream: MediaStream) => void;
  onMessage?: (raw: string) => void;
  onState?: (state: RTCPeerConnectionState) => void;
  onOpen?: () => void;
}

export interface LinkStats {
  rttMs: number | null;
  /** Average jitter buffer delay of the inbound audio, ms. */
  jitterBufferMs: number | null;
  packetsLost: number;
  bytesReceived: number;
  bytesSent: number;
}

export class LinkPeer {
  readonly pc: RTCPeerConnection;
  private channel: RTCDataChannel | null = null;
  private sender: RTCRtpSender | null = null;
  private readonly options: LinkPeerOptions;
  private lastJitter: { delay: number; count: number } | null = null;

  constructor(options: LinkPeerOptions) {
    this.options = options;
    this.pc = new RTCPeerConnection({ iceServers: ICE_SERVERS, bundlePolicy: "max-bundle" });
    if (options.initiator) {
      const transceiver = this.pc.addTransceiver("audio", { direction: "sendrecv" });
      this.sender = transceiver.sender;
      if (options.sendTrack) void transceiver.sender.replaceTrack(options.sendTrack);
      this.attach(this.pc.createDataChannel(CONTROL_CHANNEL, { ordered: true }));
    }
    this.pc.ontrack = (event) => {
      const stream = event.streams[0] ?? new MediaStream([event.track]);
      // A small, stable jitter buffer target: the host's program delay absorbs
      // latency, so trading a little delay for fewer time-stretch events is right.
      try { (event.receiver as RTCRtpReceiver & { jitterBufferTarget?: number }).jitterBufferTarget = 120; } catch { /* unsupported */ }
      options.onTrack?.(stream);
      event.track.onunmute = () => options.onTrack?.(stream);
    };
    this.pc.ondatachannel = (event) => this.attach(event.channel);
    this.pc.onconnectionstatechange = () => options.onState?.(this.pc.connectionState);
  }

  private attach(channel: RTCDataChannel) {
    if (channel.label !== CONTROL_CHANNEL) return;
    this.channel = channel;
    channel.onmessage = (event) => this.options.onMessage?.(String(event.data));
    channel.onopen = () => this.options.onOpen?.();
    if (channel.readyState === "open") queueMicrotask(() => this.options.onOpen?.());
  }

  get open(): boolean { return this.channel?.readyState === "open"; }
  get state(): RTCPeerConnectionState { return this.pc.connectionState; }

  async createOffer(): Promise<{ type: "offer"; sdp: string }> {
    const offer = await this.pc.createOffer();
    await this.pc.setLocalDescription({ type: "offer", sdp: tuneOpus(offer.sdp ?? "") });
    await gathered(this.pc);
    return { type: "offer", sdp: this.pc.localDescription?.sdp ?? "" };
  }

  async acceptOffer(offer: { type: "offer"; sdp: string }): Promise<{ type: "answer"; sdp: string }> {
    await this.pc.setRemoteDescription(offer);
    const transceiver = this.pc.getTransceivers().find((t) => t.receiver.track.kind === "audio");
    if (transceiver) {
      transceiver.direction = "sendrecv";
      this.sender = transceiver.sender;
      if (this.options.sendTrack) await transceiver.sender.replaceTrack(this.options.sendTrack);
    }
    const answer = await this.pc.createAnswer();
    await this.pc.setLocalDescription({ type: "answer", sdp: tuneOpus(answer.sdp ?? "") });
    await gathered(this.pc);
    return { type: "answer", sdp: this.pc.localDescription?.sdp ?? "" };
  }

  async acceptAnswer(answer: { type: "answer"; sdp: string }): Promise<void> {
    if (this.pc.signalingState === "have-local-offer") await this.pc.setRemoteDescription(answer);
  }

  async setSendTrack(track: MediaStreamTrack | null): Promise<void> {
    if (this.sender && this.sender.track !== track) await this.sender.replaceTrack(track);
  }

  send(message: unknown): boolean {
    if (this.channel?.readyState !== "open" || this.channel.bufferedAmount > 262_144) return false;
    this.channel.send(JSON.stringify(message));
    return true;
  }

  async stats(): Promise<LinkStats> {
    const report = await this.pc.getStats();
    const out: LinkStats = { rttMs: null, jitterBufferMs: null, packetsLost: 0, bytesReceived: 0, bytesSent: 0 };
    report.forEach((s) => {
      if (s.type === "candidate-pair" && s.nominated && s.state === "succeeded" && typeof s.currentRoundTripTime === "number") out.rttMs = s.currentRoundTripTime * 1000;
      if (s.kind !== "audio") return;
      if (s.type === "inbound-rtp") {
        out.packetsLost += Number(s.packetsLost ?? 0);
        out.bytesReceived += Number(s.bytesReceived ?? 0);
        const delay = Number(s.jitterBufferDelay ?? 0), count = Number(s.jitterBufferEmittedCount ?? 0);
        if (count > 0) {
          const prev = this.lastJitter;
          // Recent average when we have a previous sample, lifetime otherwise.
          out.jitterBufferMs = prev && count > prev.count ? ((delay - prev.delay) / (count - prev.count)) * 1000 : (delay / count) * 1000;
          this.lastJitter = { delay, count };
        }
      }
      if (s.type === "outbound-rtp") out.bytesSent += Number(s.bytesSent ?? 0);
    });
    return out;
  }

  close(): void {
    try { this.channel?.close(); } catch { /* closed */ }
    this.pc.close();
  }
}
