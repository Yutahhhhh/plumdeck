import { junctionLeaseKey } from '@/services/junction/state';
import { localTrackId } from '@/services/junction/asset-resolver';
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { djEngineClient } from "@/services/dj-engine/client";
import { Ddj1000Decoder, ddj1000Feedback, ddj1000ExtendedFeedback } from "@/services/midi/ddj1000";
import { Ddj1000Runtime, type ControllerActions } from "@/services/midi/ddj1000-runtime";
import { JUNCTION_CHANNEL_KEY, readJunctionChannel, type JunctionChannelAssign } from "@/services/midi/junction-channel";
import { sampler } from "@/services/dj-engine/sampler";
import { padSelectionFeedback } from "@/services/midi/pad-state";
import { JOG_DEFAULT, jogSetting } from "@/services/midi/jog-settings";
import { coloredPadFeedback } from "@/services/midi/pad-colors";
import { jogFrame, isJogScreenMidi, type JogAssets } from "@/services/midi/ddj1000-display";
import { loadJogAssets } from "@/services/midi/ddj1000-display-assets";
import { GenericMidiDecoder, parseGenericProfile } from "@/services/midi/generic-midi";
import { DDJ400_PROFILE } from "@/services/midi/ddj400";
export type MidiStatus = { enabled: boolean; connected: boolean; generation: number; device: string | null; received: number; sent: number; error: string | null;
  nativePerformance?: {active:boolean;cues:number[];ranges:number[]};
  display?: { midiOpen: boolean; hidOpen: boolean; authenticated: boolean; reportsSent: number; error: string | null } };
const OFF: MidiStatus = { enabled: false, connected: false, generation: 0, device: null, received: 0, sent: 0, error: null };
export function useDdj1000(actions: ControllerActions) {
  const latest = useRef(actions); latest.current = actions;
  const [enabled, setEnabled] = useState(() => localStorage.getItem("plumdeck.ddj1000.enabled") !== "false");
  const [status, setStatus] = useState<MidiStatus>(OFF);
  const [retry, setRetry] = useState(0);
  const [junctionChannel, setJunctionChannelState] = useState<JunctionChannelAssign>(() => readJunctionChannel(localStorage.getItem(JUNCTION_CHANNEL_KEY)));
  const junctionChannelRef = useRef(junctionChannel);
  const lightTestUntil = useRef(0);
  const [sensitivity, setSensitivity] = useState(() => {
    return jogSetting(Number(localStorage.getItem("plumdeck.ddj1000.jogSensitivity") ?? JOG_DEFAULT),localStorage.getItem("plumdeck.ddj1000.jogSensitivityVersion"));
  });
  const sensitivityRef = useRef(sensitivity); sensitivityRef.current = sensitivity;
  useEffect(() => { localStorage.setItem("plumdeck.ddj1000.enabled", String(enabled)); }, [enabled]);
  useEffect(() => {
    const next=jogSetting(sensitivity,localStorage.getItem("plumdeck.ddj1000.jogSensitivityVersion"));
    sensitivityRef.current=next;
    if(next!==sensitivity) setSensitivity(next);
    localStorage.setItem("plumdeck.ddj1000.jogSensitivity",String(next));
    localStorage.setItem("plumdeck.ddj1000.jogSensitivityVersion","4");
  }, [sensitivity]);
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window || "__TAURI__" in window)) { setStatus({ ...OFF, error: "MIDI接続はデスクトップ版で利用できます" }); return; }
    let live = true, polling = false, writing = false, initialized = false;
    let connection = OFF, engineSession = djEngineClient.getSessionId();
    const genericProfile = parseGenericProfile(localStorage.getItem("plumdeck.midi.profile"));
    let decoder = genericProfile && genericProfile.adapterId !== "ddj1000" ? new GenericMidiDecoder(genericProfile) : new Ddj1000Decoder();
    void invoke("dj_midi_select_device", { device: localStorage.getItem("plumdeck.midi.device") || null }).catch(() => undefined);
    const runtime = new Ddj1000Runtime(djEngineClient, () => latest.current);
    for (const deck of ["A", "B", "C", "D"] as const) runtime.setTempoRange(deck, Number(localStorage.getItem(`plumdeck.tempoRange.${deck}`)) || 16);
    runtime.junctionChannel = junctionChannelRef.current;
    const junctionChannelChange = (event: Event) => { runtime.junctionChannel = (event as CustomEvent<JunctionChannelAssign>).detail; runtime.reset(); };
    window.addEventListener("plumdeck:controller-junction-channel", junctionChannelChange);
    const tempoRange = (event: Event) => {
      const detail = (event as CustomEvent<{ deck: import("@/types/dj-engine").DeckId; range: number }>).detail;
      runtime.setTempoRange(detail.deck, detail.range);
    };
    window.addEventListener("plumdeck:controller-tempo-range", tempoRange);
    const sent = new Map<string, number>();
    let refreshFeedbackAt = 0;
    let reading = false;
    let displayWriting = false;
    const displayAssets = new Map<string, { token: string; assets?: JogAssets }>();
    const reset = () => { decoder.reset(); runtime.reset(); sent.clear(); initialized = false; };
    let performanceConfigured = "";
    const poll = async () => {
      if (!live || polling) return; polling = true;
      try {
        const next = await invoke<MidiStatus>("dj_midi_status", { enabled });
        if (!live) return;
        if (connection.generation !== next.generation || connection.connected !== next.connected) {
          reset();
          if (!genericProfile) decoder = next.device?.toUpperCase().startsWith("DDJ-400") ? new GenericMidiDecoder(DDJ400_PROFILE) : new Ddj1000Decoder();
          performanceConfigured = "";
        }
        connection = next; setStatus(next);
        const snapshot = djEngineClient.getState().snapshot;
        if (next.nativePerformance?.active) for (const [i,deck] of (["A","B","C","D"] as const).entries()) {
          const range=next.nativePerformance.ranges?.[i];
          if([6,10,16,75].includes(range)&&range!==Number(localStorage.getItem(`plumdeck.tempoRange.${deck}`))){
            runtime.setTempoRange(deck,range);latest.current.library({control:'tempoRange',deck,value:range});
          }
          const cue = next.nativePerformance.cues[i]; if (Number.isFinite(cue)) latest.current.cuePoints[deck] = cue;
        }
        if ((!genericProfile || genericProfile.adapterId === "ddj1000") && next.device?.toUpperCase().startsWith("DDJ-1000") && snapshot?.engine.capabilities.includes("performance.midi.v2") && djEngineClient.getSessionId()) {
          const ranges = (["A","B","C","D"] as const).map(deck => Number(localStorage.getItem(`plumdeck.tempoRange.${deck}`)) || 16);
          const token = `${djEngineClient.getSessionId()}:${sensitivityRef.current}:${ranges.join(',')}`;
          if (token !== performanceConfigured) {
            await invoke("dj_midi_performance_config", {sessionId:djEngineClient.getSessionId(),sensitivity:sensitivityRef.current,ranges,cues:(["A","B","C","D"] as const).map(deck=>latest.current.cuePoints[deck])});
            performanceConfigured = token;
          }
        }
        if (enabled && next.connected && !initialized && (!genericProfile || genericProfile.adapterId === "ddj1000") && next.device?.toUpperCase().startsWith("DDJ-1000")) {
          // DDJ-1000 PC APP CONNECT: request the current hardware controls.
          // Install the listener and generation first, or the reply is lost.
          await invoke("dj_midi_send", { generation: next.generation, messages: [[0x9f, 0x09, 0x7f]] });
          if (live && connection.generation === next.generation) initialized = true;
        }
      } catch (e) { if (live) { reset(); connection = OFF; setStatus({ ...OFF, error: String(e) }); } }
      finally { polling = false; }
    };
    void poll();
    const inputTimer = setInterval(() => {
      if (!live || !enabled || !connection.connected || reading) return;
      reading = true;
      const inputLease = junctionLeaseKey();
      void invoke<{ generation: number; messages: number[][]; nativePerformance?: boolean }[]>("dj_midi_read").then(events => {
        if (!live || inputLease !== junctionLeaseKey()) return;
        runtime.jogSensitivity = sensitivityRef.current;
        for (const event of events) {
          if (!connection.connected || event.generation !== connection.generation) continue;
          for (const bytes of event.messages) for (const action of decoder.feed(bytes)) {
            // Every packet carries its execution owner; buffered UI observation
            // cannot re-execute native edges after a route change.
            if (event.nativePerformance && ["jog","searchJog","nudge","touch","vinyl","keylock","tempoRange","tempo","crossfader","gain","trim","eqHigh","eqMid","eqLow","play","cue","start"].includes(action.control)) continue;
            runtime.dispatch(action);
          }
        }
      }).catch(e => {
        if (live) { reset(); connection = OFF; setStatus({ ...OFF, error: String(e) }); }
      }).finally(() => { reading = false; });
    }, 4); // Match the MIDI worker cadence; frame-rate polling adds latency to cuts.
    const pollTimer = setInterval(() => void poll(), 500);
    const displayTimer = setInterval(() => {
      if (!live || !enabled || !connection.connected || displayWriting || (genericProfile && genericProfile.adapterId !== "ddj1000") || !connection.device?.toUpperCase().startsWith("DDJ-1000")) return;
      const snapshot = djEngineClient.getState().snapshot;
      const decks = (["A", "B", "C", "D"] as const).map(id => {
        const deck = snapshot?.decks[id];
        const token = deck?.track ? `${djEngineClient.getSessionId()}:${id}:${djEngineClient.getDeckGeneration(id)}:${deck.track.trackId}` : "";
        let entry = displayAssets.get(id);
        if (!entry || entry.token !== token) {
          entry = { token }; displayAssets.set(id, entry);
          const target = entry, trackId = localTrackId(deck?.track);
          if (token && trackId !== null) void loadJogAssets(trackId).then(assets => {
            if (live && displayAssets.get(id) === target) target.assets = assets;
          }).catch(e => { if (live && displayAssets.get(id) === target) latest.current.error(`ジョグ画面の画像取得: ${String(e)}`); });
        }
        return jogFrame(deck, token, entry.assets);
      });
      displayWriting = true;
      void invoke("dj_jog_display_update", { generation: connection.generation, decks })
        .catch(e => { if (live) setStatus(previous => ({ ...previous, error: String(e) })); })
        .finally(() => { displayWriting = false; });
    }, 50);
    const feedbackTimer = setInterval(() => {
      if (!live || !enabled || !connection.connected || !initialized || writing || (genericProfile && genericProfile.adapterId !== "ddj1000") || !connection.device?.toUpperCase().startsWith("DDJ-1000")) return;
      const session = djEngineClient.getSessionId();
      if (session !== engineSession) { reset(); engineSession = session; }
      // MIDI output has no delivery acknowledgement. Re-send static LEDs too
      // so a late hardware startup cannot leave an OS-accepted frame cached forever.
      if (Date.now() >= refreshFeedbackAt) { sent.clear(); refreshFeedbackAt = Date.now() + 2000; }
      const messages = coloredPadFeedback([...ddj1000Feedback(djEngineClient.getState().snapshot, djEngineClient.getState().meters), ...ddj1000ExtendedFeedback(djEngineClient.getState().snapshot, sampler.getSnapshot()), ...padSelectionFeedback()],djEngineClient.getState().snapshot,latest.current.cueColors,sampler.getSnapshot())
        .filter(message => !connection.display?.hidOpen || !isJogScreenMidi(message))
        .map(message => lightTestUntil.current > Date.now() && message[0] >= 0x90 && message[0] <= 0x93 && message[1] === 0x0c
          ? [message[0], message[1], Math.floor(Date.now() / 500) % 2 ? 127 : 0] : message)
        .filter(([s, k, v]) => sent.get(`${s}:${k}`) !== v);
      if (!messages.length) return;
      writing = true;
      const generation = connection.generation;
      void (async () => {
        for (let offset = 0; offset < messages.length; offset += 128) {
          if (!live || generation !== connection.generation) return;
          const batch = messages.slice(offset, offset + 128);
          await invoke("dj_midi_send", { generation, messages: batch });
          if (live && generation === connection.generation) for (const [s,k,v] of batch) sent.set(`${s}:${k}`,v);
        }
      })().catch(e => { if (live) setStatus(previous => ({ ...previous, error: String(e) })); }).finally(() => { writing = false; });
    }, 50);
    return () => {
      live = false; clearInterval(displayTimer); clearInterval(inputTimer); clearInterval(pollTimer); clearInterval(feedbackTimer); window.removeEventListener("plumdeck:controller-tempo-range", tempoRange); window.removeEventListener("plumdeck:controller-junction-channel", junctionChannelChange); runtime.dispose();
      // The native lease closes the ports and clears feedback itself. Avoid a
      // delayed cleanup disabling the next effect after a reconnect.
      void invoke("dj_midi_status", { enabled: false }).catch(() => undefined);
    };
  }, [enabled, retry]);
  const setJunctionChannel = (value: JunctionChannelAssign) => {
    junctionChannelRef.current = value; setJunctionChannelState(value);
    try { localStorage.setItem(JUNCTION_CHANNEL_KEY, value); } catch { /* per-viewer convenience only */ }
    window.dispatchEvent(new CustomEvent("plumdeck:controller-junction-channel", { detail: value }));
  };
  return { status, enabled, setEnabled, sensitivity, setSensitivity, junctionChannel, setJunctionChannel,
    reconnect: () => setRetry(value => value + 1),
    testLights: () => { lightTestUntil.current = Date.now() + 4000; } };
}
