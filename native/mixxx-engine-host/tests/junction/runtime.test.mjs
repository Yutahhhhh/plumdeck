import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {mkdtemp, mkdir, writeFile, readFile, rm} from 'node:fs/promises';
import path from 'node:path';
import test from 'node:test';
import {createJunctionServer, listen} from '../../../../services/junction-signaling/dist/src/server.js';
import {createLogger} from '../../../../services/junction-signaling/dist/src/logger.js';

const binary = process.env.PLUMDECK_TEST_HOST || path.resolve(import.meta.dirname,'../../build-upstream/plumdeck-mixxx-engine-host');
const pause = ms => new Promise(resolve=>setTimeout(resolve,ms));
async function until(read,predicate,label,timeout=20000) {
  const end=Date.now()+timeout;let lastDiagnostic='';
  while(Date.now()<end) {
    const value=await read();
    const diagnostic=JSON.stringify({active:value.active,phase:value.handoffState,connection:value.connection,readiness:value.readiness});
    if(diagnostic!==lastDiagnostic){console.info(label,diagnostic);lastDiagnostic=diagnostic;}
    if(predicate(value))return value;
    if((label==='guest epoch activation'||label==='host return epoch activation'||label==='guest to guest epoch')&&value.handoffState==='playing')throw new Error('Handoff aborted before epoch activation');
    await pause(80);
  }
  throw new Error(`${label} did not settle within ${timeout} ms`);
}
function native(directory) {
  const child=spawn(binary,[],{env:{...process.env,PLUMDECK_MIXXX_OUTPUT_DEVICE:process.env.PLUMDECK_MIXXX_OUTPUT_DEVICE || 'BlackHole 2ch',PLUMDECK_MIXXX_RECORDING_DIR:directory}});
  const pending=new Map();let id=0,hello,stderr='';
  child.stderr.on('data',data=>{stderr=(stderr+data).slice(-12000);if(process.env.PLUMDECK_JUNCTION_TRACE){for(const line of data.toString().split('\n'))if(line.startsWith('junction '))console.info(path.basename(directory),line);}});
  const rejectAll=error=>{for(const entry of pending.values()){clearTimeout(entry.timer);entry.reject(error);}pending.clear();};
  child.on('error',rejectAll);child.on('exit',code=>rejectAll(new Error(`Native engine exited (${code})`)));
  createInterface({input:child.stdout}).on('line',line=>{
    let value;try{value=JSON.parse(line);}catch{return;}
    const entry=pending.get(value.id);if(!entry)return;
    pending.delete(value.id);clearTimeout(entry.timer);entry.resolve(value);
  });
  async function raw(op,params={}) {
    const key=++id;
    const result=new Promise((resolve,reject)=>{
      // Two full Mixxx engines initialise their audio graphs concurrently in this
      // test. Cold plugin/device discovery can exceed 12 seconds on otherwise
      // healthy machines, so keep command I/O bounded without mistaking startup
      // work for a Junction deadlock.
      const timer=setTimeout(()=>{pending.delete(key);reject(new Error(`Native ${op} timed out: ${stderr.replace(/plumdeck-junction:\/\/\S+/g,'[invite redacted]')}`));},30000);
      pending.set(key,{resolve,reject,timer});
    });
    child.stdin.write(JSON.stringify({id:key,op,params,...(hello?{sessionId:hello.sessionId,engineId:hello.engineId}:{})})+'\n');
    return result;
  }
  async function command(op,params={}) {const reply=await raw(op,params);assert.notEqual(reply.kind,'error',`${op}: ${reply.error?.message ?? 'native error'}`);assert.notEqual(reply.ok,false,`${op}: ${reply.error?.message ?? 'native error'}`);return reply.data ?? reply;}
  return {raw,command,suspend:()=>child.kill('SIGSTOP'),resume:()=>child.kill('SIGCONT'),diagnostics:()=>stderr.replace(/plumdeck-junction:\/\/\S+/g,'[invite redacted]'),async start(){hello=await command('session.hello');assert.equal(hello.engine.implementation,'mixxx');await until(()=>command('state.snapshot'),s=>s.audio.applied,'local audio');},async close(){
    child.kill('SIGCONT');child.stdin.end();if(child.exitCode!==null)return;
    await Promise.race([new Promise(resolve=>child.once('exit',resolve)),pause(3000)]);
    if(child.exitCode===null){child.kill('SIGKILL');await new Promise(resolve=>child.once('exit',resolve));}
    assert.equal(child.exitCode,0,`Native exits cleanly after session close: ${child.signalCode ?? 'exit'}; ${stderr.slice(-1200)}`);
  }};
}
function tone(seconds=240,frequency=440) {
  const frames=48000*seconds,data=Buffer.alloc(44+frames*4);
  data.write('RIFF');data.writeUInt32LE(data.length-8,4);data.write('WAVEfmt ',8);data.writeUInt32LE(16,16);data.writeUInt16LE(1,20);data.writeUInt16LE(2,22);data.writeUInt32LE(48000,24);data.writeUInt32LE(192000,28);data.writeUInt16LE(4,32);data.writeUInt16LE(16,34);data.write('data',36);data.writeUInt32LE(frames*4,40);
  for(let n=0;n<frames;n++){const value=Math.round(Math.sin(n*2*Math.PI*frequency/48000)*8000+Math.sin(n*2*Math.PI*733.37/48000)*2800+Math.sin(n*2*Math.PI*(113+n/48000*.013)/48000)*1000);data.writeInt16LE(value,44+n*4);data.writeInt16LE(value,46+n*4);}return data;
}
function assertProgramAudio(wav,continuous=false) {
  assert.equal(wav.toString('ascii',0,4),'RIFF');assert.equal(wav.readUInt32LE(4)+8,wav.length,'recording finalizes RIFF size');
  let pcm,bits,channels,rate;
  for(let pos=12;pos+8<=wav.length;){const size=wav.readUInt32LE(pos+4),tag=wav.toString('ascii',pos,pos+4);if(tag==='fmt '){channels=wav.readUInt16LE(pos+10);rate=wav.readUInt32LE(pos+12);bits=wav.readUInt16LE(pos+22);}if(tag==='data')pcm=wav.subarray(pos+8,pos+8+size);pos+=8+size+(size%2);}
  assert.equal(channels,2);assert.equal(bits,24);assert(pcm?.length>4800,'Program has actual recorded frames');
  let energy=0;for(let i=0;i+3<=pcm.length;i+=3){const value=pcm.readIntLE(i,3)/8388608;energy+=value*value;}
  assert(Math.sqrt(energy/(pcm.length/3))>.005,'Program recording contains non-silent PCM from the adopted performance');
  let sine=0,cosine=0;const frames=Math.floor(pcm.length/6);
  for(let n=0;n<frames;n++){const value=pcm.readIntLE(n*6,3)/8388608,angle=2*Math.PI*440*n/rate;sine+=value*Math.sin(angle);cosine+=value*Math.cos(angle);}
  assert(Math.hypot(sine,cosine)/frames>.005,'Program contains the adopted 440 Hz source rather than unrelated device audio');
  if(continuous){let silence=0,longest=0;for(let n=0;n<frames;n++){const value=Math.abs(pcm.readIntLE(n*6,3)/8388608);silence=value<.0001?silence+1:0;longest=Math.max(longest,silence);}
    assert(longest<=rate*.005,`Program handoff introduces ${longest/rate*1000} ms of silence (limit5ms)`);
  }
}

test('two real native peers admit explicitly, turn over by fader start with continuous actual Program PCM, and recover',{timeout:180000},async()=>{
  const directory=await mkdtemp('/tmp/plumdeck-junction-runtime-');
  const turn = process.env.JUNCTION_TURN_ADDRESS ? {
    turnUrls:[`${process.env.JUNCTION_TURN_TLS ? 'turns' : 'turn'}:${process.env.JUNCTION_TURN_ADDRESS}`],
    turnSecret:(await readFile(process.env.JUNCTION_TURN_SECRET_FILE,'utf8')).trim(),turnTtlSeconds:Number(process.env.PLUMDECK_JUNCTION_TEST_TURN_TTL||600),
  } : {};
  const createServer=()=>createJunctionServer({...turn,port:0,host:'127.0.0.1',roomTtlSeconds:3600,maxPeersPerRoom:8,maxFrameBytes:65536,relayRatePerSecond:100,frameRatePerSecond:300,joinAttemptsPerMinute:20,allowedOrigins:null,trustProxy:false,logLevel:'error'},createLogger({level:'error',write:()=>{}}));
  let server=createServer();
  let host,guest,third;
  try {
    await listen(server,0,'127.0.0.1');const signalingUrl=`ws://127.0.0.1:${server.address().port}`;
    await mkdir(path.join(directory,'host'));await mkdir(path.join(directory,'guest'));
    host=native(path.join(directory,'host'));guest=native(path.join(directory,'guest'));
    await Promise.all([host.start(),guest.start()]);
    const devices=await host.command('audio.devices.list');const output=devices.devices.find(d=>d.name===(process.env.PLUMDECK_MIXXX_OUTPUT_DEVICE || 'BlackHole 2ch')&&d.outputChannels>=2);
    assert(output,'Install/select an actual stereo loopback device for this hardware integration test');assert.match(output.id,/^coreaudio:\d+$/);
    const source=path.join(directory,'tone.wav');await writeFile(source,tone());
    await host.command('deck.load',{deck:'A',track:{trackId:'junction-test-tone',path:source,durationMs:240000,title:'Isolated integration tone'}});
    await until(()=>host.command('state.snapshot'),s=>s.decks.A.status==='ready'||s.decks.A.status==='paused','tone decode');
    await host.command('mixer.channel.gain',{deck:'A',gain:.8});await host.command('mixer.master.gain',{gain:.8});await host.command('deck.play',{deck:'A'});
    if(process.env.PLUMDECK_JUNCTION_TEST_DECKS==='4'){
      for(const [i,deck] of ['B','C','D'].entries()){
        await host.command('deck.load',{deck,track:{trackId:`junction-${deck}`,path:source,durationMs:240000,title:`Deck ${deck}`}});
        await until(()=>host.command('state.snapshot'),s=>['ready','paused'].includes(s.decks[deck].status),`${deck} decode`);
        await host.command('deck.seek',{deck,positionMs:(i+1)*1100});await host.command('mixer.channel.gain',{deck,gain:.13});
        await host.command('deck.play',{deck});
      }
    }
    if(process.env.PLUMDECK_JUNCTION_TEST_SAMPLER){
      await host.command('sampler.gain',{gain:.08});
      for(const [bank,slot] of [[0,0],[3,15]]){
        await host.command('sampler.bank',{bank});await host.command('sampler.load',{bank,slot,path:source});
        await until(()=>host.command('sampler.state'),s=>s.slots[slot].status==='ready',`sampler ${bank}:${slot} decode`);
        await host.command('sampler.play',{bank,slot});
      }
      await host.command('sampler.bank',{bank:0});
    }
    if(process.env.PLUMDECK_JUNCTION_TEST_COLOR)await host.command('mixer.colorfx.set',{deck:'A',effect:process.env.PLUMDECK_JUNCTION_TEST_COLOR,amount:.3});
    if(process.env.PLUMDECK_JUNCTION_TEST_PAD)await host.command('mixer.fx.set',{deck:'A',effect:process.env.PLUMDECK_JUNCTION_TEST_PAD,enabled:true,mix:.25,depth:.5});
    if(process.env.PLUMDECK_JUNCTION_TEST_FX)await host.command('mixer.beatfx.set',{target:'master',effect:process.env.PLUMDECK_JUNCTION_TEST_FX,enabled:true,mix:.25,beats:.5,bpm:120});
    await host.command('junction.create',{displayName:'Test host',sessionName:'Isolated runtime test',signalingUrl,programDevice:output.id.slice(10),adoptCurrent:true});
    const created=await until(()=>host.command('junction.snapshot'),s=>s.active&&s.invite&&s.localPeerId&&s.program.state==='running','host session and Program');
    assert.equal(created.hostPeerId,created.localPeerId);assert.equal(created.performerPeerId,created.localPeerId);
    await guest.command('junction.join',{displayName:'Test guest',invite:created.invite});
    const pending=await until(()=>host.command('junction.snapshot'),s=>s.participants.some(p=>p.peerId!==s.localPeerId&&p.approved===false),'explicit pending admission');
    const guestPeer=pending.participants.find(p=>p.peerId!==pending.localPeerId);
    const unapproved=await host.raw('junction.handoff.request',{targetPeerId:guestPeer.peerId});
    assert(unapproved.kind==='error'||unapproved.ok===false,'unapproved participant cannot become next DJ');
    await host.command('junction.peer.approve',{peerId:guestPeer.peerId,accept:true});
    const joined=await until(()=>guest.command('junction.snapshot'),s=>s.active&&s.hostPeerId===created.hostPeerId&&s.performerPeerId===created.performerPeerId&&s.participants.length===2,'authenticated guest state',60000);
    assert.notEqual(joined.localPeerId,joined.performerPeerId);
    // Every DJ operates only their own mixer; the old nomination handoff is gone.
    const prepared=await guest.raw('mixer.channel.gain',{deck:'A',gain:0});assert(!(prepared.kind==='error'||prepared.ok===false),'a waiting guest prepares on its own mixer');
    assert.equal((await host.command('state.snapshot')).decks.A.status,'playing','the ON AIR host is untouched by guest preparation');
    const retired=await host.raw('junction.handoff.request',{targetPeerId:joined.localPeerId});assert(retired.kind==='error'||retired.ok===false,'the nomination handoff is retired');
    const before=await host.command('state.snapshot');assert.equal(before.decks.A.status,'playing');
    const recording=path.join(directory,'program.wav');await host.command('junction.program.record.start',{path:recording});await pause(1200);await host.command('junction.program.record.stop');assertProgramAudio(await readFile(recording));
    const guestSource=path.join(directory,'guest.wav');await writeFile(guestSource,tone(240,554.37));
    await guest.command('deck.load',{deck:'A',track:{trackId:'guest-tone',path:guestSource,title:'Guest tone',durationMs:240000}});
    await until(()=>guest.command('state.snapshot'),s=>['ready','paused'].includes(s.decks.A.status),'guest decode');await guest.command('deck.play',{deck:'A'});
    const handoffRecording=path.join(directory,'handoff.wav');await host.command('junction.program.record.start',{path:handoffRecording});
    await until(()=>host.command('junction.snapshot'),s=>s.turn.nextPeerId===joined.localPeerId,'the guest is next in the timetable');
    await until(()=>guest.command('junction.snapshot'),s=>s.turn.signal==='ready'&&s.junctionInput.receiving,'guest READY on the host J relay',60000);
    await guest.command('mixer.channel.gain',{deck:'A',gain:.75});
    const owner=await until(()=>guest.command('junction.snapshot'),s=>BigInt(s.epoch)>BigInt(created.epoch)&&s.performerPeerId===s.localPeerId,'fader start puts the guest on air',30000);
    assert.equal(BigInt(owner.epoch),BigInt(created.epoch)+1n,'one turn advances exactly one epoch');
    await until(()=>host.command('junction.snapshot'),s=>s.performerPeerId===owner.localPeerId&&s.turn.outgoingPeerId===s.localPeerId&&s.turn.tailDecks.includes('A'),'host sends its tail',20000);
    const tailLocked=await host.raw('deck.pause',{deck:'A'});assert(tailLocked.kind==='error'||tailLocked.ok===false,'the outgoing tail keeps playing');
    await guest.command('junction.input.set',{volume:0});
    await until(()=>host.command('junction.snapshot'),s=>s.turn.outgoingPeerId==='','the guest fades the host out',15000);
    const currentOwnerEpoch=owner.epoch;
    const signalPort=server.address().port;await server.close();
    await until(()=>host.command('junction.snapshot'),s=>s.connection.state!=='connected','discovery outage observed',10000);
    server=createServer();await listen(server,signalPort,'127.0.0.1');
    const rediscovered=await until(()=>host.command('junction.snapshot'),s=>s.connection.state==='connected','native host re-registration',15000);
    assert.equal(rediscovered.performerPeerId,owner.localPeerId);assert.equal(rediscovered.epoch,currentOwnerEpoch,'discovery restart preserves native authority');
    await pause(1500);await host.command('junction.program.record.stop');assertProgramAudio(await readFile(handoffRecording),true);
    const again=await host.command('junction.snapshot');
    guest.suspend();
    const recovering=await until(()=>host.command('junction.snapshot'),s=>s.handoffState==='recovery','real producer loss detection',10000);
    assert.equal(recovering.epoch,again.epoch,'failure does not roll ownership back');
    await host.command('junction.recovery.resume');
    const recovered=await until(()=>host.command('junction.snapshot'),s=>BigInt(s.epoch)===BigInt(again.epoch)+1n&&s.performerPeerId===s.localPeerId,'future recovery epoch',10000);
    guest.resume();
    await until(()=>guest.command('junction.snapshot'),s=>s.epoch===recovered.epoch&&s.performerPeerId===recovered.performerPeerId,'returning peer observes recovery',10000);
    const recoveryRecording=path.join(directory,'recovery.wav');await host.command('junction.program.record.start',{path:recoveryRecording});await pause(1200);await host.command('junction.program.record.stop');assertProgramAudio(await readFile(recoveryRecording));
    await host.command('junction.end');await until(()=>host.command('junction.snapshot'),s=>!s.active,'host graceful end',10000);
    await until(()=>guest.command('junction.snapshot'),s=>!s.active,'guest observes host end');
    await pause(250);
    for(const client of [host,guest]){
      const ended=await client.command('junction.snapshot');
      assert.equal(ended.connection.state,'disconnected','stale socket callbacks cannot revive an ended session');
      assert.equal(ended.connection.detail,'');assert.equal(ended.readiness.ready,false);assert.deepEqual(ended.readiness.reasons,[]);
    }
    await Promise.all([host.close(),guest.close(),third?.close()]);
  } catch(error) {
    for(const [label,client] of [['host',host],['guest',guest],['third',third]]) if(client) {
      const state=await client.command('junction.snapshot').catch(()=>null);
      const engine=await client.command('state.snapshot').catch(()=>null);
      console.error(label,JSON.stringify(state ? {active:state.active,connection:state.connection,handoffState:state.handoffState,localPeerId:state.localPeerId,hostPeerId:state.hostPeerId,performerPeerId:state.performerPeerId,participants:state.participants,program:state.program,readiness:state.readiness,nextPeerId:state.nextPeerId,epoch:state.epoch,decks:engine ? Object.fromEntries(Object.entries(engine.decks).map(([deck,value])=>[deck,{status:value.status,positionMs:value.positionMs,rate:value.rate,loop:value.loopRegion,trackId:value.track?.trackId,assetId:value.track?.assetId}])) : null} : null),client.diagnostics());
    }
    throw error;
  } finally {
    await Promise.allSettled([host?.close(),guest?.close(),third?.close()]);await server.close();await rm(directory,{recursive:true,force:true});
  }
});
