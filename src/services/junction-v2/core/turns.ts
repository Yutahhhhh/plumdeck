// Junction v2 session and turn state.
//
// The host owns this state and broadcasts it whole after every change; guests
// only render it and send requests. Pure functions, no I/O, so the rules can be
// tested without WebRTC or an audio engine. Shared verbatim with PlumDeck Lite.
//
// Model (owner decision, 2026-10-29): one continuous set, a turn order, "who is
// on air" and a manual NEXT. No STANDBY/READY/fader-start machinery.
//
//   onAir     the DJ whose uplink is the program (what goes out to the stream)
//   next      who takes over on NEXT: an explicit pick, else the next connected
//             DJ after onAir in the order
//   previous  the DJ that onAir took over from. Their tracks keep playing on
//             onAir's J deck until onAir fades them out; operator handoff and
//             the end of the outgoing audio are separate steps, as in a real B2B.

export type ParticipantKind = "host" | "app" | "lite";

export interface Participant {
  id: string;
  name: string;
  kind: ParticipantKind;
  connected: boolean;
}

export interface SessionState {
  /** Bumped on every change so guests can drop stale broadcasts. */
  revision: number;
  participants: Participant[];
  /** Turn order. Only these participants can go on air. */
  order: string[];
  onAir: string | null;
  previous: string | null;
  /** Explicit next pick; null means "the following DJ in the order". */
  nextPick: string | null;
  /** Number of handoffs so far (also identifies the current program segment). */
  handoffs: number;
}

export type Role = "onAir" | "next" | "previous" | "idle";

/**
 * What a participant sends and hears.
 *
 *  uplink  "full"  = the participant's whole mix including its J deck.
 *          "local" = only its own decks (never the J deck), so nobody can hear
 *                    themselves come back through someone else's J deck.
 *  feedFrom        whose uplink the host relays into this participant's J deck.
 *
 * onAir and next send "full": onAir's mix is the program (and carries the
 * previous DJ's tail through its J deck); next's mix must already contain the
 * on-air audio so the host can splice onto it seamlessly. Everybody else sends
 * "local". J feeds: onAir hears the previous DJ (the tail it is mixing out),
 * previous hears nothing (onAir's mix carries previous's own tracks, so that
 * feed would be an echo of themselves), everybody else hears onAir. No feed
 * ever contains the listener's own audio:
 *   onAir.full  = onAir + previous.local  -> heard by idle DJs and next
 *   next.full   = next + onAir.full       -> heard by nobody (program only)
 *   previous.local                        -> heard by onAir only
 * Once onAir has faded previous out (or anyone releases it), previous becomes
 * idle and hears onAir again.
 */
export interface Route {
  uplink: "full" | "local";
  feedFrom: string | null;
}

export class TurnError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "TurnError";
  }
}

export function emptySession(host: Participant, hostPlays = true): SessionState {
  return {
    revision: 1,
    participants: [host],
    order: hostPlays ? [host.id] : [],
    onAir: null,
    previous: null,
    nextPick: null,
    handoffs: 0,
  };
}

const bump = (state: SessionState, patch: Partial<SessionState>): SessionState => ({
  ...state,
  ...patch,
  revision: state.revision + 1,
});

export function participant(state: SessionState, id: string | null): Participant | null {
  return id ? state.participants.find((p) => p.id === id) ?? null : null;
}

const playable = (state: SessionState, id: string) =>
  state.order.includes(id) && participant(state, id)?.connected === true;

/** Who goes on air on the next NEXT. */
export function nextPerformer(state: SessionState): string | null {
  if (state.nextPick && state.nextPick !== state.onAir && playable(state, state.nextPick)) return state.nextPick;
  const { order } = state;
  if (!order.length) return null;
  const start = state.onAir ? order.indexOf(state.onAir) + 1 : 0;
  for (let step = 0; step < order.length; step += 1) {
    const id = order[(start + step + order.length) % order.length];
    if (id !== state.onAir && playable(state, id)) return id;
  }
  return null;
}

export function roleOf(state: SessionState, id: string): Role {
  if (state.onAir === id) return "onAir";
  if (nextPerformer(state) === id) return "next";
  if (state.previous === id) return "previous";
  return "idle";
}

export function routeFor(state: SessionState, id: string): Route {
  const role = roleOf(state, id);
  const uplink = role === "onAir" || role === "next" ? "full" : "local";
  let feedFrom: string | null;
  if (role === "onAir") {
    feedFrom = state.previous && state.previous !== id && participant(state, state.previous)?.connected ? state.previous : null;
  } else if (role === "previous") {
    feedFrom = null;
  } else {
    feedFrom = state.onAir && state.onAir !== id ? state.onAir : null;
  }
  return { uplink, feedFrom };
}

export function routes(state: SessionState): Record<string, Route> {
  return Object.fromEntries(state.participants.map((p) => [p.id, routeFor(state, p.id)]));
}

export function addParticipant(state: SessionState, p: Participant, joinOrder = true): SessionState {
  const existing = participant(state, p.id);
  const participants = existing
    ? state.participants.map((q) => (q.id === p.id ? { ...q, ...p } : q))
    : [...state.participants, p];
  const order = joinOrder && !state.order.includes(p.id) ? [...state.order, p.id] : state.order;
  return bump(state, { participants, order });
}

export function setConnected(state: SessionState, id: string, connected: boolean): SessionState {
  const p = participant(state, id);
  if (!p || p.connected === connected) return state;
  return bump(state, { participants: state.participants.map((q) => (q.id === id ? { ...q, connected } : q)) });
}

/** A participant left for good. If they were on air the program keeps their
 * (now silent) slot until someone presses NEXT: the host never cuts by itself. */
export function removeParticipant(state: SessionState, id: string): SessionState {
  if (!participant(state, id)) return state;
  return bump(state, {
    participants: state.participants.map((q) => (q.id === id ? { ...q, connected: false } : q)),
    order: state.order.filter((x) => x !== id),
    previous: state.previous === id ? null : state.previous,
    nextPick: state.nextPick === id ? null : state.nextPick,
  });
}

export function setOrder(state: SessionState, order: string[]): SessionState {
  const known = new Set(state.participants.map((p) => p.id));
  const unique = [...new Set(order)];
  if (unique.some((id) => !known.has(id))) throw new TurnError("順番に知らない参加者が含まれています");
  return bump(state, { order: unique, nextPick: state.nextPick && unique.includes(state.nextPick) ? state.nextPick : null });
}

export function setNextPick(state: SessionState, id: string | null): SessionState {
  if (id !== null && !state.order.includes(id)) throw new TurnError("順番に入っていないDJは次にできません");
  if (id !== null && id === state.onAir) throw new TurnError("ON AIR中のDJは次にできません");
  return bump(state, { nextPick: id });
}

/** Host, on-air DJ and next DJ may press NEXT. */
export function canAdvance(state: SessionState, by: string, hostId: string): boolean {
  const next = nextPerformer(state);
  if (!next) return false;
  return by === hostId || by === state.onAir || by === next;
}

export function advance(state: SessionState, by: string, hostId: string): SessionState {
  const next = nextPerformer(state);
  if (!next) throw new TurnError("次に交代できるDJがいません");
  if (!canAdvance(state, by, hostId)) throw new TurnError("NEXTはホスト、ON AIR中のDJ、次のDJだけが押せます");
  return bump(state, {
    previous: state.onAir,
    onAir: next,
    nextPick: null,
    handoffs: state.handoffs + 1,
  });
}

/** The on-air DJ has faded the previous DJ out: stop relaying their tail. */
export function releasePrevious(state: SessionState, by: string, hostId: string): SessionState {
  if (!state.previous) return state;
  if (by !== hostId && by !== state.onAir) throw new TurnError("前のDJの音を止められるのはON AIR中のDJかホストです");
  return bump(state, { previous: null });
}

/** Stop the set: nobody on air. The program goes silent. */
export function stopSet(state: SessionState): SessionState {
  return bump(state, { onAir: null, previous: null, nextPick: null });
}

/** Guests accept a broadcast only if it is newer than what they have. */
export function acceptBroadcast(current: SessionState | null, incoming: SessionState): SessionState {
  return !current || incoming.revision > current.revision ? incoming : current;
}

export function isSessionState(value: unknown): value is SessionState {
  if (!value || typeof value !== "object") return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v.revision === "number" &&
    Array.isArray(v.participants) &&
    Array.isArray(v.order) &&
    (v.onAir === null || typeof v.onAir === "string") &&
    (v.previous === null || typeof v.previous === "string") &&
    (v.nextPick === null || typeof v.nextPick === "string") &&
    typeof v.handoffs === "number"
  );
}

/**
 * Decides when the on-air DJ has faded the previous DJ out of the J deck, from
 * the on-air DJ's periodic "J audible" reports. Only a deck that was heard
 * first can be faded out; a stream that stops arriving counts as gone.
 */
export class ReleaseWatch {
  static readonly silentMs = 1500;
  private heard = false;
  private silentSince: number | null = null;
  private key = "";

  /** Returns true when previous should be released. */
  observe(state: SessionState, jAudible: boolean, nowMs: number): boolean {
    const key = `${state.handoffs}:${state.previous ?? ""}`;
    if (key !== this.key) { this.key = key; this.heard = false; this.silentSince = null; }
    if (!state.previous) return false;
    if (jAudible) { this.heard = true; this.silentSince = null; return false; }
    if (!this.heard) return false;
    if (this.silentSince === null) { this.silentSince = nowMs; return false; }
    return nowMs - this.silentSince >= ReleaseWatch.silentMs;
  }
}
