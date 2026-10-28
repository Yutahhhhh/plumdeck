import test from "node:test";
import assert from "node:assert/strict";
import {BEAT_JUMP_PAGES,BEAT_LOOP_PAGES,beatJumpPages,keyPad,padPreset} from "../../../src/services/midi/pad-presets.ts";
import {getPadSelection,PAD_MODE_LAST_PAGE,selectPads} from "../../../src/services/midi/pad-state.ts";
import {coloredPadFeedback,padColor,padColorCode} from "../../../src/services/midi/pad-colors.ts";
import {jogSetting} from "../../../src/services/midi/jog-settings.ts";
import type {EngineSnapshot} from "../../../src/types/dj-engine.ts";

test("large persisted jog settings are migrated, not just the fallback default",()=>{
  for(const saved of [0.5,1.8,3,10]) assert.equal(jogSetting(saved,"2"),0.1);
  assert.equal(jogSetting(0.01,"2"),0.01);
  assert.equal(jogSetting(0.005,"3"),0.005);
  assert.equal(jogSetting(0.05,"3"),0.1);
  assert.equal(jogSetting(0.05,"4"),0.05);
  assert.equal(jogSetting(0.15,"3"),0.15);
  assert.equal(jogSetting(10,"3"),0.25);
  assert.equal(jogSetting(NaN,null),0.1);
});
test("each supported pad assignment exposes two distinct pages of eight slots",()=>{
  assert.equal(BEAT_JUMP_PAGES.length,2);assert.equal(BEAT_LOOP_PAGES.length,2);
  for(const pages of [BEAT_JUMP_PAGES,BEAT_LOOP_PAGES]) {
    assert.ok(pages.every(page=>page.length===8));assert.notDeepEqual(pages[0],pages[1]);
    assert.ok(pages.flat().every(value=>Math.abs(value)<=64));
  }
  for(const mode of [1,5]) assert.notDeepEqual(Array.from({length:8},(_,i)=>padPreset(mode,i)),Array.from({length:8},(_,i)=>padPreset(mode,i+8)));
  assert.notDeepEqual(Array.from({length:8},(_,i)=>keyPad(0,i)),Array.from({length:8},(_,i)=>keyPad(1,i)));
  for(let mode=0;mode<8;mode++) { selectPads("A",mode,9);assert.equal(getPadSelection("A").page,PAD_MODE_LAST_PAGE[mode]); }
  // KEY SHIFT covers -12..+12 over four pages; BEAT JUMP adds FINE and 128 on hosts with range v2.
  const keys=[0,1,2,3].flatMap(page=>Array.from({length:8},(_,i)=>keyPad(page,i))).filter(v=>v!==null).sort((a,b)=>a!-b!);
  assert.deepEqual(keys,Array.from({length:25},(_,i)=>i-12));
  assert.equal(beatJumpPages([]).length,2);assert.deepEqual(beatJumpPages(["deck.loop.range.v2"])[2],[-1/8,1/8,-1/4,1/4,-1/2,1/2,-128,128]);
  selectPads("A",0);
});
test("pad output uses explicit color indices, propagates saved cue colors and deduplicates addresses",()=>{
  const snapshot={decks:{A:{track:{trackId:"1"},hotCues:[1000,null,...Array(14).fill(null)]}}} as unknown as EngineSnapshot;
  selectPads("A",0);
  const out=coloredPadFeedback([[0x97,0,127],[0x97,0,127],[0x97,1,127]],snapshot,{A:["#ff435a"]});
  assert.deepEqual(out,[[0x97,0,41],[0x98,0,41],[0x97,1,0],[0x98,1,0]]);
  selectPads("A",5,1,true);
  const mapped=coloredPadFeedback([[0x97,1,0]],snapshot);
  assert.equal(mapped[0][2],padColorCode(padColor(5,9)),"an empty firmware hot-cue slot must still light the software-selected FX");
  selectPads("A",0);
});
