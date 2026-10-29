import { padPreset } from "./pad-presets.ts";
import { getPadSelection } from "./pad-state.ts";
import type { DeckId, EngineSnapshot } from "../../types/dj-engine";
// DDJ color indices documented by Mixxx's Pioneer DDJ hardware investigation:
// https://github.com/mixxxdj/mixxx/wiki/Pioneer-Ddj-Controllers#ddj-pad-colors
// RGB swatches approximate those named colors; they are not LED measurements.
const PALETTE = [
  ["#448aff",1], ["#40cfff",7], ["#00df80",17], ["#ffe052",29],
  ["#ff923a",36], ["#ff435a",41], ["#ed64d5",52], ["#a878ff",60], ["#ffffff",64],
] as const;
export const CUE_COLORS = ["#00df80","#ffe052","#ff923a","#ff435a","#ed64d5","#a878ff","#448aff","#40cfff"];
// PAD FX colour by effect family: delays blue, rolls/gates green, space violet,
// filters yellow, modulation magenta/cyan, dirt orange/red, noise white.
const FX_COLORS:Record<string,string>={echo:"#448aff",lowcutecho:"#448aff",mtdelay:"#448aff",spiral:"#40cfff",reverb:"#a878ff",flanger:"#40cfff",enigmajet:"#40cfff",phaser:"#ed64d5",
  filter:"#ffe052",moogladder4filter:"#ffe052",autopan:"#00df80",tremolo:"#00df80",roll:"#00df80",sliproll:"#00df80",bitcrusher:"#ff923a",distortion:"#ff435a",pitchshift:"#ed64d5",whitenoise:"#ffffff"};
/** Hot cue colour: the cue's own colour, else the slot's default. The one
 * source for pads, waveform markers and controller LEDs. */
export function hotCueColor(slot:number,custom?:string|null):string { return custom && /^#[0-9a-f]{6}$/i.test(custom)?custom:CUE_COLORS[slot&7]; }
export function padColor(mode:number,slot:number,custom?:string|null,capabilities:readonly string[]=[]):string {
  if(mode===0) return hotCueColor(slot,custom);
  if(mode===1 || mode===5) return FX_COLORS[padPreset(mode,slot,capabilities).effect] ?? "#448aff";
  return mode===3?"#ed64d5":mode===6?"#ff923a":mode===2?"#448aff":mode===4?"#00df80":"#40cfff";
}
export function padColorCode(color:string):number {
  const rgb=(hex:string)=>[1,3,5].map(at=>parseInt(hex.slice(at,at+2),16));
  const input=rgb(/^#[0-9a-f]{6}$/i.test(color)?color:"#ffffff");
  return [...PALETTE].sort((a,b)=>{
    const distance=(hex:string)=>rgb(hex).reduce((sum,v,i)=>sum+(v-input[i])**2,0);
    return distance(a[0])-distance(b[0]);
  })[0][1];
}
export function coloredPadFeedback(messages:number[][], snapshot:EngineSnapshot|null,
  colors?:Partial<Record<DeckId,(string|null)[]>>, sampler?:{slots:{status:string}[]}):number[][] {
  const result=new Map<string,number[]>();
  for(const message of messages) {
    const [status,note]=message;
    if(status<0x97 || status>0x9e) {result.set(`${status}:${note}`,message);continue;}
    const ch=Math.floor((status-0x97)/2),deck=(["A","B","C","D"] as const)[ch],selected=getPadSelection(deck);
    const mode=selected.software?selected.mode:note>>4,slot=selected.software?selected.page*8+(note&7):note&15;
    const state=snapshot?.decks[deck];
    const on=mode===0?state?.hotCues[slot]!=null:mode===3?["ready","playing"].includes(sampler?.slots[(slot&7)+(ch%2===0?8:0)]?.status??""):Boolean(state?.track);
    const value=on?padColorCode(padColor(mode,slot,colors?.[deck]?.[slot],snapshot?.engine?.capabilities)):0;
    // Include the SHIFT pad channel. One canonical value per address prevents
    // core/page feedback duplicates from repeatedly repainting the same pad.
    for(const output of [0x97+ch*2,0x98+ch*2]) result.set(`${output}:${note}`,[output,note,value]);
  }
  return [...result.values()];
}
