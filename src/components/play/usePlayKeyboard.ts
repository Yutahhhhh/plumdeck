import { useEffect, useRef } from "react";
import type { DjEngineClient } from "@/services/dj-engine/client";
import { Ddj1000Runtime, type ControllerActions } from "@/services/midi/ddj1000-runtime";
import { junctionLeaseKey } from "@/services/junction/state";
import type { DeckId } from "@/types/dj-engine";
import { createKeyboardRuntime, dispatchKeyboardLoad } from "./keyboard-runtime";
import { isMomentary, type ShortcutPreset } from "./keyboard-shortcuts";

type Options = {
  client: DjEngineClient;
  preset: ShortcutPreset;
  actions: ControllerActions;
  activeDeck: DeckId;
  visibleDecks: readonly DeckId[];
  togglePlay: (deck: DeckId) => void;
  seekRelative: (deck: DeckId, deltaMs: number) => void;
  toggleLayout: () => void;
  /** Manual loop as on screen (Q snapping, pending IN mark, RELOOP to IN). */
  loop: Record<"loopIn" | "loopOut" | "reloop", (deck: DeckId) => void>;
};

/** Shares controller commands without opening MIDI or altering the view. */
export function usePlayKeyboard(options: Options) {
  const latest = useRef(options);
  latest.current = options;
  useEffect(() => {
    const client = options.client;
    const controller = new Ddj1000Runtime(client, () => latest.current.actions);
    const bends = new Map<DeckId, { token: symbol; direction: number; valid: () => boolean }[]>();
    const keyboard = createKeyboardRuntime(() => latest.current.preset, action => {
      const current = latest.current;
      if (action.kind === "layout") { current.toggleLayout(); return; }
      if (action.kind === "zoom") { current.actions.library({ control: "zoom", value: action.direction }); return; }
      if (action.kind === "selectDeck") {
        const deck = current.visibleDecks[action.deck];
        if (!deck) return false;
        current.actions.activate(deck);
        return;
      }
      if (action.kind === "togglePlayActive") { current.togglePlay(current.activeDeck); return; }
      if (action.kind === "seekActive") { current.seekRelative(current.activeDeck, action.deltaMs); return; }
      if (!("side" in action)) return false;
      const deck: DeckId = action.side === 0 ? "A" : "B";
      if (action.kind === "load") { dispatchKeyboardLoad(action, command => controller.dispatch(command)); return; }
      const state = client.getState().snapshot?.decks[deck];
      if (!state?.track || ["empty", "loading", "error"].includes(state.status)) return false;
      if (action.kind === "loopIn" || action.kind === "loopOut" || action.kind === "reloop") { current.loop[action.kind](deck); return; }
      const session = client.getSessionId(), generation = client.getDeckGeneration(deck), lease = junctionLeaseKey();
      const valid = () => session === client.getSessionId() && generation === client.getDeckGeneration(deck) && lease === junctionLeaseKey();
      if (action.kind === "bend") {
        if (!client.getState().snapshot?.engine.capabilities.includes("deck.pitchbend")) return false;
        const send = (amount: number) => {
          void Promise.resolve().then(() => { if (valid()) return client.pitchbend(deck, amount); })
            .catch(error => latest.current.actions.error(String(error)));
        };
        const token = Symbol();
        const held = (bends.get(deck) ?? []).filter(item => item.valid());
        held.push({ token, direction: action.direction, valid });
        bends.set(deck, held);
        send(action.direction * 0.04);
        const timer = window.setInterval(() => {
          if (!valid()) { window.clearInterval(timer); return; }
          const active = bends.get(deck);
          if (active?.[active.length - 1]?.token === token) send(action.direction * 0.04);
        }, 50);
        return () => {
          window.clearInterval(timer);
          const remaining = (bends.get(deck) ?? []).filter(item => item.token !== token && item.valid());
          if (remaining.length) bends.set(deck, remaining); else bends.delete(deck);
          send((remaining[remaining.length - 1]?.direction ?? 0) * 0.04);
        };
      }
      const control = action.kind === "memoryPrevious" ? "cuePrevious" : action.kind === "memoryNext" ? "cueNext" : action.kind;
      const command = { control, deck, value: 1, pressed: true, ...("slot" in action ? { slot: action.slot } : {}) };
      controller.dispatch(command);
      if (isMomentary(action)) return () => {
        if (valid()) controller.dispatch({ ...command, value: 0, pressed: false });
      };
    });
    const down = (event: KeyboardEvent) => {
      const target = event.target instanceof Element ? event.target : null;
      const blocked = event.defaultPrevented || event.isComposing || Boolean(document.querySelector('[role="dialog"]') || target?.closest('input,textarea,select,button,[role=slider],[contenteditable]:not([contenteditable="false"])'));
      if (keyboard.keyDown(event, blocked)) event.preventDefault();
    };
    const up = (event: KeyboardEvent) => { if (keyboard.keyUp(event.code)) event.preventDefault(); };
    const blur = () => {
      try { keyboard.releaseAll(); } catch (error) { latest.current.actions.error(String(error)); }
    };
    const visibility = () => { if (document.hidden) blur(); };
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    window.addEventListener("blur", blur);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
      window.removeEventListener("blur", blur);
      document.removeEventListener("visibilitychange", visibility);
      try { blur(); } finally { controller.dispose(); }
    };
  }, [options.client]);
}
