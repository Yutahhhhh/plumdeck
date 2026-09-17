import { junctionLeaseKey, junctionState } from '../junction/state.ts';
import { padPreset, BEAT_LOOP_PAGES, BEAT_JUMP_PAGES, keyPad } from "./pad-presets.ts";
import { getPadSelection, selectPads, subscribePads } from "./pad-state.ts";
import { BEAT_FX, beatFxMaxBeats } from "./beat-fx.ts";
import type { DjEngineClient } from "../dj-engine/client";
import type { BeatFxState, DeckId, PadEffect } from "../../types/dj-engine";
import type { MidiAction } from "./ddj1000";
import { controllerTailLock, junctionChannelActive, junctionChannelSettings, junctionChannelTarget, type JunctionChannelAssign } from "./junction-channel.ts";
import { junctionCommand } from "../junction/client.ts";
const DECKS: DeckId[] = ["A", "B", "C", "D"];
const clamp = (v: number, min: number, max: number) => Math.max(min, Math.min(max, v));
export type ControllerActions = {
  activate: (deck: DeckId) => void;
  library: (action: { control: string; deck?: DeckId; value: number; mode?: number }) => void;
  memory?: (deck: DeckId, action: "save" | "delete" | "previous" | "next") => Promise<unknown>;
  hotcue: (deck: DeckId, slot: number, clear: boolean) => Promise<unknown>;
  cuePoints: Record<DeckId, number>;
  cueColors?: Partial<Record<DeckId,(string|null)[]>>;
  error: (message: string) => void;
};
type Jog = { id: string; displacement: number; session: string | null; track: string; last: number;
  speedTime: number; speedPosition: number; speed: number; releaseTimer?: ReturnType<typeof setTimeout> };
/** Device gestures use the same acknowledged client and persistence callbacks as
 * the UI. Discrete actions are ordered per deck; continuous controls coalesce in
 * DjEngineClient. Session/track guards discard work after a load or reconnect. */
export class Ddj1000Runtime {
  private queues = new Map<string, Promise<unknown>>();
  private jogs = new Map<DeckId, Jog>();
  private seekPreview = new Map<DeckId, { position: number; at: number; session: string | null; track: string }>();
  private bends = new Map<DeckId, { rate: number; timer: ReturnType<typeof setTimeout>; session: string | null; track: string }>();
  private cuePreview = new Map<DeckId, { session: string | null; track: string }>();
  private heldReverse = new Map<DeckId, {session: string | null; track: string}>();
  private heldFx = new Map<DeckId, { effect: PadEffect; session: string | null; track: string }>();
  private padPresses = new Map<string, MidiAction>();
  private releaseFxHeld = false;
  private keyboardCues = new Map<DeckId,number>();
  private choosingKeyboardCue = new Set<DeckId>();
  private keyPages = new Map<DeckId,number>();
  private loopAdjust = new Map<DeckId, { target: "in" | "out"; session: string | null; track: string }>();
  private loopIn = new Map<DeckId, { positionMs: number; session: string | null; track: string }>();
  private ranges = new Map<DeckId, number>();
  private jumps = new Map<DeckId, number>();
  private vinyl = new Map<DeckId, boolean>();
  private colorEnabled = true;
  private colorEffect = "filter";
  private colorValues: Record<DeckId, number> = { A: 0, B: 0, C: 0, D: 0 };
  private nativeFx: BeatFxState = {effect:"echo",target:"A",enabled:false,mix:0.5,beats:1};
  private nativeFxSuspended: boolean | null = null;
  private fxTapTimes: number[] = [];
  private fxTarget: DeckId | "master" = "A";
  private fxEffect: PadEffect | null = "echo";
  private fxEnabled = false;
  private fxMix = 0.5;
  private fxMixQueued = false;
  private fxMixLatest: number | null = null;
  private nativeFxMixQueued = false;
  private nativeFxMixLatest: number | null = null;
  private epoch = 0;
  private junctionKey = junctionLeaseKey();
  private unsubscribeJunction: () => void;
  private pickup = new Map<string, {previous?: number; acquired: boolean}>();
  private alive = true;
  private heartbeat: ReturnType<typeof setInterval>;
  private unsubscribePads: () => void;
  // Legacy constructor default; the device hook supplies the user's calibrated setting.
  public jogSensitivity = 1.8;
  /** Physical mixer channel that carries JUNCTION MASTER while this DJ has J. */
  public junctionChannel: JunctionChannelAssign = "";
  private junctionPending: Record<string, unknown> = {};
  private junctionTimer: ReturnType<typeof setTimeout> | null = null;
  /** J strip moves coalesce like the on-screen fader: one native call per 50 ms. */
  private sendJunction(settings: Record<string, unknown>) {
    this.junctionPending = {...this.junctionPending, ...settings};
    if (this.junctionTimer) return;
    this.junctionTimer = setTimeout(() => {
      this.junctionTimer = null;
      const pending = this.junctionPending; this.junctionPending = {};
      if (this.alive) this.perform(() => junctionCommand("input.set", pending));
    }, 50);
  }
  private client: DjEngineClient;
  private actions: () => ControllerActions;
  constructor(client: DjEngineClient, actions: () => ControllerActions) {
    this.client = client; this.actions = actions;
    this.unsubscribeJunction = junctionState.subscribe(() => {
      const next = junctionLeaseKey();
      if (next === this.junctionKey) return;
      this.junctionKey = next; this.epoch++; this.pickup.clear();
      this.releaseFxHeld = false; this.fxMixLatest = null; this.nativeFxMixLatest = null;
      this.padPresses.clear(); this.cuePreview.clear(); this.heldReverse.clear(); this.heldFx.clear();
      for (const jog of this.jogs.values()) if (jog.releaseTimer) clearTimeout(jog.releaseTimer);
      this.jogs.clear(); for (const bend of this.bends.values()) clearTimeout(bend.timer); this.bends.clear();
    });
    const selections = DECKS.map(getPadSelection);
    this.unsubscribePads=subscribePads(()=>{
      DECKS.forEach((deck,index)=>{
        const selection=getPadSelection(deck);
        if(selection===selections[index]) return;
        selections[index]=selection;
        // A mode change can suppress the old mode's NOTE OFF in firmware.
        // Queue cleanup after any pending press so no held FX stays latched.
        const next=(this.queues.get(deck)??Promise.resolve()).then(async()=>{
          const held=this.heldFx.get(deck);
          if(!held) return;
          this.heldFx.delete(deck);
          if(this.valid(deck,held.session,held.track)) await this.client.setFx(deck,held.effect,false,0.5);
        }).catch(e=>this.actions().error(String(e)));
        this.queues.set(deck,next);
      });
    });
    this.heartbeat = setInterval(() => {
      for (const [deck, jog] of this.jogs) {
        if (!this.valid(deck, jog.session, jog.track)) { this.jogs.delete(deck); continue; }
        // Real motion already keeps the native watchdog alive. A duplicate
        // position during motion is interpreted as a stopped hand and damps
        // the velocity estimator, producing a dip every heartbeat interval.
        if (performance.now() - jog.last < 200) continue;
        this.perform(() => this.client.scratch(deck, "move", jog.displacement, jog.id, performance.now(), true));
      }
    }, 200);
  }
  setTempoRange(deck: DeckId, percent: number) { if ([6, 10, 16, 75].includes(percent)) this.ranges.set(deck, percent / 100); }
  private trackToken(deck: DeckId) {
    const track = this.client.getState().snapshot?.decks[deck].track?.trackId;
    return `${track ?? ""}:${this.client.getDeckGeneration(deck)}:${junctionLeaseKey()}`;
  }
  private valid(deck: DeckId, session: string | null, track: string) {
    return this.client.getSessionId() === session && this.trackToken(deck) === track;
  }
  private perform(task: () => Promise<unknown>) { void task().catch(e => this.actions().error(e instanceof Error ? e.message : String(e))); }
  /** Held gesture state is only meaningful for the track it was started on. A
   * load or a reconnect in between makes it stale, so drop it instead of
   * applying it to whatever is on the deck now. */
  private held<T extends { session: string | null; track: string }>(map: Map<DeckId, T>, deck: DeckId): T | undefined {
    const entry = map.get(deck);
    if (!entry) return undefined;
    if (this.valid(deck, entry.session, entry.track)) return entry;
    map.delete(deck);
    return undefined;
  }
  private softTakeover(action: MidiAction): boolean {
    if (!junctionState.active()) return true;
    const {control,deck,value} = action;
    const snapshot = this.client.getState().snapshot;
    if (!snapshot) return false;
    if (junctionChannelActive(this.junctionChannel, deck, junctionState.get())) {
      const target = junctionChannelTarget(control, junctionState.get()?.junctionInput?.channel);
      if (target === undefined) return true;
      return this.acquire(`J:${control}`, value, target);
    }
    const targetDeck = /^filter[A-D]$/.test(control) ? control.slice(-1) as DeckId : deck;
    const channel = targetDeck ? snapshot.mixer.channels[targetDeck] : undefined;
    let target: number | undefined;
    if (control === 'crossfader') target = (snapshot.mixer.crossfader + 1) / 2;
    else if (control === 'gain' && channel) target = channel.gain;
    else if (control === 'trim' && channel) target = (channel.trim ?? 1) / 2;
    else if (control.startsWith('eq') && channel) { const gain = ({eqLow:channel.eqLow,eqMid:channel.eqMid,eqHigh:channel.eqHigh} as Record<string,number>)[control]; if (gain !== undefined) target = gain <= 1 ? gain / 2 : .5 + (gain - 1) / 6; }
    else if ((control === 'filter' || /^filter[A-D]$/.test(control)) && channel) target = ((channel.filter ?? 0) + 1) / 2;
    else if (control === 'tempo' && deck) target = .5 + (snapshot.decks[deck].rate - 1) / (2 * (this.ranges.get(deck) ?? .16));
    else if (control === 'fxMix') target = snapshot.mixer.beatFx?.mix ?? .5;
    if (target === undefined || !Number.isFinite(target)) return true;
    return this.acquire(`${deck ?? 'master'}:${control}`, value, target);
  }
  private acquire(key: string, value: number, target: number): boolean {
    const pickup = this.pickup.get(key) ?? {acquired:false};
    if (!pickup.acquired) pickup.acquired = Math.abs(value-target) < .025 || (pickup.previous !== undefined && (pickup.previous-target)*(value-target)<=0);
    pickup.previous=value;this.pickup.set(key,pickup);
    if (!pickup.acquired) this.actions().error('値を合わせると操作できます');
    return pickup.acquired;
  }
  dispatch(action: MidiAction) {
    if (!this.alive) return;
    const junction = junctionState.get();
    // The J strip belongs to the receiver: it is never part of an outgoing tail.
    const jStrip = junctionChannelActive(this.junctionChannel, action.deck, junction) && junctionChannelSettings(action.control, action.value, action.pressed, junction?.junctionInput?.channel) !== null;
    if (!jStrip && controllerTailLock(action.control, action.deck, junction)) {
      // Dropped without a toast: the hardware keeps moving while the engine
      // does not, so this control must be picked up again after the release.
      this.pickup.set(`${action.deck ?? 'master'}:${action.control}`, {previous: action.value, acquired: false});
      return;
    }
    if (!this.softTakeover(action)) return;
    if (jStrip) {
      const settings = junctionChannelSettings(action.control, action.value, action.pressed, junction?.junctionInput?.channel);
      if (settings) this.sendJunction({...settings});
      return;
    }
    if (action.deck && action.slot !== undefined && action.mode !== undefined) {
      const key = `${action.deck}:${action.mode}:${action.slot}:${Boolean(action.shift)}`;
      if (action.pressed === false) {
        const held = this.padPresses.get(key); this.padPresses.delete(key);
        if (held) action = {...held,pressed:false,value:0};
      } else if (action.pressed) {
        const selection=getPadSelection(action.deck);
        // Some firmware versions do not accept page/mode writes. A selection
        // made on screen must still control the physical pads. Preserve the
        // press mapping for note-off even if the mode changes while held.
        if (selection.software) action={...action,mode:selection.mode,slot:(action.slot&7)+Math.min(selection.page,1)*8,
          control:["hotcue","padFx","beatJumpPad","sampler","keyboard","padFx","beatLoopPad","keyShift"][selection.mode]};
        else if(selection.mode!==action.mode) selectPads(action.deck,action.mode,action.mode===4 || action.mode===7?1:action.slot>>3);
        this.padPresses.set(key,action);
      }
    }
    const { control, deck, value, pressed } = action;
    if (deck && ["padPage","jumpRangeDown","jumpRangeUp"].includes(control)) {
      if(!pressed) return;
      const selected=getPadSelection(deck), mode=selected.software?selected.mode:(action.mode??2);
      const right=control==="jumpRangeUp" || (control==="padPage" && value>0);
      if(mode===3 || action.shift) this.perform(async()=>{
        const state=await this.client.send("sampler.state") as {bank:number};
        return this.client.send("sampler.bank",{bank:clamp(state.bank+(right?1:-1),0,3)});
      });
      else selectPads(deck,mode,right?1:0,selected.software);
      this.actions().library({control:"padPage",deck,value:right?1:-1});
      return;
    }
    // Mode selection is available even on an unloaded deck. Keep software and
    // outgoing hardware state together instead of relying on a transient event.
    const mode = ({hotcueMode:0,fxMode:action.mode===5?5:1,jumpMode:2,sampler:3,keyboard:4,loopMode:6,keyShift:7} as Record<string,number>)[control];
    if (deck && mode !== undefined && action.slot === undefined) {
      if (pressed) {
        selectPads(deck, mode, mode===4 || mode===7 ? 1 : 0);
        if (mode===4) this.choosingKeyboardCue.add(deck);
        this.actions().activate(deck);
        this.actions().library({...action,control:mode===3?"samplerMode":mode===4?"keyboardMode":mode===7?"keyShiftMode":control});
      }
      return;
    }
    if (["browse", "back", "browseView", "related", "load", "previous", "next", "zoom"].includes(control)) {
      if (pressed === false || ["browse", "zoom"].includes(control) && value === 0) return;
      if (deck) this.actions().activate(deck);
      if (control === "load" && deck && this.client.getState().snapshot?.decks[deck].status === "playing") {
        this.actions().error(`DECK ${deck}は再生中です。停止してからロードしてください。`); return;
      }
      this.actions().library(action); return;
    }
    if (control === "samplerGain") { this.perform(() => this.client.send("sampler.gain", { gain: value })); return; }
    if (control === "samplerCue") { if (pressed) this.perform(async () => { const state = await this.client.send("sampler.state") as { pfl: boolean }; return this.client.send("sampler.pfl", { enabled: !state.pfl }); }); return; }
    if (control === "sampler") {
      if (pressed && action.slot !== undefined) this.perform(() => this.client.send(action.shift ? "sampler.stop" : "sampler.play", { slot: (action.slot! & 7) + (deck === "A" || deck === "C" ? 8 : 0) }));
      else if (pressed) this.actions().library({control: "samplerMode", deck, value});
      return;
    }
    if (deck && pressed) this.actions().activate(deck);
    if (deck && ["hotcueMode", "fxMode", "jumpMode", "loopMode"].includes(control)) {
      if (pressed) this.actions().library(action); return;
    }
    const snapshot = this.client.getState().snapshot;
    if (!snapshot || !this.client.getSessionId()) return;
    if (control === "crossfader") { this.perform(() => this.client.setCrossfader(value * 2 - 1)); return; }
    if (snapshot.engine?.capabilities?.includes("mixer.colorfx") && (control === "colorSelect" || /^filter[A-D]$/.test(control))) {
      if (control === "colorSelect") { if (!pressed) return; this.colorEffect=["echo","pitchshift","whitenoise","filter"][value]; }
      else this.colorValues[control.slice(-1) as DeckId]=value*2-1;
      for (const id of control === "colorSelect" ? DECKS : [control.slice(-1) as DeckId]) this.perform(()=>this.client.send("mixer.colorfx.set",{deck:id,effect:this.colorEffect,amount:this.colorValues[id]}));
      return;
    }
    if (control === "colorSelect" || control === "colorState") {
      if (control === "colorSelect" && !pressed) return;
      this.colorEnabled = value === 3 && (control === "colorSelect" || Boolean(pressed));
      if (value !== 3 && pressed) this.actions().error("SOUND COLOR FXはFILTERに対応しています。FILTERボタンを選んでください。");
      for (const id of DECKS) this.perform(() => this.client.setFilter(id, this.colorEnabled ? this.colorValues[id] : 0));
      return;
    }
    if (control.startsWith("filter") && control.length === 7) {
      const id = control.slice(-1) as DeckId; this.colorValues[id] = value * 2 - 1;
      this.perform(() => this.client.setFilter(id, this.colorEnabled ? this.colorValues[id] : 0)); return;
    }
    if (snapshot.engine?.capabilities?.includes("mixer.beatfx") && ["fxSelect", "fxAssign", "fxState", "fxToggle", "fxMix", "fxBeatDown", "fxBeatUp", "fxRelease", "fxAuto", "fxTap"].includes(control)) {
      if (control !== "fxMix" && control !== "fxState" && control !== "fxRelease" && !pressed) return;
      if (control === "fxRelease") {
        if (!snapshot.engine.capabilities.includes("mixer.beatfx.release")) {
          if(pressed) this.actions().error("RELEASE ECHOを使うにはplumdeckを再起動し、新しい音声エンジンを読み込んでください。");
          return;
        }
        this.releaseFxHeld=Boolean(pressed);
      }
      // The mix knob streams CC while it is turned. Keep one request in flight
      // and remember only the newest position, or the queue grows without bound
      // and the engine keeps chasing values the DJ has already turned past.
      if (control === "fxMix") {
        this.nativeFxMixLatest = value;
        if (this.nativeFxMixQueued) return;
        this.nativeFxMixQueued = true;
      }
      let tappedBpm: number | undefined;
      if (control === "fxTap") {
        const now=performance.now(), last=this.fxTapTimes[this.fxTapTimes.length-1];
        if(last===undefined || now-last>1500) this.fxTapTimes=[];
        else if(now-last<200) return;
        this.fxTapTimes.push(now); this.fxTapTimes=this.fxTapTimes.slice(-5);
        if(this.fxTapTimes.length<2) return;
        tappedBpm=60000*(this.fxTapTimes.length-1)/(now-this.fxTapTimes[0]);
      }
      if(control === "fxAuto") this.fxTapTimes=[];
      const session = this.client.getSessionId(), epoch = this.epoch;
      const next = (this.queues.get("nativeFx") ?? Promise.resolve()).then(async () => {
        if (session !== this.client.getSessionId() || epoch !== this.epoch) return;
        let params: Record<string,unknown> = {};
        if (control === "fxTap") params={bpm:tappedBpm};
        if (control === "fxAuto") params={auto:true};
        if (control === "fxSelect") {
          const effect = BEAT_FX[value]?.[0];
          if (!effect) {
            this.nativeFxSuspended ??= (this.client.getState().snapshot?.mixer.beatFx??this.nativeFx).enabled;
            await this.client.send("mixer.beatfx.set",{enabled:false});
            throw new Error("BEAT FXの選択番号が不正です。");
          }
          params={effect,...(this.nativeFxSuspended===null?{}:{enabled:this.nativeFxSuspended})};
          this.nativeFxSuspended=null;
        }
        if (control === "fxAssign") params={target:["A","B","C","D","master","mic","sampler"][value]};
        if (control === "fxMix") { const latest = this.nativeFxMixLatest ?? value; this.nativeFxMixLatest = null; params={mix:latest}; }
        if (control === "fxState") params={enabled:Boolean(pressed)};
        if (control === "fxToggle") params={toggle:true};
        if (this.nativeFxSuspended!==null && (control==="fxState" || control==="fxToggle")) {
          this.nativeFxSuspended=control==="fxToggle"?!this.nativeFxSuspended:Boolean(pressed);
          params={enabled:false};
        }
        if (control === "fxRelease") params={release:Boolean(pressed)};
        if (control === "fxBeatDown" || control === "fxBeatUp") {
          const current=this.client.getState().snapshot?.mixer.beatFx??this.nativeFx;
          params={beats:clamp(current.beats*(control === "fxBeatDown"?0.5:2),0.125,beatFxMaxBeats(current.effect))};
        }
        const result = await this.client.send("mixer.beatfx.set",params) as {beatFx?:BeatFxState};
        if(result?.beatFx) this.nativeFx={...result.beatFx};
      }).catch(e=>this.actions().error(String(e))).finally(() => {
        if (control === "fxMix") {
          this.nativeFxMixQueued = false;
          const pending = this.nativeFxMixLatest; this.nativeFxMixLatest = null;
          if (pending !== null && epoch === this.epoch && this.alive && session === this.client.getSessionId()) this.dispatch({ control: "fxMix", value: pending });
        }
      });
      this.queues.set("nativeFx",next); return;
    }
    if (["fxSelect", "fxAssign", "fxState", "fxToggle", "fxMix"].includes(control)) {
      if (control !== "fxMix" && control !== "fxState" && !pressed) return;
      if (control === "fxMix") {
        this.fxMixLatest = value;
        if (this.fxMixQueued) return;
        this.fxMixQueued = true;
      }
      const session = this.client.getSessionId();
      const tokens = Object.fromEntries(DECKS.map(id => [id, this.trackToken(id)]));
      const epoch = this.epoch;
      const next = (this.queues.get("fx") ?? Promise.resolve()).then(async () => {
        if (epoch !== this.epoch || !this.alive || session !== this.client.getSessionId()) return;
        const previousTargets = this.fxTarget === "master" ? DECKS : [this.fxTarget];
        if (["fxSelect", "fxAssign"].includes(control) && this.fxEnabled && this.fxEffect) {
          for (const id of previousTargets) if (this.valid(id, session, tokens[id])) await this.client.setFx(id, this.fxEffect, false, this.fxMix);
        }
        if (control === "fxSelect") {
          this.fxEffect = ({ 1: "echo", 4: "reverb", 7: "flanger", 8: "phaser" } as Record<number, PadEffect>)[value] ?? null;
          if (!this.fxEffect) this.actions().error("選択したBEAT FXは現在の音声エンジンでは未対応です。ECHO / REVERB / FLANGER / PHASERを選んでください。");
        }
        if (control === "fxAssign") {
          if (value > 4) { this.fxEnabled = false; this.actions().error("BEAT FXのMIC / SAMPLER割り当ては未対応です。"); return; }
          this.fxTarget = value === 4 ? "master" : DECKS[value];
        }
        if (control === "fxState") this.fxEnabled = Boolean(pressed);
        if (control === "fxToggle") this.fxEnabled = !this.fxEnabled;
        if (control === "fxMix") { this.fxMix = this.fxMixLatest ?? value; this.fxMixLatest = null; }
        if (this.fxEffect) for (const id of this.fxTarget === "master" ? DECKS : [this.fxTarget]) {
          if (this.valid(id, session, tokens[id]) && this.client.getState().snapshot?.decks[id].track) await this.client.setFx(id, this.fxEffect, this.fxEnabled, this.fxMix);
        }
      }).catch(e => this.actions().error(String(e))).finally(() => {
        if (control === "fxMix") {
          this.fxMixQueued = false;
          const pending = this.fxMixLatest; this.fxMixLatest = null;
          if (pending !== null && epoch === this.epoch && this.alive && session === this.client.getSessionId()) this.dispatch({ control: "fxMix", value: pending });
        }
      });
      this.queues.set("fx", next); return;
    }
    if (!deck) return;
    if (control === "gain") { this.perform(() => this.client.setChannelGain(deck, value)); return; }
    if (control === "trim") { this.perform(() => this.client.setTrim(deck, value * 2)); return; }
    if (control.startsWith("eq")) {
      const gain = value <= 0.5 ? value * 2 : 1 + (value - 0.5) * 6;
      this.perform(() => this.client.setEq(deck, control.slice(2).toLowerCase() as "low" | "mid" | "high", gain)); return;
    }
    if (control.startsWith("assign") && pressed) {
      this.perform(() => this.client.send("mixer.channel.orientation", { deck, orientation: control === "assignLeft" ? 0 : control === "assignRight" ? 2 : 1 })); return;
    }
    if (control === "vinylState" || control === "vinyl") {
      if (control === "vinylState") this.vinyl.set(deck, Boolean(pressed));
      else if (pressed) this.vinyl.set(deck, this.vinyl.get(deck) === false);
      return;
    }
    const current = snapshot.decks[deck];
    if (!current.track || ["loading", "error", "empty"].includes(current.status)) return;
    const track = this.trackToken(deck), session = this.client.getSessionId();
    if (["keyboard", "keyShift", "keySync", "keyReset", "slip", "slipReverse", "reverse"].includes(control)) {
      if (control !== "slipReverse" && !pressed) return;
      if (["keyShift", "keyboard"].includes(control) && action.slot === undefined) { if(control === "keyboard") this.choosingKeyboardCue.add(deck); this.actions().library({ control: control === "keyboard" ? "keyboardMode" : "keyShiftMode", deck, value }); return; }
      if (control === "slipReverse") { if(pressed) this.heldReverse.set(deck,{session,track}); else this.heldReverse.delete(deck); }
      if (control === "keyboard" && this.choosingKeyboardCue.has(deck)) { if(current.hotCues[action.slot!] == null) {this.actions().error("KEYBOARD用のホットキューを選択してください");return;} this.keyboardCues.set(deck,action.slot!);this.choosingKeyboardCue.delete(deck);return;}
      const semitones = keyPad(getPadSelection(deck).page, action.slot??0);
      if (["keyShift","keyboard"].includes(control) && semitones===null) return;
      const op = ["keyShift", "keyboard"].includes(control) ? "deck.key.shift" : control === "keySync" ? "deck.key.sync" : control === "keyReset" ? "deck.key.reset" : `deck.${control}.set`;
      const params = ["keyShift", "keyboard"].includes(control) ? { semitones } : { enabled: control === "slipReverse" ? Boolean(pressed) : !current[control as "slip" | "reverse"] };
      this.perform(async () => { await this.client.send(op, { deck, trackId: current.track!.trackId, ...params });
        if (control === "keyboard" && this.valid(deck,session,track)) { const cue = current.hotCues[this.keyboardCues.get(deck)??0]; if (cue == null) throw new Error("KEYBOARDにはホットキューを設定してください"); await this.client.seek(deck,cue); if(this.valid(deck,session,track)) await this.client.play(deck); }
      }); return;
    }
    if (control === "tempoRange" && pressed) {
      const ranges = [0.06, 0.1, 0.16, 0.75];
      this.ranges.set(deck, ranges[(ranges.indexOf(this.ranges.get(deck) ?? 0.16) + 1) % ranges.length]);
      this.actions().library({ control: "tempoRange", deck, value: this.ranges.get(deck)! * 100 }); return;
    }
    if (control === "tempo") {
      this.cancelBend(deck, false);
      this.perform(() => this.client.setTempo(deck, clamp(1 + (value * 2 - 1) * (this.ranges.get(deck) ?? 0.16), 0.25, 4))); return;
    }
    if (control === "touch" && this.held(this.loopAdjust, deck)) return;
    if (["loopInAdjust", "reloop"].includes(control) && pressed && current.loopRegion?.enabled && (control === "loopInAdjust" || action.shift)) {
      const target = control === "loopInAdjust" ? "in" : "out";
      if (this.held(this.loopAdjust, deck)?.target === target) this.loopAdjust.delete(deck); else { this.releaseJog(deck); this.loopAdjust.set(deck, { target, session, track }); }
      return;
    }
    if (control === "touch") {
      if (pressed && this.vinyl.get(deck) !== false) {
        const continuing = this.held(this.jogs, deck);
        if (continuing?.releaseTimer) {
          clearTimeout(continuing.releaseTimer); continuing.releaseTimer = undefined;
          return;
        }
        this.releaseJog(deck);
        this.cancelBend(deck, true);
        const now = performance.now();
        const jog = { id: crypto.randomUUID(), displacement: 0, session, track, last: now,
          speedTime: now, speedPosition: 0, speed: 0 };
        this.jogs.set(deck, jog); this.perform(() => this.client.scratch(deck, "begin", 0, jog.id));
      } else {
        const jog = this.held(this.jogs, deck);
        // A fast reverse spin can outlive the touch sensor. Follow actual
        // rotation packets for a short grace interval instead of switching
        // directly from reverse scratch to forward tempo bending.
        if (jog && jog.speed < -.75 && performance.now() - jog.last < 60) this.deferJogRelease(deck, jog);
        else this.releaseJog(deck);
      }
      return;
    }
    if (["jog", "searchJog", "nudge"].includes(control)) {
      if (!value) return;
      const adjust = this.held(this.loopAdjust, deck)?.target, loop = current.loopRegion;
      if (adjust && loop) {
        const start = adjust === "in" ? clamp(loop.startMs+value*this.jogSensitivity,0,loop.endMs-1) : loop.startMs;
        const end = adjust === "out" ? clamp(loop.endMs+value*this.jogSensitivity,start+1,current.track!.durationMs) : loop.endMs;
        this.perform(async () => { await this.client.setLoop(deck,start,end); if (this.valid(deck,session,track)) await this.client.enableLoop(deck,true); }); return;
      }
      // VINYL OFF is encoded in the platter CC itself, even if the software
      // missed the mode button. Do not turn it into a held-touch scratch.
      if (action.vinyl === false || control === "searchJog") this.releaseJog(deck);
      let jog = this.jogs.get(deck);
      if (jog?.releaseTimer && value > 0) { this.releaseJog(deck); jog = undefined; }
      if ((control === "jog" || control === "nudge") && jog && this.valid(deck, jog.session, jog.track)) {
        jog.displacement = clamp(jog.displacement + value * this.jogSensitivity, -60_000, 60_000);
        jog.last = performance.now();
        const elapsed = jog.last - jog.speedTime;
        if (elapsed >= 8) {
          jog.speed = elapsed > 120 ? 0 : (jog.displacement - jog.speedPosition) / elapsed;
          jog.speedTime = jog.last; jog.speedPosition = jog.displacement;
        }
        if (jog.releaseTimer) this.deferJogRelease(deck, jog);
        this.perform(() => this.client.scratch(deck, "move", jog.displacement, jog.id));
      } else if (control === "searchJog" || current.status !== "playing") {
        const preview = this.seekPreview.get(deck), now = performance.now();
        const origin = preview && now - preview.at < 200 && this.valid(deck, preview.session, preview.track) ? preview.position : current.positionMs;
        const position = clamp(origin + value * (control === "searchJog" ? 100 : this.jogSensitivity), -60_000, current.track!.durationMs);
        this.seekPreview.set(deck, { position, at: now, session, track });
        this.perform(() => this.client.seek(deck, position));
      } else {
        if (this.client.getState().snapshot?.engine.capabilities.includes("deck.pitchbend")) {
          this.perform(() => this.client.pitchbend(deck, clamp(value * 0.002, -0.75, 0.75)));
          return;
        }
        const base = this.bends.get(deck)?.rate ?? current.rate;
        this.cancelBend(deck, false);
        this.perform(() => this.client.setTempo(deck, clamp(base + value * 0.002, 0.25, 4)));
        const timer = setTimeout(() => this.cancelBend(deck, true), 120);
        this.bends.set(deck, { rate: base, timer, session, track });
      }
      return;
    }
    this.seekPreview.delete(deck);
    if (pressed === false && control !== "cue" && control !== "padFx") return;
    const epoch = this.epoch;
    const key = deck;
    const next = (this.queues.get(key) ?? Promise.resolve()).then(async () => {
      if (!this.alive || epoch !== this.epoch || !this.valid(deck, session, track)) return;
      const d = this.client.getState().snapshot!.decks[deck];
      switch (control) {
        case "play":
          { const cue = this.cuePreview.get(deck); this.cuePreview.delete(deck);
            if (cue && this.valid(deck, cue.session, cue.track)) return; }
          await (d.status === "playing" ? this.client.pause(deck) : this.client.play(deck)); break;
        case "cue":
          if (pressed) {
            if (d.status === "playing") { await this.client.pause(deck); if (this.valid(deck, session, track)) await this.client.seek(deck, this.actions().cuePoints[deck]); }
            else if (Math.abs(d.positionMs - this.actions().cuePoints[deck]) > 20) this.actions().cuePoints[deck] = d.positionMs;
            else { this.cuePreview.set(deck, { session, track }); await this.client.play(deck); }
          } else {
            const cue = this.cuePreview.get(deck); this.cuePreview.delete(deck);
            if (cue && this.valid(deck, cue.session, cue.track)) { await this.client.pause(deck); if (this.valid(deck, session, track)) await this.client.seek(deck, this.actions().cuePoints[deck]); }
          }
          break;
        case "start": await this.client.seek(deck, 0); break;
        case "keylock": await this.client.setKeylock(deck, !d.keylock); break;
        case "quantize": await this.client.setQuantize(deck, !d.quantize); break;
        case "sync": {
          const leader = DECKS.find(id => id !== deck && this.client.getState().snapshot?.decks[id].status === "playing");
          await this.client.setSync(deck, !d.syncEnabled, leader); break;
        }
        case "master":
          // Leading is a role inside the sync group, not the absence of sync.
          // Clearing this deck's own sync would hand the tempo to whichever deck
          // the engine elects next, which is what SHIFT+SYNC is meant to prevent.
          await this.client.setSync(deck, true, deck);
          for (const id of DECKS) if (id !== deck && this.client.getState().snapshot?.decks[id].syncEnabled) await this.client.setSync(id, true, deck);
          break;
        case "pfl": await this.client.setPfl(deck, !this.client.getState().snapshot!.mixer.channels[deck].pfl); break;
        case "hotcue":
          if (action.slot !== undefined && action.slot < 16) await this.actions().hotcue(deck, action.slot, Boolean(action.shift));
          break;
        case "memorySave": case "memoryDelete":
          await this.actions().memory?.(deck, control === "memorySave" ? "save" : "delete"); break;
        case "faderStart": await this.client.play(deck); break;
        case "faderStop": await this.client.pause(deck); if (this.valid(deck,session,track)) await this.client.seek(deck,this.actions().cuePoints[deck]); break;
        case "loopIn":
          if (d.loopRegion?.enabled) { await this.client.setLoop(deck,d.loopRegion.startMs,d.loopRegion.startMs+(d.loopRegion.endMs-d.loopRegion.startMs)/2); if (this.valid(deck,session,track)) await this.client.enableLoop(deck,true); }
          else this.loopIn.set(deck, { positionMs: d.positionMs, session, track }); break;
        case "loopOut": {
          if (d.loopRegion?.enabled) { await this.client.setLoop(deck,d.loopRegion.startMs,Math.min(d.track!.durationMs,d.loopRegion.startMs+(d.loopRegion.endMs-d.loopRegion.startMs)*2)); if (this.valid(deck,session,track)) await this.client.enableLoop(deck,true); break; }
          const start = this.held(this.loopIn, deck)?.positionMs;
          if (start !== undefined && d.positionMs > start) { await this.client.setLoop(deck, start, d.positionMs); if (this.valid(deck, session, track)) await this.client.enableLoop(deck, true); this.loopIn.delete(deck); }
          break;
        }
        case "loop": await (d.loopRegion?.enabled ? this.client.enableLoop(deck, false) : this.client.beatLoop(deck, 4)); break;
        case "reloop": if (d.loopRegion) await this.client.enableLoop(deck, !d.loopRegion.enabled); break;
        case "loopHalf": case "loopDouble":
          if (d.loopRegion) {
            const { startMs, endMs } = d.loopRegion;
            const end = Math.min(d.track!.durationMs, startMs + (endMs - startMs) * (control === "loopHalf" ? 0.5 : 2));
            if (end > startMs) await this.client.setLoop(deck, startMs, end);
          } break;
        case "jumpRangeDown": case "jumpRangeUp": break; // Absolute page selection is handled before track gating.
        case "beatJumpPad": await this.client.beatJump(deck, BEAT_JUMP_PAGES[(action.slot!>>3)&1][action.slot!&7]); break;
        case "beatLoopPad": await this.client.beatLoop(deck, BEAT_LOOP_PAGES[(action.slot! >> 3) & 1][action.slot! & 7]); break;
        case "padFx": {
          const preset = padPreset(action.mode, action.slot);
          const effect = preset.effect;
          if (pressed) this.heldFx.set(deck, { effect, session, track });
          else if (this.heldFx.get(deck)?.effect !== effect || !this.valid(deck, this.heldFx.get(deck)!.session, this.heldFx.get(deck)!.track)) break;
          else this.heldFx.delete(deck);
          await this.client.setFx(deck, effect, Boolean(pressed), preset.mix, preset.depth); break;
        }
        case "cuePrevious": case "cueNext": {
          await this.actions().memory?.(deck, control === "cueNext" ? "next" : "previous"); break;
        }
        case "searchBack": case "searchForward": await this.client.seek(deck, clamp(d.positionMs + (control === "searchBack" ? -5000 : 5000), 0, d.track!.durationMs)); break;
      }
      if (this.valid(deck, session, track)) await this.client.refreshSnapshot();
    }).catch(e => this.actions().error(e instanceof Error ? e.message : String(e)));
    this.queues.set(key, next);
  }
  private releaseJog(deck: DeckId) {
    const jog = this.jogs.get(deck); this.jogs.delete(deck);
    if (jog?.releaseTimer) clearTimeout(jog.releaseTimer);
    if (jog && this.valid(deck, jog.session, jog.track)) this.perform(() => this.client.scratch(deck, "end", jog.displacement, jog.id));
  }
  private deferJogRelease(deck: DeckId, jog: Jog) {
    if (jog.releaseTimer) clearTimeout(jog.releaseTimer);
    jog.releaseTimer = setTimeout(() => {
      if (this.jogs.get(deck) === jog) this.releaseJog(deck);
    }, 60);
  }
  private cancelBend(deck: DeckId, restore: boolean) {
    const bend = this.bends.get(deck); if (!bend) return;
    clearTimeout(bend.timer); this.bends.delete(deck);
    if (restore && this.valid(deck, bend.session, bend.track)) this.perform(() => this.client.setTempo(deck, bend.rate));
  }
  reset() {
    this.pickup.clear();
    if (this.releaseFxHeld) {
      this.releaseFxHeld=false;
      const session=this.client.getSessionId(), leaseKey = junctionLeaseKey();
      const release=(this.queues.get("nativeFx")??Promise.resolve()).then(()=>{
        if(session===this.client.getSessionId() && leaseKey === junctionLeaseKey()) return this.client.send("mixer.beatfx.set",{release:false});
      }).catch(e=>this.actions().error(String(e)));
      this.queues.set("nativeFx",release);
    }
    this.epoch++; this.fxMixLatest = null; this.nativeFxMixLatest = null;
    this.nativeFxSuspended=null;
    this.fxTapTimes=[];
    this.padPresses.clear();
    for (const deck of DECKS) {
      this.releaseJog(deck); this.cancelBend(deck, true);
      const reverse=this.heldReverse.get(deck);this.heldReverse.delete(deck);
      if(reverse && this.valid(deck,reverse.session,reverse.track)) this.perform(()=>this.client.send("deck.slipReverse.set",{deck,enabled:false}));
      const cue = this.cuePreview.get(deck); this.cuePreview.delete(deck);
      const fx = this.heldFx.get(deck); this.heldFx.delete(deck);
      // Wait for an in-flight press before releasing it, including on teardown.
      const release = (this.queues.get(deck) ?? Promise.resolve()).then(async () => {
        if (cue && this.valid(deck, cue.session, cue.track)) { await this.client.pause(deck); if (this.valid(deck, cue.session, cue.track)) await this.client.seek(deck, this.actions().cuePoints[deck]); }
        if (fx && this.valid(deck, fx.session, fx.track)) await this.client.setFx(deck, fx.effect, false, 0.5);
      }).catch(e => this.actions().error(String(e)));
      this.queues.set(deck, release);
    }
    this.seekPreview.clear(); this.loopIn.clear(); this.loopAdjust.clear(); this.keyPages.clear(); this.keyboardCues.clear(); this.choosingKeyboardCue.clear(); this.jumps.clear(); this.vinyl.clear();
  }
  dispose() { this.unsubscribeJunction(); this.unsubscribePads(); this.reset(); this.alive = false; clearInterval(this.heartbeat); if (this.junctionTimer) clearTimeout(this.junctionTimer); this.junctionTimer = null; }
}
