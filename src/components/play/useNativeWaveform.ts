import { tileWorker } from '@/services/waveform/worker-client';
import { useEffect, useState, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { djEngineClient } from '@/services/dj-engine/client';
import { chooseLod, type WaveformTile } from '@/services/waveform/protocol';
import { normalizeWaveformManifest, waveformRepository, type WaveformManifest } from '@/services/waveform/repository';
import type { DeckId } from '@/types/dj-engine';
const EMPTY_TILES:WaveformTile[]=[];
type WaveformBinding = {session:string;generation:number};
export function useNativeWaveform(deck:string|undefined,positionMs:number,spanMs:number,overview:boolean,physicalPixels=2000){
  const [binding,setBinding]=useState<WaveformBinding|null>(null);
  const [manifest,setManifest]=useState<WaveformManifest|null>(null);
  const [pcm,setPcm]=useState<{asset:string;session:string;generation:number;tile:WaveformTile}|null>(null);
  const asset=useRef<string|null>(null);
  const [error,setError]=useState<string|null>(null);
  const [reload,setReload]=useState(0);
  const [retry,setRetry]=useState(0);
  const [loaded,setLoaded]=useState<{key:string;tiles:WaveformTile[]} |null>(null);
  useEffect(()=>{
    const update=()=>{
      const state=djEngineClient.getState().snapshot;
      const slot=deck&&['A','B','C','D'].includes(deck)?state?.decks[deck as DeckId]:null;
      const session=state?.engine.capabilities.includes('waveform.tiles.v2') ? state.sessionId : null;
      const next:WaveformBinding|null=session&&slot?.loadGeneration&&slot.track?{session,generation:slot.loadGeneration}:null;
      setBinding(old=>old?.session===next?.session&&old?.generation===next?.generation?old:next);
    };update();return djEngineClient.subscribe(update);
  },[deck]);
  useEffect(()=>{
    setError(null);setManifest(null);setLoaded(null);if(!binding||!deck)return;
    let live=true,timer:ReturnType<typeof setTimeout>;
    const read=async()=>{
      try{
        const source={deck,loadGeneration:binding.generation};
        const result=await djEngineClient.send('waveform.ensure',source) as {assetKey?:string;state?:string};
        if(result.assetKey){
          asset.current=result.assetKey;
          if(result.state==='error'){if(live)setError('波形を読み込めませんでした');return;}
          const m=normalizeWaveformManifest(await invoke<unknown>('dj_waveform_manifest',{sessionId:binding.session,assetKey:result.assetKey}));
          if(!m)throw new Error('Invalid waveform manifest');
          if(live&&m.state==='error'){setError('波形を読み込めませんでした');return;}
          if(live&&m.schemaVersion===2&&m.assetKey===result.assetKey&&Array.isArray(m.levels)){setManifest(m);if(m.state==='ready'||m.state==='error')return;}
        }
      }catch{/* Partial publication and legacy hosts are not repeated toasts. */}
      if(live)timer=setTimeout(()=>void read(),250);
    };void read();return()=>{live=false;clearTimeout(timer);};
  },[binding,deck,reload]);
  const rate=manifest?.sourceSampleRateHz??44100;
  const spanFrames=overview?(manifest?.sourceFrameCount??0):spanMs*rate/1000;
  const lod=chooseLod(spanFrames/Math.max(1,physicalPixels));
  const framesPerTile=64*2**lod*2048;
  const first=overview?0:Math.max(0,Math.floor((positionMs*rate/1000-(manifest?.sourceFrameOrigin??0)-spanFrames/2)/framesPerTile));
  const last=overview?Math.ceil(spanFrames/framesPerTile):Math.ceil((positionMs*rate/1000-(manifest?.sourceFrameOrigin??0)+spanFrames/2)/framesPerTile);
  const key=`${binding?.session}:${manifest?.assetKey}:${lod}:${first}:${last}`;
  useEffect(()=>{
    if(!binding||!manifest)return;
    const level=manifest.levels.find(level=>level.lod===lod);if(!level)return;
    const handles:ReturnType<typeof waveformRepository.acquire>[]=[];let live=true;let timer:ReturnType<typeof setTimeout>;
    for(let tile=first;tile<last&&handles.length<8;tile++){
      if(!level.readyTileRanges.some(([from,to])=>tile>=from&&tile<to))continue;
      handles.push(waveformRepository.acquire(binding.session,manifest.assetKey,lod,tile,1,manifest.sourceFrameOrigin));
    }
    void Promise.all(handles.map(h=>h.promise)).then(tiles=>{if(live)setLoaded({key,tiles});}).catch(()=>{if(live)timer=setTimeout(()=>setRetry(value=>value+1),1000);});
    return()=>{live=false;clearTimeout(timer);handles.forEach(handle=>handle.release());};
  },[key,manifest?.revision,retry]);
  const framesPerPixel=spanFrames/Math.max(1,physicalPixels);
  const pcmSpan=Math.max(Math.ceil(spanFrames*1.5)+2,Math.min(8192,Math.ceil(rate*.12)));
  const pcmStep=Math.max(1,Math.floor(pcmSpan/6));
  const pcmStart=Math.max(0,Math.floor((positionMs*rate/1000-pcmSpan/2)/pcmStep)*pcmStep-1);
  const pcmBin=Math.max(1,Math.floor(framesPerPixel),Math.ceil(pcmSpan/8192));
  useEffect(()=>{
    if(!binding||!manifest||!deck||overview||framesPerPixel>=64||pcmSpan>262144)return;
    let live=true,timer:ReturnType<typeof setTimeout>;
    const requestId=`${deck}:${binding.generation}:${pcmStart}:${pcmSpan}`;
    const read=async()=>{
      try{
        const source={deck,loadGeneration:binding.generation};
        const reply=await djEngineClient.send('waveform.requestRange',{...source,assetKey:manifest.assetKey,requestId,startSourceFrame:pcmStart,endSourceFrame:pcmStart+pcmSpan,detail:'pcm',priority:0}) as {state?:string;windowId?:string};
        if(reply.state==='ready'&&reply.windowId){
          const bytes=await invoke<ArrayBuffer>('dj_waveform_pcm',{sessionId:binding.session,assetKey:manifest.assetKey,windowId:reply.windowId});
          const tile=await tileWorker.parse(bytes,pcmBin);
          if(tile.startFrame!==pcmStart||tile.coveredFrames!==pcmSpan||tile.sampleRate!==rate)throw new Error('PCM range mismatch');
          if(live)setPcm({asset:manifest.assetKey,session:binding.session,generation:binding.generation,tile});return;
        }
      }catch{ /* Keep the valid coarse tiles while the window is unavailable. */ }
      if(live)timer=setTimeout(()=>void read(),30);
    };void read();return()=>{live=false;clearTimeout(timer);void djEngineClient.send('waveform.cancelRequest',{requestId}).catch(()=>{});};
  },[binding,manifest?.assetKey,deck,overview,pcmStart,pcmSpan,pcmBin]);
  const fine=pcm&&pcm.asset===manifest?.assetKey&&pcm.session===binding?.session&&pcm.generation===binding?.generation&&framesPerPixel<64&&!overview&&pcm.tile.startFrame<=Math.max(0,positionMs*rate/1000-spanFrames/2)&&pcm.tile.startFrame+pcm.tile.coveredFrames>=positionMs*rate/1000+spanFrames/2;
  return {manifest,error,retry:()=>{if(asset.current)void djEngineClient.send("waveform.invalidate",{assetKey:asset.current}).catch(()=>{});setReload(value=>value+1);},tiles:fine?[pcm.tile]:loaded?.key.startsWith(`${binding?.session}:${manifest?.assetKey}:`)?loaded.tiles:EMPTY_TILES,enabled:!!binding};
}
