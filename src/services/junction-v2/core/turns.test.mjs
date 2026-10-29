import assert from "node:assert/strict";
import test from "node:test";
import {
  acceptBroadcast, addParticipant, advance, canAdvance, emptySession, nextPerformer, releasePrevious,
  removeParticipant, ReleaseWatch, roleOf, routeFor, routes, setConnected, setNextPick, setOrder, stopSet,
} from "./turns.ts";
import { decodeCode, encodeCode, parseGuestMessage, parseHostMessage } from "./protocol.ts";

const host = { id: "H", name: "Host", kind: "host", connected: true };
const dj = (id, kind = "app") => ({ id, name: id, kind, connected: true });

function fiveDjs() {
  let s = emptySession(host);
  for (const id of ["A", "B", "C", "D"]) s = addParticipant(s, dj(id, id === "C" ? "lite" : "app"));
  return s;
}

test("order starts with the host and NEXT walks it cyclically", () => {
  let s = fiveDjs();
  assert.deepEqual(s.order, ["H", "A", "B", "C", "D"]);
  assert.equal(s.onAir, null);
  assert.equal(nextPerformer(s), "H");
  s = advance(s, "H", "H");
  assert.equal(s.onAir, "H");
  assert.equal(s.previous, null);
  assert.equal(nextPerformer(s), "A");
  for (const expected of ["A", "B", "C", "D", "H", "A"]) {
    const before = s.onAir;
    s = advance(s, s.onAir, "H");
    assert.equal(s.onAir, expected);
    assert.equal(s.previous, before);
  }
  assert.equal(s.handoffs, 7);
});

test("disconnected DJs are skipped, explicit next pick wins", () => {
  let s = advance(fiveDjs(), "H", "H");
  s = setConnected(s, "A", false);
  assert.equal(nextPerformer(s), "B");
  s = setNextPick(s, "D");
  assert.equal(nextPerformer(s), "D");
  s = advance(s, "D", "H");
  assert.equal(s.onAir, "D");
  assert.equal(s.nextPick, null);
  assert.equal(nextPerformer(s), "H");
  assert.throws(() => setNextPick(s, "D"), /ON AIR/);
  assert.throws(() => setNextPick(s, "X"), /順番/);
});

test("only host, on-air and next may press NEXT", () => {
  let s = advance(fiveDjs(), "H", "H");
  s = advance(s, "H", "H"); // A on air, B next
  assert.equal(canAdvance(s, "A", "H"), true);
  assert.equal(canAdvance(s, "B", "H"), true);
  assert.equal(canAdvance(s, "H", "H"), true);
  assert.equal(canAdvance(s, "C", "H"), false);
  assert.throws(() => advance(s, "C", "H"), /NEXT/);
});

test("routes: on air hears previous, others hear on air, full uplink only for on air and next", () => {
  let s = advance(fiveDjs(), "H", "H");
  s = advance(s, "H", "H"); // previous H, on air A, next B
  assert.equal(roleOf(s, "A"), "onAir");
  assert.equal(roleOf(s, "B"), "next");
  assert.equal(roleOf(s, "H"), "previous");
  assert.equal(roleOf(s, "C"), "idle");
  assert.deepEqual(routeFor(s, "A"), { uplink: "full", feedFrom: "H" });
  assert.deepEqual(routeFor(s, "B"), { uplink: "full", feedFrom: "A" });
  assert.deepEqual(routeFor(s, "H"), { uplink: "local", feedFrom: null });
  assert.deepEqual(routeFor(s, "C"), { uplink: "local", feedFrom: "A" });
  s = releasePrevious(s, "A", "H");
  assert.deepEqual(routeFor(s, "A"), { uplink: "full", feedFrom: null });
  assert.deepEqual(routeFor(s, "H"), { uplink: "local", feedFrom: "A" });
});

test("release watch fires after the on-air DJ fades a heard J deck out", () => {
  let s = advance(advance(fiveDjs(), "H", "H"), "H", "H");
  const watch = new ReleaseWatch();
  assert.equal(watch.observe(s, false, 0), false, "never heard: not a fade");
  assert.equal(watch.observe(s, false, 5000), false);
  assert.equal(watch.observe(s, true, 6000), false);
  assert.equal(watch.observe(s, false, 7000), false);
  assert.equal(watch.observe(s, false, 8000), false);
  assert.equal(watch.observe(s, false, 8600), true);
  s = advance(s, "A", "H");
  assert.equal(watch.observe(s, false, 9000), false, "new handoff resets");
});

test("no participant ever receives a feed that contains its own uplink", () => {
  // Brute force over many states: build "contains" closure of full uplinks.
  let s = fiveDjs();
  for (let step = 0; step < 12; step += 1) {
    s = advance(s, s.onAir ?? "H", "H");
    if (step === 5) s = setConnected(s, "B", false);
    if (step === 8) s = setConnected(s, "B", true);
    const r = routes(s);
    const contains = (id, seen = new Set()) => {
      if (seen.has(id)) return seen;
      seen.add(id);
      if (r[id]?.uplink === "full" && r[id].feedFrom) contains(r[id].feedFrom, seen);
      return seen;
    };
    for (const p of s.participants) {
      const feed = r[p.id].feedFrom;
      if (!feed) continue;
      assert.ok(!contains(feed).has(p.id), `step ${step}: ${p.id} hears itself through ${feed}`);
    }
  }
});

test("removing a participant drops them from the order; stop clears the air", () => {
  let s = advance(advance(fiveDjs(), "H", "H"), "H", "H");
  s = removeParticipant(s, "H");
  assert.equal(s.previous, null);
  assert.ok(!s.order.includes("H"));
  s = setOrder(s, ["B", "A", "C", "D"]);
  assert.equal(nextPerformer(s), "C");
  s = stopSet(s);
  assert.equal(s.onAir, null);
  assert.equal(nextPerformer(s), "B");
});

test("guests keep the newest broadcast only", () => {
  const a = fiveDjs();
  const b = advance(a, "H", "H");
  assert.equal(acceptBroadcast(b, a), b);
  assert.equal(acceptBroadcast(a, b), b);
  assert.equal(acceptBroadcast(null, a), a);
});

test("protocol messages validate and codes round-trip", () => {
  assert.deepEqual(parseGuestMessage(JSON.stringify({ t: "request", request: { action: "next" } })), { t: "request", request: { action: "next" } });
  assert.equal(parseGuestMessage(JSON.stringify({ t: "request", request: { action: "hack" } })), null);
  assert.equal(parseGuestMessage("{"), null);
  const s = fiveDjs();
  assert.deepEqual(parseHostMessage(JSON.stringify({ t: "state", state: s })), { t: "state", state: s });
  const sdp = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\na=日本語\r\n";
  const code = encodeCode({ type: "offer", sdp });
  assert.match(code, /^JLINK2\.[A-Za-z0-9_-]+$/);
  assert.deepEqual(decodeCode(`  ${code}\n`, "offer"), { type: "offer", sdp });
  assert.throws(() => decodeCode(code, "answer"), /応答コード/);
  assert.throws(() => decodeCode("hello", "offer"), /Junction/);
});
