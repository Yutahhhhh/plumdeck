import { useMemo } from "react";
import { JunctionInputDeck } from "@/components/play/JunctionInputDeck";
import { junctionLink } from "@/services/junction-v2/session";
import { routeFor } from "@/services/junction-v2/core/turns";
import type { JunctionInputState } from "@/types/junction";
import { TurnControls } from "./JunctionLinkPanel";
import { nameOf, turnView, useJunctionLink } from "./useJunctionLink";
import "./junction-link.css";

/**
 * Play view while Junction v2 is active: whose turn it is, NEXT, and the
 * JUNCTION deck (the other DJs' audio as a normal mixer channel with level,
 * EQ, crossfader assign and CUE on this machine's engine).
 */
export function JunctionLinkTurnBar() {
  const s = useJunctionLink();
  return turnView(s) ? <div className="jl-strip"><TurnControls s={s} compact /></div> : null;
}

export function JunctionLinkDeck() {
  const s = useJunctionLink();
  const view = turnView(s);
  const feedFrom = view && s.me ? routeFor(view.session, s.me).feedFrom : null;
  const engine = s.engine;
  const deckState = useMemo<JunctionInputState | null>(() => {
    if (!engine?.deck?.available) return null;
    return {
      peerId: feedFrom ?? "",
      releasingPeerId: view?.myRole === "onAir" && view.previous ? view.previous : "",
      receiving: engine.input.receiving,
      latencyMs: engine.input.latencyMs,
      underruns: String(engine.input.underruns),
      channel: engine.deck,
      djName: feedFrom ? nameOf(s, feedFrom) : undefined,
      lane: { bucketMs: 100, peaks: [], decks: [] },
    };
  }, [engine, feedFrom, view?.myRole, view?.previous, s]);
  if (!view || !deckState) return null;
  return (
    <div className="dj-deck-pair dj-deck-pair--junction">
      <JunctionInputDeck
        state={deckState}
        performerName={deckState.djName}
        mixingTail={view.myRole === "onAir" && Boolean(view.previous)}
        onSet={(settings) => junctionLink.setInput(settings)}
        onRelease={async () => junctionLink.releasePrevious()}
      />
    </div>
  );
}
