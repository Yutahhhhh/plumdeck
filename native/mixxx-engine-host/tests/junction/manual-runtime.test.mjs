// Runs actual native Mixxx/WebRTC/Opus peers. Deliberately no signaling-server
// imports or process: the only exchange is the text passed by this test.
import assert from 'node:assert/strict';
import test from 'node:test';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {mkdtemp,writeFile,readFile,rm} from 'node:fs/promises';
import path from 'node:path';
const binary=process.env.PLUMDECK_TEST_HOST||path.resolve(import.meta.dirname,'../../build-upstream/plumdeck-mixxx-engine-host');
const previousBinary=process.env.PLUMDECK_PREVIOUS_HOST;
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function until(read,predicate,label,timeout=40000){const end=Date.now()+timeout;let last;while(Date.now()<end){last=await read();if(predicate(last))return last;await pause(60);}throw Error(`${label}: timed out; phase=${last?.handoffState}, connection=${last?.connection?.state}, turn=${last?.turn?.signal}/${last?.turn?.blocker?.code}`);}
function native(directory,executable=binary,extraEnv={}){
 const child=spawn(executable,[],{env:{...process.env,...extraEnv,PLUMDECK_JUNCTION_EPHEMERAL_NETWORK:'1',PLUMDECK_MIXXX_OUTPUT_DEVICE:process.env.PLUMDECK_MIXXX_OUTPUT_DEVICE||'BlackHole 2ch',PLUMDECK_MIXXX_RECORDING_DIR:directory}});
 let hello,id=0,stderr='';const pending=new Map();child.stderr.on('data',b=>{stderr=(stderr+b).slice(-3000);});
 const reject=reason=>{for(const p of pending.values()){clearTimeout(p.timer);p.reject(reason);}pending.clear();};child.on('error',reject);child.on('exit',code=>reject(Error(`Native exited: ${code}`)));
 createInterface({input:child.stdout}).on('line',line=>{let reply;try{reply=JSON.parse(line);}catch{return;}const p=pending.get(reply.id);if(p){clearTimeout(p.timer);pending.delete(reply.id);p.resolve(reply);}});
 const raw=(op,params={})=>{if(child.exitCode!==null||child.signalCode!==null)return Promise.reject(Error(`Native already exited: ${child.exitCode}/${child.signalCode}`));const current=++id;const promise=new Promise((resolve,reject)=>{const timer=setTimeout(()=>{pending.delete(current);reject(Error(`Native ${op} timed out`));},15000);pending.set(current,{resolve,reject,timer});});child.stdin.write(JSON.stringify({id:current,op,params,...(hello?{engineId:hello.engineId,sessionId:hello.sessionId}:{})})+'\n');return promise;};
 const command=async(op,params={})=>{const r=await raw(op,params);assert(r.kind!=='error'&&r.ok!==false,`${op}: ${r.error?.message||'native error'}`);return r.data??r;};
 return {command,raw,snapshot:()=>command('junction.snapshot'),suspend:()=>child.kill('SIGSTOP'),resume:()=>child.kill('SIGCONT'),async start(){hello=await command('session.hello');await until(()=>command('state.snapshot'),s=>s.audio.applied,'audio configuration');await command('junction.network.configure',{stunUrls:[],turn:{},save:false});},async close(){child.kill('SIGCONT');child.stdin.end();if(child.exitCode===null&&child.signalCode===null)await Promise.race([new Promise(r=>child.once('exit',r)),pause(4000)]);if(child.exitCode===null&&child.signalCode===null){child.kill('SIGKILL');await new Promise(r=>child.once('exit',r));}assert.equal(child.exitCode,0,`clean native exit (${child.signalCode}); ${stderr}`);}};
}
function tone(){const rate=48000,n=rate*240,b=Buffer.alloc(44+n*4);b.write('RIFF');b.writeUInt32LE(b.length-8,4);b.write('WAVEfmt ',8);b.writeUInt32LE(16,16);b.writeUInt16LE(1,20);b.writeUInt16LE(2,22);b.writeUInt32LE(rate,24);b.writeUInt32LE(rate*4,28);b.writeUInt16LE(4,32);b.writeUInt16LE(16,34);b.write('data',36);b.writeUInt32LE(n*4,40);for(let i=0;i<n;i++){const x=Math.round(7500*Math.sin(i*2*Math.PI*440/rate)+1700*Math.sin(i*2*Math.PI*733.37/rate));b.writeInt16LE(x,44+i*4);b.writeInt16LE(x,46+i*4);}return b;}
// A small stereo file (the waveform generator requires stereo) with unique content per run, so the coordinator really
// transfers and verifies it instead of finding a cached copy of the same hash.
function remoteTone(seconds,seed){const rate=22050,n=rate*seconds,b=Buffer.alloc(44+n*4);b.write('RIFF');b.writeUInt32LE(b.length-8,4);b.write('WAVEfmt ',8);b.writeUInt32LE(16,16);b.writeUInt16LE(1,20);b.writeUInt16LE(2,22);b.writeUInt32LE(rate,24);b.writeUInt32LE(rate*4,28);b.writeUInt16LE(4,32);b.writeUInt16LE(16,34);b.write('data',36);b.writeUInt32LE(n*4,40);const f=330+(seed%97);for(let i=0;i<n;i++){const x=Math.round(9000*Math.sin(i*2*Math.PI*f/rate));b.writeInt16LE(x,44+i*4);b.writeInt16LE(x,46+i*4);}b.writeUInt32LE(seed>>>0,44);return b;}
function recordedPcm(bytes){assert.equal(bytes.toString('ascii',0,4),'RIFF');assert.equal(bytes.readUInt32LE(4)+8,bytes.length);let pcm,bits,channels;for(let p=12;p+8<=bytes.length;){const n=bytes.readUInt32LE(p+4),tag=bytes.toString('ascii',p,p+4);if(tag==='fmt '){channels=bytes.readUInt16LE(p+10);bits=bytes.readUInt16LE(p+22);}if(tag==='data')pcm=bytes.subarray(p+8,p+8+n);p+=8+n+(n%2);}assert.equal(bits,24);assert.equal(channels,2);assert(pcm.length>48000);let energy=0;for(let p=0;p+3<=pcm.length;p+=3){const x=pcm.readIntLE(p,3)/8388608;energy+=x*x;}assert(Math.sqrt(energy/(pcm.length/3))>.005,'real Program audio is non-silent');// Exclude only the recorder's opening half-second; admission/handoff/re-exchange happen later.
let silence=0,longest=0,longestAt=0;for(let p=48000*6/2;p+6<=pcm.length;p+=6){silence=Math.abs(pcm.readIntLE(p,3)/8388608)<.0001?silence+1:0;if(silence>longest){longest=silence;longestAt=p/6-silence;}}assert(longest<=240,`handoff/re-exchange Program silence ${longest/48}ms at ${longestAt/48000}s exceeds 5ms`);}
const participant=(s,id)=>s.participants.find(p=>p.peerId===id);
async function readyInvite(host,id){return until(host.snapshot,s=>participant(s,id)?.exchange?.inviteText?.length>0,'both offers complete');}
async function newInvite(host,peerId){const before=await host.snapshot();await host.command('junction.invite.create',peerId?{peerId}:{});const created=await host.snapshot();const peer=peerId?participant(created,peerId):created.participants.find(p=>p.peerId!==created.localPeerId&&!before.participants.some(old=>old.peerId===p.peerId));assert(peer,'individual invitation card exists');if(peer.exchange.state==='collecting')assert(!peer.exchange.inviteText,'unfinished gathering is not exportable');const ready=await readyInvite(host,peer.peerId);return {peerId:peer.peerId,text:participant(ready,peer.peerId).exchange.inviteText};}
async function response(guest,text,name,profile={}){await guest.command('junction.exchange.inspect',{text});await guest.command('junction.join',{displayName:name,djName:name,text,...profile});const s=await until(guest.snapshot,s=>s.exchange?.responseText?.length>0,'both answers complete');return s.exchange.responseText;}
const denied=r=>r.kind==='error'||r.ok===false;
const lease=async peer=>{const s=await peer.snapshot();return {sessionId:s.sessionId,epoch:s.epoch,actorPeerId:s.localPeerId};};
test('Junction Live stays local display state: wire announcements never carry paths or load a deck',async()=>{
 const runtime=await readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8');
 const tracks=await readFile(path.resolve(import.meta.dirname,'../../src/junction/shared_tracks.cpp'),'utf8');
 const host=await readFile(path.resolve(import.meta.dirname,'../../src/host.cpp'),'utf8');
 assert.match(runtime,/if\(!wire\)state\["junctionTracks"\]=\(hosting&&auth\.owner!=auth\.local\)/);
 assert.match(runtime,/auto s=publicState\(true\);[^\n]*s\.remove\("junctionTracks"\)/);
 const announcement=tracks.slice(tracks.indexOf('QJsonObject trackAnnouncement('),tracks.indexOf('std::optional<std::vector<AnnouncedTrack>> parseTrackAnnouncement('));
 assert(announcement.length>0);assert.doesNotMatch(announcement,/"path"/);
 assert.doesNotMatch(runtime,/importJunctionTrack|tryMirror|junction\.tracks\.load/,'remote tracks are never auto-loaded into coordinator decks');
 assert.doesNotMatch(host,/loadJunctionTrack|junction\.tracks\.load/,'the monitor has no native deck-load command');
});
test('Program Mixer routing is published locally and guards the local return feed',async()=>{
 const runtime=await readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8');
 assert.match(runtime,/if\(!wire\)\{state\["localPrep"\]=localPrep\(\);state\["junctionInput"\]=inputState\(\);\n\s*state\["operatorPeerId"\][^\n]*\n\s*state\["programMixer"\]=programMixer\(\)\.toJson/,'routing is local-only snapshot state, never on the wire');
 assert.match(runtime,/if\(previous==auth\.local\)\{if\(programMixer\(true,false\)\.localReturnAllowed\(\)\)/,'a local return is refused while JUNCTION MASTER is in main');
 assert.doesNotMatch(runtime,/backend->junctionInputMainMix\((true|false)\)/,'every main-mix change goes through the mirrored setter');
});
test('connected Lite DJs are candidates and a played DJ can request again',async()=>{
 const runtime=await readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8');
 const status=runtime.slice(runtime.indexOf('QString rosterStatus('),runtime.indexOf('void enrichRosterRow('));
 assert.match(status,/if\(manual&&peer&&!peer->lite\)/,'a Lite row never reports the idle manual exchange as invited');
 assert(status.indexOf('"requested"')<status.indexOf('"finished"'),'a new request outranks the played state');
 assert.doesNotMatch(runtime,/演奏中・演奏済みのDJは移動できません/,'played DJs can be queued again');
});
test('additive Junction Live negotiation is independent from the compatible baseline',async()=>{
 const runtime=await readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8');
 const exchange=await readFile(path.resolve(import.meta.dirname,'../../src/junction/manual_exchange.cpp'),'utf8');
 assert.match(runtime,/junction-5/,'new releases retain the compatible baseline');
 assert.doesNotMatch(runtime,/junction-6/);
 assert.match(exchange,/junction-5/,'manual exchange remains compatible with the previous release');
 assert.doesNotMatch(exchange,/junction-6/);
 assert.match(runtime,/junction-live-monitor-v1/);
 assert.match(runtime,/"junctionCapabilities",QJsonArray\{kJunctionTracksCapability,QString::fromLatin1\(turn::kCapability\)\}/,'fader start is an additive capability');
 assert.match(runtime,/capabilities\.contains\(QString::fromLatin1\(turn::kCapability\)\)\)p\.faderStartV1=true/,'a peer takes turns only after negotiating fader start');
 assert.match(runtime,/if\(!peer\.approved\|\|!peer\.hello\|\|!peer\.faderStartV1\)return false;/,'an older peer is never silently given the old handoff');
 assert.match(runtime,/!host->second->junctionTracksV1/,'an old host is never sent an unknown track message');
 assert.match(runtime,/MessageType::TrackAnnounce && p\.junctionTracksV1/,'an announcement is accepted only after negotiation');
});
test('the Lite takeover seam is measured from the deck, not from the clock',async()=>{
 const runtime=await readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8');
 // No wall-clock cut and no standing correction survive anywhere.
 assert.doesNotMatch(runtime,/liteCut|LiteCut|anchorBias/,'the wall-clock cut and the persistent anchor bias are gone');
 // The deck is fed the whole block description; a bare frame count and rate
 // would throw away the media frame the whole seam is built on.
 assert.match(runtime,/input\.write\(samples,info\);/);
 // The one-shot: armed in exactly one place, consumed by exchange in exactly
 // one place, so no later handoff can inherit it.
 // Armed when this computer takes over, armed by a guest going on air (its
 // stream then names the seam), expired if no block measured it, cleared on stop.
 assert.equal(runtime.match(/takeoverAnchor\.store\(/g).length,4,'armed at a local takeover or a guest ON AIR, expired, and cleared on stop');
 assert.match(runtime,/const bool measureSeam=takeOver&&programOpened;[\s\S]{0,400}takeoverAnchor\.store\(measureSeam/);
 assert.match(runtime,/void goOnAir\(\)[\s\S]{0,500}q->setCaptureAnchor\(UINT64_MAX,0\);takeoverAnchor\.store\(true/,'a guest measures its seam on its own anchored capture');
 assert.match(runtime,/if\(remoteSeam\)seamFrame\.store\(report->frame/,'the host takes a remote seam from the new DJ report, never from its clock');
 assert.equal(runtime.match(/takeoverAnchor\.exchange\(false/g).length,1,'consumed exactly once, by the capture that uses it');
 const capture=runtime.slice(runtime.indexOf('void Runtime::capture('),runtime.indexOf('QJsonObject Runtime::command('));
 assert.match(capture,/TakeoverAnchor::resolve\(d->input\.renderedMediaFrame\(\),elapsed\)/,'the anchor is the frame the deck rendered in this same callback');
 assert.match(capture,/if\(takeover\)d->seamFrame\.store\(media,std::memory_order_release\);/,'the Program boundary is that same measured frame');
 // Program keeps the outgoing DJ strictly below the boundary and the new
 // operator from it on: no gap, no duplicated frame, and no timer involved.
 const allowed=runtime.slice(runtime.indexOf('const auto allowed=[&]'),runtime.indexOf('quint32 begin=0,end=info.frameCount;'));
 assert.match(allowed,/if\(seam\)\{const auto boundary=seamFrame\.load\([^)]*\);if\(!boundary\|\|f<boundary\)return producer==seam->oldOwner&&info\.epoch==seam->oldEpoch;\}/);
 assert.match(allowed,/info\.generation>=ownerMinGeneration/,'a remote ON AIR stream counts only from its anchored generation');
 assert.doesNotMatch(allowed,/now\(\)|monotonicNanos/,'admission never consults a clock');
 // The seam retires on Program having actually been fed past the boundary.
 assert.match(runtime,/if\(boundary&&programEnqueuedThrough>=boundary\)seam\.reset\(\);/);
 const select=runtime.slice(runtime.indexOf('void selectOwner('),runtime.indexOf('bool turnEligible('));
 assert.doesNotMatch(select,/programPending\.clear\(\)/,'an operator switch never drops audio already delivered');
 assert.doesNotMatch(select,/const auto cut=now\(\)/,'the seam is not a moment in time');
});
test('state transfer is unreachable from fader start while recovery stays independent',async()=>{
 const runtime=await readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8');
 assert.doesNotMatch(runtime,/auth\.prepare\(/,'nothing prepares a graph handoff');
 assert.doesNotMatch(runtime,/broadcast\("handoff\.prepare"/,'no host announces a graph handoff');
 const command=runtime.slice(runtime.indexOf('QJsonObject Runtime::command('));
 assert.match(command,/if\(op=="handoff\.request"\|\|op=="handoff\.accept"\|\|op=="handoff\.cancel"\)return reject\("この操作は廃止されました/,'the old operations explain themselves');
 const gate=runtime.slice(runtime.indexOf('Fader start never transfers the performance graph'),runtime.indexOf('if(auth.faderStart)return;'));
 for(const type of ['HandoffPrepare','HandoffFence','HandoffFenced','HandoffReady','HandoffCommit','GraphManifest','GraphCheckpoint','ValidationWindow','AssetRequest'])assert.match(gate,new RegExp(`MessageType::${type}`),`${type} never reaches the state machine`);
 assert.match(runtime,/auth\.faderStart=true;\n\s*timer\.setInterval/,'every runtime starts in fader start');
 assert.match(runtime,/auth=Authority\{\};auth\.faderStart=true;/,'and stays there after a session ends');
 assert.match(runtime,/if\(auth\.faderStart\|\|hosting\|\|!q->localDeckTracks/,'no Junction Live file announcement');
 // Recovery does not use the transfer: it re-routes Program to the host's own mix.
 const recovery=runtime.slice(runtime.indexOf('void beginRecovery('),runtime.indexOf('void collectValidation('));
 assert.doesNotMatch(recovery,/startExport|restoreGraph|beginValidation|auth\.commit\(/);
 assert.match(runtime,/void ownerLost\([\s\S]*beginRecovery\(reason\);/,'an ON AIR loss without a READY next DJ falls back to recovery');
});
test('Lite deck metadata is timestamped on receipt and positions are extrapolated for display',async()=>{
 const runtime=await readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8');
 const host=await readFile(path.resolve(import.meta.dirname,'../../src/host.cpp'),'utf8');
 assert.match(runtime,/found->second->liteDecksAt=monotonicNanos\(\)/,'native receipt time is authoritative');
 const display=runtime.slice(runtime.indexOf('QJsonArray displayedLiteDecks('),runtime.indexOf('void feedInput('));
 assert.match(display,/elapsedMs=double\(ageNanos\)\/1000000\.0/);
 assert.match(display,/positionMs.*elapsedMs\*deck\["rate"\]/s,'playing positions advance at the announced rate');
 assert.match(runtime,/const auto decks=displayedLiteDecks\(\*source->second,lagMs\)/,'snapshots expose the extrapolated copy, as heard through J');
 assert.equal(host.match(/orientation < 0\.5 \? \(1\.0 - crossfader\)/g)?.length,2,
   'native deck audibility maps orientation 0/1/2 to left/thru/right');
});
test('JUNCTION input drafts update from a bounded input reply without a second full snapshot',async()=>{
 const runtime=await readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8');
 const client=await readFile(path.resolve(import.meta.dirname,'../../../../src/services/junction/client.ts'),'utf8');
 const inputSet=runtime.slice(runtime.indexOf('if(op=="input.set")'),runtime.indexOf('if(op=="input.release")'));
 assert.match(inputSet,/return d->inputState\(\)/);
 assert.doesNotMatch(inputSet,/return snapshot\(\)/);
 assert.match(client,/else if \(op === 'input.set'\)[\s\S]*junctionState\.set\(\{\.\.\.current, junctionInput:/);
});
test('the JUNCTION deck is live input: fed only on the realtime grant and never replayed or exported',async()=>{
 const backend=await readFile(path.resolve(import.meta.dirname,'../../src/mixxx_backend.cpp'),'utf8');
 const before=backend.slice(backend.indexOf('audioBridge_.before='),backend.indexOf('audioBridge_.after='));
 assert.match(before,/if\(self->blockGrant_\.mayProcess\)self->feedJunctionAux\(frames\);/);
 assert.equal(before.match(/receiveBuffer|readJunctionInput/g),null,'the aux is fed in exactly one guarded place');
 const feed=backend.slice(backend.indexOf('void feedJunctionAux('),backend.indexOf('void detachLiveInputs('));
 assert.match(feed,/noexcept/);assert.doesNotMatch(feed,/new |resize|push_back|lock|QJson|QString/,'no allocation or locks on the audio thread');
 const restore=backend.slice(backend.indexOf('void tryFinalizeGraphRestore(){'),backend.indexOf('void applyGraphDeck('));
 assert.match(restore,/transferComplete\(restoreTransfer_\)\)return;\s*\/\/[^\n]*\n[^\n]*\n\s*detachLiveInputs\(\);/,'replay starts only after the microphone and aux are detached');
 assert.match(backend,/void detachLiveInputs\(\)\{[^}]*microphone_->receiveBuffer\([^}]*junctionAux_->receiveBuffer\(junctionAuxInput_,nullptr,0\)/);
 const exported=backend.slice(backend.indexOf('QJsonObject snapshotStoppedGraph()'),backend.indexOf('QString restoreJunctionGraph('))+backend.slice(backend.indexOf('void applyGraphMixer(){'),backend.indexOf('bool snapshotPending_'));
 assert.doesNotMatch(exported,/kJunctionGroup|Auxiliary|junctionAux/,'the graph never carries the live aux source');
});
test('Lite return audio is bidirectional, per-recipient, and excluded from JUNCTION feedback',async()=>{
 const [runtime,transport,backend]=await Promise.all([
  readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8'),
  readFile(path.resolve(import.meta.dirname,'../../src/junction/media_transport.cpp'),'utf8'),
  readFile(path.resolve(import.meta.dirname,'../../src/mixxx_backend.cpp'),'utf8'),
 ]);
 assert.match(transport,/Description::Direction::SendRecv/,'the native Lite offer negotiates return audio');
 assert.doesNotMatch(transport,/Description::Direction::RecvOnly/,'the old receive-only Lite offer is gone');
 assert.match(runtime,/std::map<QString,std::unique_ptr<PcmRing>> liteReturnRings/,'each Lite recipient has one bounded SPSC return ring');
 assert.match(runtime,/if\(previous==auth\.local\)\{if\(programMixer\(true,false\)\.localReturnAllowed\(\)\)\{localReturnTarget\.store\(ring/,'a local previous owner uses the local-master return tap');
 assert.match(runtime,/else if\(peers\.count\(previous\)\)\{returnRelaySource=previous;returnRelayTarget=target;/,'a remote previous DJ is relayed only to its receiver');
 assert.match(runtime,/if\(hosting\|\|id==auth\.host\)route\(p->transport->decodedRing\(\),id\)/,'every guest consumes the host J relay');
 assert.match(backend,/ControlObject::set\(ConfigKey\(kJunctionGroup, "main_mix"\), 0\)/,'received audio starts CUE-only');
 assert.match(backend,/if\(local\)runtime->captureLocalReturn\(local,frames,frame,44100\)/,'the return tap reads the LOCAL NEXT bus');
 assert.doesNotMatch(backend,/captureLocalReturn\(master/,'the return tap never reads main (Program Master)');
 const mixerHook=await readFile(path.resolve(import.meta.dirname,'../../cmake/target/CMakeLists.txt'),'utf8');
 assert.match(mixerHook,/pChannelInfo->m_handle\.handle\(\) == plumdeckExcluded \|\| pChannelInfo->m_pChannel->isTalkoverChannel\(\)\) continue;/,'the LOCAL NEXT bus skips Auxiliary1 and microphones');
 assert.match(backend,/audioBridge_\.localReturnExcluded\.store\(junctionHandle\.handle\(\)\.handle\(\)/,'Auxiliary1 is the excluded channel');
 // J enters this DJ's mix at STANDBY (booth monitor and the standby stream),
 // reset to unity there and nowhere else, so a takeover never undoes the mix move.
 assert.match(runtime,/if\(id==auth\.local\)\{faderStart\.reset\(\);backend->junctionInputTakeOver\(\);setInputMainMix\(true\);\}/,'host STANDBY');
 assert.match(runtime,/if\(isNext&&previousNext!=auth\.local\)\{faderStart\.reset\(\);backend->junctionInputTakeOver\(\);setInputMainMix\(true\);\}/,'guest STANDBY');
 assert.equal(runtime.match(/if\(isNext&&previousNext!=auth\.local\)\{faderStart\.reset\(\);backend->junctionInputTakeOver\(\);setInputMainMix\(true\);\}/g)?.length,2,'guest STANDBY under a desktop host and under a Lite host');
 assert.equal(runtime.match(/backend->junctionInputTakeOver\(\)/g)?.length,3,'unity only when STANDBY begins');
 assert.equal(runtime.match(/setInputMainMix\(true\)/g)?.length,3,'no other path puts JUNCTION MASTER into main');
 const tailHook=await readFile(path.resolve(import.meta.dirname,'../../cmake/target/CMakeLists.txt'),'utf8');
 assert.match(tailHook,/if \(plumdeckMask && [^\n]*\) continue;/,'the OUTGOING tail restricts the bus to its decks');
 assert.match(tailHook,/plumdeckFixedGain >= 0 \? plumdeckFixedGain : m_pMainGain->get\(\)/,'the tail level is fixed at the turn change');
 assert.match(runtime,/const float\* pcm=d->tailSend\.load\(std::memory_order_relaxed\)&&local\?local:master;/,'an OUTGOING guest streams only its tail');
});
test('JUNCTION MASTER is real engine audio: Program Master mixes it with LOCAL NEXT, the return never carries it',{timeout:120000},async()=>{
 const directory=await mkdtemp('/tmp/plumdeck-junction-master-');const host=native(directory,binary,{PLUMDECK_JUNCTION_AUDIO_PROBE:'1'});
 try{
  await host.start();
  const probe=async(params={})=>(await host.command('junction.input.probe',params));
  const meters=async()=>(await probe()).channel.meters;
  const settle=async(label,predicate)=>until(meters,predicate,label,10000);
  const steady=async(label,predicate)=>{await pause(500);const m=await meters();assert(predicate(m),`${label}: ${JSON.stringify(m)}`);return m;};
  // A synthetic JUNCTION MASTER through the same JunctionInput a decoded P2P stream uses.
  let state=await probe({tone:{amplitude:.5,frequencyHz:1000},mainMix:false,channel:{volume:1,orientation:1,eqLow:1,eqMid:1,eqHigh:1}});
  assert.equal(state.channel.mainMix,false);
  await until(async()=>(await probe()).channel.vu,vu=>vu>.05,'Auxiliary1 receives the input',10000);
  await steady('CUE-only JUNCTION MASTER stays off Program Master',m=>m.programPeak<.001&&m.localReturnPeak<.001);
  state=await probe({mainMix:true});assert.equal(state.channel.mainMix,true);assert.equal(state.programMixer.returnFeed.tap,'local-next-bus');
  const mixed=await settle('main_mix puts JUNCTION MASTER into Program Master',m=>m.programPeak>.1);
  await steady('JUNCTION MASTER in Program never reaches the return bus',m=>m.localReturnPeak<.001);
  await probe({channel:{volume:0}});await settle('LEVEL 0 silences JUNCTION MASTER on Program',m=>m.programPeak<.001);
  await probe({channel:{volume:1}});await settle('LEVEL restores it',m=>m.programPeak>.1);
  await probe({channel:{eqMid:0}});const cut=await settle('mid EQ kill cuts a 1 kHz JUNCTION MASTER',m=>m.programPeak<mixed.programPeak/4);
  await probe({channel:{eqMid:1}});await settle('mid EQ restores it',m=>m.programPeak>cut.programPeak*3);
  await probe({channel:{orientation:0}});await host.command('mixer.crossfader',{position:1});
  await settle('crossfader to the far side silences a left-assigned JUNCTION MASTER',m=>m.programPeak<.001);
  await probe({channel:{orientation:1}});await settle('THRU ignores the crossfader',m=>m.programPeak>.1);
  await host.command('mixer.crossfader',{position:0});
  const pfl=await host.raw('junction.input.probe',{channel:{pfl:true}});
  if(state.channel.pflAvailable){assert(!denied(pfl));await probe({mainMix:false});await settle('CUE sends JUNCTION MASTER to headphones only',m=>m.pflPeak>.05&&m.programPeak<.001);await probe({mainMix:true,channel:{pfl:false}});}
  else assert(denied(pfl),'CUE is refused without a headphone output instead of pretending');
  // LOCAL NEXT joins the same Program Master; only it is returned.
  const source=path.join(directory,'local-next.wav');await writeFile(source,tone());
  await host.command('deck.load',{deck:'A',track:{trackId:'local-next',path:source,title:'LOCAL NEXT tone'}});
  await until(()=>host.command('state.snapshot'),s=>['ready','paused'].includes(s.decks.A.status),'LOCAL NEXT decode');await host.command('deck.play',{deck:'A'});
  const both=await settle('LOCAL NEXT reaches the return bus',m=>m.localReturnPeak>.05);
  assert(both.programPeak>both.localReturnPeak+.05,`Program Master carries JUNCTION MASTER on top of LOCAL NEXT: ${JSON.stringify(both)}`);
  await probe({channel:{volume:0}});
  const localOnly=await steady('Program without JUNCTION MASTER',m=>m.localReturnPeak>.05);
  assert(Math.abs(localOnly.localReturnPeak-both.localReturnPeak)<.02,`the return is unchanged by JUNCTION MASTER: ${JSON.stringify({both,localOnly})}`);
  assert(Math.abs(localOnly.programPeak-localOnly.localReturnPeak)<.02,`with JUNCTION MASTER down, Program Master equals LOCAL NEXT: ${JSON.stringify(localOnly)}`);
  // The master knob is the DJ's booth volume: neither Program nor the J feed follows it.
  await host.command('mixer.master.gain',{gain:.2});
  const booth=await steady('booth volume down',m=>m.localReturnPeak>.05);
  assert(Math.abs(booth.programPeak-localOnly.programPeak)<.02&&Math.abs(booth.localReturnPeak-localOnly.localReturnPeak)<.02,`the venue and J never follow the master knob: ${JSON.stringify({localOnly,booth})}`);
  await host.command('mixer.master.gain',{gain:1});
  await probe({tone:{amplitude:0},mainMix:false});
  await host.command('deck.pause',{deck:'A'});
 }finally{await host.close();await rm(directory,{recursive:true,force:true});}
});
test('Lite takeover starts the JUNCTION deck audible and measures its Program seam once',{timeout:120000},async()=>{
 const directory=await mkdtemp('/tmp/plumdeck-lite-takeover-');const host=native(directory);
 try{
  await host.start();
  const devices=await host.command('audio.devices.list');const output=devices.devices.find(d=>d.name.includes('BlackHole')&&d.outputChannels>=2);assert(output,'loopback device required, not skipped');
  await host.command('junction.create',{djName:'ホスト DJ',sessionName:'Lite takeover',programDevice:output.id.replace(/^coreaudio:/,''),adoptCurrent:true,exchangeMode:'manual'});
  const created=await until(host.snapshot,s=>s.active&&s.program.state==='running','host and Program');const liteId='lite-takeover-dj';
  await host.command('junction.lite.peer.ensure',{peerId:liteId,djName:'Lite DJ'});
  const performing=await host.command('junction.lite.owner.set',{ownerPeerId:liteId});
  assert.equal(performing.performerPeerId,liteId);assert.equal(performing.localPrep,false,'the outgoing Mac stays locked while its master is still returned');
  assert.equal(performing.junctionInput.releasingPeerId,created.localPeerId);
  assert.equal(performing.programMixer.venueSource,'direct-stream','a remote operator still reaches the venue without this mixer');
  assert.equal(performing.programMixer.localNext.cueOnly,true,'LOCAL NEXT never reaches Program before takeover');
  assert.equal(performing.programMixer.junctionMaster.inProgram,false);
  assert.notEqual(performing.programMixer.returnFeed.source,'relayed-peer','the Mac returns only its own local play');
  assert.equal(performing.operatorPeerId,liteId);
  // Simulate the new Lite operator fading the Mac return and releasing it.
  await host.command('junction.input.release');
  let released=await host.snapshot();assert.equal(released.junctionInput.releasingPeerId,'');
  assert.equal(released.turn.nextPeerId,'','a DJ who finished is out of the timetable until they join again');
  // Joining the queue makes this Mac next: STANDBY resets J to unity, flat, THRU.
  await host.command('junction.input.set',{volume:0,orientation:0});
  await host.command('junction.turn.join');
  const standby=await until(host.snapshot,s=>s.turn.nextPeerId===s.localPeerId&&s.localPrep&&s.junctionInput.peerId===liteId,'the released Mac prepares on STANDBY while Lite performs');
  assert.equal(standby.junctionInput.channel.volume,1);assert.equal(standby.junctionInput.channel.orientation,1,'THRU');assert.equal(standby.junctionInput.channel.audible,true);
  assert.equal(standby.turn.signal,'standby','no Lite audio arrives, so this DJ cannot be READY');
  assert(['waiting_junction','junction_unstable'].includes(standby.turn.blocker.code),`one DJ-facing reason: ${standby.turn.blocker.code}`);
  assert.equal(standby.programMixer.junctionMaster.inProgram,false,'STANDBY hears J on the booth monitor only');
  const takeover=async()=>{const s=await host.command('junction.lite.owner.set',{ownerPeerId:created.localPeerId});assert.equal(s.performerPeerId,created.localPeerId);assert.equal(s.localPrep,false);assert.equal(s.junctionInput.releasingPeerId,liteId,'the outgoing Lite DJ keeps sounding');return s;};
  const taken=await takeover();
  assert.equal(taken.junctionInput.channel.volume,1,'a takeover keeps J where the DJ mixed it');assert.equal(taken.junctionInput.channel.audible,true);
  assert.equal(taken.programMixer.venueSource,'local-mix','Program Master is this mixer after takeover');
  assert.equal(taken.programMixer.junctionMaster.inProgram,true,'JUNCTION MASTER is mixed, not bypassed');
  assert.equal(taken.programMixer.localNext.inProgram,true);
  assert.equal(taken.programMixer.returnFeed.source,'none','nothing is returned while JUNCTION MASTER is in main');
  assert.equal(taken.turn.signal,'onair');assert.equal(taken.turn.outgoingPeerId,liteId);
  // The seam is measured by the first captured block, not timed: the one-shot
  // is already spent and the boundary is real by the time the next snapshot
  // is answered. This peer sends no audio, so the deck reported nothing to
  // render and the boundary falls back to the session clock.
  const seamed=await until(host.snapshot,s=>s.junctionInput.seamPending===false&&Number(s.junctionInput.seamFrame)>0,'the takeover seam is measured',4000);
  await pause(600);const settled=await host.snapshot();
  assert.equal(Number(settled.junctionInput.seamFrame),Number(seamed.junctionInput.seamFrame),'the boundary is measured once and never re-derived');
  assert.equal(settled.junctionInput.seamPending,false,'nothing re-arms the one-shot');
  // Held while the deck is up: no fade, so no release yet.
  assert.equal(settled.junctionInput.releasingPeerId,liteId,'no automatic release while the new operator keeps the deck up');
  // This peer never sent a frame, so the stream-ended fallback is what ends
  // the hold: an outgoing DJ whose audio stopped arriving is already gone.
  await until(host.snapshot,s=>s.junctionInput.releasingPeerId==='','a stream that never arrives releases on its own',6000);
  // Again, released explicitly this time.
  await host.command('junction.lite.owner.set',{ownerPeerId:liteId});
  await host.command('junction.input.release');await host.command('junction.turn.join');
  await until(host.snapshot,s=>s.turn.nextPeerId===s.localPeerId,'STANDBY again');
  await takeover();released=await host.command('junction.input.release');
  assert.equal(released.junctionInput.releasingPeerId,'','an explicit release ends the hold at once');
  assert.equal(released.programMixer.junctionMaster.inProgram,false,'a released JUNCTION MASTER leaves Program');
  await host.command('junction.end');await until(host.snapshot,s=>!s.active,'host end');
 }finally{await host.close();await rm(directory,{recursive:true,force:true});}
});
test('fader start: a remote first DJ, guest to host and host to guest turns keep Program continuous and release the tail',{timeout:360000},async()=>{
 const directory=await mkdtemp('/tmp/plumdeck-fader-start-');const host=native(directory),guest=native(directory);
 try{
  await host.start();await guest.start();
  const devices=await host.command('audio.devices.list');const output=devices.devices.find(d=>d.name.includes('BlackHole')&&d.outputChannels>=2);assert(output,'loopback device required, not skipped');
  const seed=Date.now()%1000000007,hostSource=path.join(directory,'host.wav'),guestSource=path.join(directory,'guest.wav');await writeFile(hostSource,tone());await writeFile(guestSource,remoteTone(240,seed));
  for(const [peer,file,label] of [[host,hostSource,'host'],[guest,guestSource,'guest']]){
   await peer.command('mixer.channel.gain',{deck:'A',gain:0});await peer.command('deck.load',{deck:'A',track:{trackId:`${label}-tone`,path:file,title:`${label} tone`,artist:'Fader DJ',bpm:124}});
   await until(()=>peer.command('state.snapshot'),s=>['ready','paused'].includes(s.decks.A.status),`${label} decode`);await peer.command('deck.play',{deck:'A'});
  }
  await host.command('junction.create',{djName:'Host DJ',sessionName:'Fader start',programDevice:output.id.replace(/^coreaudio:/,''),adoptCurrent:false,startInLobby:true,exchangeMode:'manual'});
  const slot=await newInvite(host);const answer=await response(guest,slot.text,'Guest DJ');await host.command('junction.exchange.import',{text:answer});await host.command('junction.peer.approve',{peerId:slot.peerId,accept:true});
  await until(guest.snapshot,s=>s.connection.state==='connected'&&s.exchange.state==='connected','guest connected');
  await until(host.snapshot,s=>participant(s,slot.peerId)?.rosterStatus==='waiting','guest negotiated');
  assert(denied(await host.raw('junction.handoff.request',{targetPeerId:slot.peerId})),'the nomination handoff is retired');
  assert(denied(await guest.raw('junction.turn.repeat',{enabled:true})),'only the host sets B2B repeat');
  await host.command('junction.turn.repeat',{enabled:true});
  // Bootstrap from a silent J: the guest is first in the timetable.
  const started=await host.command('junction.session.start',{performerPeerId:slot.peerId});
  assert.equal(started.lifecycle,'live');assert.equal(started.performerPeerId,'','nobody is on air until a fader goes up');
  await until(host.snapshot,s=>s.turn.nextPeerId===slot.peerId,'the guest is next');
  const firstReady=await until(guest.snapshot,s=>s.turn.signal==='ready','the first DJ is READY without J',40000);
  assert.equal(firstReady.turn.blocker.code,'');
  await guest.command('mixer.channel.gain',{deck:'A',gain:1});
  await until(host.snapshot,s=>s.performerPeerId===slot.peerId,'raising the fader puts the first DJ on air',10000);
  await until(guest.snapshot,s=>s.turn.signal==='onair','the guest sees ON AIR');
  await until(host.snapshot,s=>Number(s.program.rms)>.001,'Program carries the first DJ',10000);
  const recording=path.join(directory,'fader-program.wav');await host.command('junction.program.record.start',{path:recording});
  // Guest to host: the host hears the guest on J and mixes in with its own fader.
  const hostStandby=await until(host.snapshot,s=>s.turn.nextPeerId===s.localPeerId&&s.junctionInput.receiving,'host STANDBY hears the ON AIR guest on J',20000);
  assert.equal(hostStandby.junctionInput.channel.volume,1);assert.equal(hostStandby.junctionInput.channel.orientation,1);
  assert.equal(hostStandby.programMixer.venueSource,'direct-stream','the venue still hears the guest directly');
  await until(host.snapshot,s=>s.turn.signal==='ready','host READY',40000);
  await until(guest.snapshot,s=>s.turn.nextStatus==='ready','the ON AIR guest sees the next DJ ready',10000);
  await host.command('junction.turn.cue',{kind:'go_ahead'});await until(guest.snapshot,s=>s.turn.cue?.kind==='go_ahead','a booth cue reaches the other DJ');
  await host.command('mixer.channel.gain',{deck:'A',gain:1});
  const hostOnAir=await until(host.snapshot,s=>s.performerPeerId===s.localPeerId,'host ON AIR by fader start',5000);
  assert.equal(hostOnAir.turn.outgoingPeerId,slot.peerId);assert.equal(hostOnAir.programMixer.venueSource,'local-mix');
  const outgoing=await until(guest.snapshot,s=>s.turn.signal==='outgoing'&&s.turn.tailDecks.includes('A'),'the guest keeps its tail',10000);
  assert.equal(outgoing.localPrep,false);
  assert(denied(await guest.raw('deck.pause',{deck:'A'})),'the tail deck refuses transport');
  assert(denied(await guest.raw('mixer.channel.gain',{deck:'A',gain:0})),'the tail level is the receiver\'s J');
  assert(denied(await guest.raw('mixer.crossfader',{position:1})),'master controls wait for the release');
  assert(!denied(await guest.raw('deck.loop.set',{deck:'A',startMs:1000,endMs:3000})),'a loop on the tail is allowed');
  assert(!denied(await guest.raw('deck.loop.enable',{deck:'A',enabled:false})),'and can be exited');
  assert(!denied(await guest.raw('deck.load',{deck:'B',track:{trackId:'guest-prep',path:guestSource,title:'prep'}})),'other decks are free for preparation');
  const withDecks=await until(host.snapshot,s=>s.junctionInput.decks?.some(d=>d.deck==='A'&&d.title==='guest tone'&&d.bpm===124),'J shows the tail track without transferring its file');
  assert(!JSON.stringify(withDecks.junctionInput.decks).includes(guestSource),'no path crosses the wire');
  await host.command('junction.input.set',{volume:0});
  await until(host.snapshot,s=>s.turn.outgoingPeerId==='','J held down releases the tail',10000);
  await until(guest.snapshot,s=>s.turn.tailDecks.length===0&&s.turn.signal!=='outgoing','the guest is free again');
  // Host to guest: with B2B repeat the guest is next again; its fader is still up.
  await until(host.snapshot,s=>s.turn.nextPeerId===slot.peerId,'B2B repeat queues the guest again');
  await until(guest.snapshot,s=>s.turn.blocker.code==='already_audible','a fader left up must come down first',40000);
  assert(denied(await guest.raw('junction.turn.onair')),'not READY: no manual ON AIR either');
  await guest.command('mixer.channel.gain',{deck:'A',gain:0});
  await until(guest.snapshot,s=>s.turn.signal==='ready'&&s.junctionInput.receiving,'guest READY on the host J relay',40000);
  await guest.command('mixer.channel.gain',{deck:'A',gain:1});
  await until(host.snapshot,s=>s.performerPeerId===slot.peerId&&s.turn.outgoingPeerId===s.localPeerId,'guest ON AIR, host OUTGOING',15000);
  await until(host.snapshot,s=>s.turn.tailDecks.includes('A'),'the host tail');
  await guest.command('junction.input.set',{volume:0});
  await until(host.snapshot,s=>s.turn.outgoingPeerId==='','the guest releases the host tail',15000);
  await pause(1500);await host.command('junction.program.record.stop');
  recordedPcm(await readFile(recording));
  await host.command('junction.end');await until(host.snapshot,s=>!s.active,'host end');await until(guest.snapshot,s=>!s.active,'guest end');
 }finally{const closed=await Promise.allSettled([host.close(),guest.close()]);await rm(directory,{recursive:true,force:true});for(const result of closed)if(result.status==='rejected')throw result.reason;}
});
test('current and previous engines connect in both host/guest directions',{skip:!previousBinary,timeout:180000},async()=>{
 const directory=await mkdtemp('/tmp/plumdeck-mixed-runtime-');
 const connect=async(hostBinary,guestBinary)=>{
  const host=native(directory,hostBinary),guest=native(directory,guestBinary);
  try{
   await host.start();await guest.start();
   await host.command('junction.create',{djName:'Compatible host',sessionName:'Mixed release',programDevice:'-1',adoptCurrent:false,startInLobby:true,exchangeMode:'manual'});
   const invite=await newInvite(host);const answer=await response(guest,invite.text,'Compatible guest');
   await host.command('junction.exchange.import',{text:answer});await host.command('junction.peer.approve',{peerId:invite.peerId,accept:true});
   await until(host.snapshot,s=>s.connection.state==='connected'&&participant(s,invite.peerId),'host sees compatible peer');
   await until(guest.snapshot,s=>s.connection.state==='connected','guest sees compatible host');
  }finally{await Promise.allSettled([host.close(),guest.close()]);}
 };
 try{await connect(binary,previousBinary);await connect(previousBinary,binary);}finally{await rm(directory,{recursive:true,force:true});}
});
test('periodic roster snapshots keep avatar blobs off the heartbeat control path',async()=>{
 const source=await readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8');
 assert.match(source,/kSnapshotIntervalTicks=100/);
 assert.match(source,/ticks%kSnapshotIntervalTicks==0\s*&&\s*hosting\)broadcast\("session\.snapshot",wireState\(false\)\)/);
 assert.match(source,/queue\(p,"session\.snapshot",wireState\(true\)\)/);
 assert.match(source,/json\(s\)\.size\(\)>60\*1024/);
 assert.match(source,/"throughSeq",u64\(auth\.throughSeq\).*"bootstrap",true/);
 assert.doesNotMatch(source.slice(source.indexOf('if(op=="session.start")'),source.indexOf('if(op.startsWith("private."))')),/startDeadline|auth\.prepare/,'a fader-start session has no bootstrap deadline or prepared handoff');
 assert.match(source,/kControlSilenceNanos=10000000000LL/);
 assert.match(source,/queued\.type!=type[\s\S]*queued\.bytes=bytes;return/);
 assert.match(source,/ticks%200==0\)p->transport->sendKeepAlive\(\)/);
 assert.doesNotMatch(source,/ExchangeState::Interrupted\)[^\n]*discardAttempt/);
 assert.match(source,/void abortBootstrap[\s\S]*broadcast\("handoff\.cancel"[\s\S]*auth\.cancel\(\)[\s\S]*restoreLobby\(\);resetPreparation\(\);fail\(failure\)/);
 assert.equal(source.match(/abortBootstrap\(failure\);return;/g)?.length,2,'fence and durable-commit failures both return to a retryable lobby');
});
test('a refused first start validates the venue output before touching the shared order',async()=>{
 const source=await readFile(path.resolve(import.meta.dirname,'../../src/junction/runtime.cpp'),'utf8');
 const start=source.slice(source.indexOf('if(op=="session.start")'));const body=start.slice(0,start.indexOf('if(op.startsWith("private."))'));
 const reorder=body.indexOf('d->rosterOrder.prepend(target)');
 assert(reorder>0);assert(body.indexOf('d->programDevice<0')>=0&&body.indexOf('d->programDevice<0')<reorder,'unset output is refused first');
 assert(body.indexOf('d->openProgram()')<reorder,'output open failure is refused before reordering');
});
test('manual multi-DJ admission, cancellation, handoff and same-peer re-exchange without a signaling server',{timeout:420000},async()=>{
 const relay=Boolean(process.env.JUNCTION_TURN_ADDRESS);
 const directory=await mkdtemp('/tmp/plumdeck-manual-runtime-');const host=native(directory),first=native(directory),second=native(directory);const peers=[host,first,second];
 try{
  for(const peer of peers)await peer.start();console.info('native engines ready');
  if(relay){
    const secret=(await readFile(process.env.JUNCTION_TURN_SECRET_FILE,'utf8')).trim();
    const summary=await host.command('junction.network.configure',{stunUrls:[],turn:{mode:'rest',urls:['turn:'+process.env.JUNCTION_TURN_ADDRESS],secret},save:false});
    assert(!JSON.stringify(summary).includes(secret),'admin secret never returned');
    const probe=await host.command('junction.network.test');assert.equal(probe.state,'checking');
    const tested=await until(()=>host.command('junction.network.test',{poll:true}),s=>s.state!=='checking','actual TURN allocation',35000);assert.equal(tested.state,'success',tested.detail);
  }
  const source=path.join(directory,'tone.wav');await writeFile(source,tone());await host.command('deck.load',{deck:'A',track:{trackId:'manual-tone',path:source,title:'Manual session tone'}});await until(()=>host.command('state.snapshot'),s=>['ready','paused'].includes(s.decks.A.status),'tone decode');await host.command('deck.play',{deck:'A'});
  const runSeed=Date.now()%1000000007,remoteSource=path.join(directory,'remote-first.wav'),nextSource=path.join(directory,'remote-next.wav');await writeFile(remoteSource,remoteTone(120,runSeed));await writeFile(nextSource,remoteTone(20,runSeed+1));
  await first.command('deck.load',{deck:'A',track:{trackId:'bootstrap-tone',path:remoteSource,title:'Remote-first tone',artist:'Junction DJ'}});await until(()=>first.command('state.snapshot'),s=>['ready','paused'].includes(s.decks.A.status),'remote-first tone decode');await first.command('deck.play',{deck:'A'});
  const devices=await host.command('audio.devices.list');const output=devices.devices.find(d=>d.name.includes('BlackHole')&&d.outputChannels>=2);assert(output,'loopback device required, not skipped');
  const outputId=output.id.replace(/^coreaudio:/,'');const hostAvatar='data:image/png;base64,AA==',guestAvatar='data:image/webp;base64,AQ==';
  await host.command('junction.create',{djName:'セッション管理 DJ',avatarDataUrl:hostAvatar,sessionName:'リモートDJから開始',programDevice:outputId,adoptCurrent:false,startInLobby:true,exchangeMode:'manual'});
  let lobby=await until(host.snapshot,s=>s.active&&s.lifecycle==='lobby','server-free lobby');assert.equal(lobby.performerPeerId,'');assert.notEqual(lobby.program.state,'running');assert.equal(lobby.program.captureActive,false);assert.equal(participant(lobby,lobby.localPeerId).rosterStatus,'waiting');
  const firstSlot=await newInvite(host);const firstAnswer=await response(first,firstSlot.text,'最初のリモート DJ',{avatarDataUrl:guestAvatar});await host.command('junction.exchange.import',{text:firstAnswer});await host.command('junction.peer.approve',{peerId:firstSlot.peerId,accept:true});await until(first.snapshot,s=>s.connection.state==='connected'&&s.exchange.state==='connected','remote-first P2P connection and exchange state');
  assert(denied(await first.raw('junction.session.start',{performerPeerId:firstSlot.peerId})),'guest cannot start the session');assert(denied(await first.raw('junction.roster.reorder',{peerIds:[]})),'guest cannot reorder the lobby');
  await pause(700);lobby=await first.snapshot();assert.equal(participant(lobby,lobby.hostPeerId).avatarDataUrl,hostAvatar,'cached profile survives lightweight heartbeats');assert.equal(participant(await host.snapshot(),firstSlot.peerId).avatarDataUrl,guestAvatar);
  // The remote first DJ is simply first in the timetable: STANDBY from a silent J, ON AIR by raising a fader.
  await first.command('mixer.channel.gain',{deck:'A',gain:0});
  const starting=await host.command('junction.session.start',{performerPeerId:firstSlot.peerId});assert.equal(starting.lifecycle,'live');assert.equal(starting.performerPeerId,'');assert.equal(starting.program.state,'running');assert.equal(starting.program.captureActive,false,'coordinator capture stays off until someone is on air');
  await until(first.snapshot,s=>s.turn.signal==='ready','remote first DJ READY',60000);
  await first.command('mixer.channel.gain',{deck:'A',gain:1});
  const firstLive=await until(first.snapshot,s=>s.lifecycle==='live'&&s.performerPeerId===s.localPeerId,'remote DJ becomes first performer',20000);assert(BigInt(firstLive.epoch)>1n);await until(host.snapshot,s=>s.lifecycle==='live'&&s.performerPeerId===firstSlot.peerId,'coordinator observes remote-first live');const retained=await first.command('state.snapshot');assert.equal(retained.decks.A.track.title,'Remote-first tone','the first DJ keeps its own decks');
  // J metadata instead of files: the coordinator, next in the timetable, sees the
  // performer's track on J; nothing is transferred and its own decks stay as they are.
  const listed=await until(host.snapshot,s=>s.junctionInput?.decks?.some(d=>d.title==='Remote-first tone'&&d.role==='current'&&d.playing),'the ON AIR track appears on the coordinator J',30000);
  assert.equal(listed.junctionInput.peerId,firstSlot.peerId);assert.deepEqual(listed.junctionTracks,[],'no Junction Live file transfer in a fader-start session');
  assert(!JSON.stringify(listed).includes(remoteSource),'the sender path is never exposed');
  let hostDecks=await host.command('state.snapshot');assert.equal(hostDecks.decks.A.track.title,'Manual session tone','existing coordinator deck is not replaced');assert.equal(hostDecks.decks.A.status,'playing');assert.equal(hostDecks.decks.B.track,null,'no automatic deck load');
  assert(denied(await host.raw('junction.tracks.load',{assetId:'0'.repeat(64),deck:'B'})),'there is deliberately no monitor deck-load operation');
  let monitoredSession=await host.snapshot();assert.equal(monitoredSession.performerPeerId,firstSlot.peerId,'watching J never changes the performer');assert.equal(monitoredSession.epoch,firstLive.epoch);assert.equal(monitoredSession.handoffState,'playing');
  await until(host.snapshot,s=>Number(s.program.rms)>.001,'Program keeps carrying the remote performer',10000);
  const beforePosition=listed.junctionInput.decks.find(d=>d.role==='current').positionMs;await until(host.snapshot,s=>s.junctionInput?.decks?.find(d=>d.role==='current')?.positionMs>beforePosition+200,'position metadata follows without a deck seek');
  await first.command('deck.load',{deck:'B',track:{trackId:'remote-next',path:nextSource,title:'Remote next tone',artist:'Junction DJ'}});await until(()=>first.command('state.snapshot'),s=>['ready','paused'].includes(s.decks.B.status),'performer loads a next track');
  await until(host.snapshot,s=>s.junctionInput?.decks?.some(d=>d.title==='Remote next tone'&&d.role==='next'&&d.deck==='B'),'the performer next track is listed on J');
  await first.command('deck.unload',{deck:'B'});
  await until(host.snapshot,s=>!s.junctionInput?.decks?.some(d=>d.title==='Remote next tone'),'performer deck.unload removes the next row');
  assert.equal((await host.command('state.snapshot')).decks.B.track,null,'performer changes never load coordinator decks');
  const liveRoster=await host.snapshot();assert.notEqual(participant(liveRoster,liveRoster.localPeerId).rosterStatus,'finished','never-playing coordinator is not marked finished');
  await host.command('junction.end');const ended=await until(host.snapshot,s=>!s.active,'remote-first host end');assert.deepEqual(ended.junctionTracks,[],'Junction Live disappears with the session');await until(first.snapshot,s=>!s.active,'remote-first guest end');
  await host.command('junction.create',{displayName:'ホスト DJ',sessionName:'手動でつなぐセッション',programDevice:output.id.replace(/^coreaudio:/,''),adoptCurrent:true,exchangeMode:'manual'});
  const created=await until(host.snapshot,s=>s.active&&s.program.state==='running','server-free host and Program');assert.equal(created.exchange.mode,'manual');assert.equal(created.lifecycle,'live','legacy create remains immediately live');assert.equal(created.coordinatorPeerId,created.hostPeerId);assert.equal(created.performerPeerId,created.localPeerId);const epoch=created.epoch;
  console.info('manual host ready');const one=await newInvite(host),two=await newInvite(host);console.info('two invitations collected');assert.notEqual(one.peerId,two.peerId);let invited=await host.snapshot();for(const id of [one.peerId,two.peerId]){const row=participant(invited,id);assert.equal(row.slotId,id);assert(row.invitationId);assert.equal(row.isPlaceholder,true);assert.equal(row.rosterStatus,'invited');assert(Number.isInteger(row.orderIndex));}
  await host.command('junction.roster.reorder',{peerIds:[created.localPeerId,two.peerId,one.peerId]});invited=await host.snapshot();assert.deepEqual(invited.participants.map(row=>row.peerId),[created.localPeerId,two.peerId,one.peerId]);assert(denied(await host.raw('junction.roster.reorder',{peerIds:[created.localPeerId,one.peerId,one.peerId]})),'duplicate roster member rejected');
  if(relay){const packet=JSON.parse(Buffer.from(one.text.split('.')[1],'base64url'));assert(packet.iceServers.some(s=>s.credential&&s.expiresAt>Date.now()));assert(!packet.iceServers.some(s=>'secret' in s));}
  const inspected=await first.command('junction.exchange.inspect',{text:one.text});assert.equal(inspected.sessionName,'手動でつなぐセッション');
  assert(denied(await first.raw('junction.exchange.inspect',{text:one.text.slice(0,Math.floor(one.text.length/2))})),'truncated invitation rejected');
  const answerOne=await response(first,'  '+one.text+'\n','参加 DJ 1'),answerTwo=await response(second,two.text,'参加 DJ 2');
  assert(denied(await host.raw('junction.exchange.import',{text:answerTwo,peerId:one.peerId})),'response for another row is rejected without changing either attempt');
  await host.command('junction.exchange.import',{text:answerTwo,peerId:two.peerId});await host.command('junction.exchange.import',{text:answerOne,peerId:one.peerId});
  let s=await host.snapshot();for(const id of [one.peerId,two.peerId]){assert.equal(participant(s,id).exchange.state,'approval_pending');assert.equal(participant(s,id).approved,false);assert.equal(participant(s,id).slotId,id);}assert.equal(participant(s,one.peerId).djName,'参加 DJ 1');assert.equal(participant(s,two.peerId).djName,'参加 DJ 2');assert.equal(s.performerPeerId,created.localPeerId);assert.equal(s.epoch,epoch);
  await host.raw('junction.exchange.import',{text:answerTwo});s=await host.snapshot();assert.equal(participant(s,two.peerId).exchange.state,'approval_pending','duplicate import cannot connect or reset card');
  await host.command('junction.invite.cancel',{peerId:one.peerId});assert(denied(await host.raw('junction.exchange.import',{text:answerOne})),'cancelled invitation refuses stale response');
  s=await host.snapshot();assert.equal(participant(s,two.peerId).exchange.state,'approval_pending','other invitation preserved');
  const notice=participant(s,one.peerId).exchange.noticeText;assert(notice,'signed offline cancellation available');if(notice){await first.command('junction.exchange.import',{text:notice});assert(['cancelled','rejected','expired'].includes((await first.snapshot()).exchange.state));}
  await host.command('junction.peer.approve',{peerId:two.peerId,accept:true});await until(second.snapshot,s=>s.connection.state==='connected'&&s.exchange.state==='connected','approved P2P connection and exchange state');
  assert(denied(await second.raw('junction.roster.reorder',{peerIds:[]})),'guest cannot reorder roster');
  const measured=await until(host.snapshot,s=>participant(s,two.peerId)?.connectionQuality?.level!=='unknown','connection quality report',8000);const quality=participant(measured,two.peerId).connectionQuality;assert(['good','fair','poor'].includes(quality.level));assert(quality.rttMs>=0&&quality.rttMs<=600000);assert(quality.packetLossPct>=0&&quality.packetLossPct<=100);
  s=await host.snapshot();assert.equal(participant(s,two.peerId).exchange.route,relay?'relay':'direct');assert.equal(s.performerPeerId,created.localPeerId);assert.equal(s.epoch,epoch);assert.notEqual((await second.snapshot()).turn.signal,'onair','connection alone never puts a DJ on air');
  const query=await second.raw('engine.clock.probe');assert(!denied(query),'waiting DJ can inspect timing');assert(!denied(await second.raw('mixer.channel.gain',{deck:'A',gain:0})),'a waiting DJ prepares freely on their own mixer');
  const retry=await newInvite(host,one.peerId);assert.notEqual(retry.text,one.text);await first.command('junction.exchange.import',{text:retry.text});const newAnswer=await until(first.snapshot,s=>s.exchange?.responseText?.length>0,'replacement response');await host.command('junction.exchange.import',{text:newAnswer.exchange.responseText});await host.command('junction.peer.approve',{peerId:one.peerId,accept:true});await until(first.snapshot,s=>s.connection.state==='connected','second independent guest');
  await until(host.snapshot,s=>participant(s,one.peerId).exchange.state==='connected','both first guest channels connected');
  // Program plays one venue delay (1 s) behind the session start; record once it carries sound.
  await until(host.snapshot,s=>Number(s.program.rms)>.001,'Program carries the host before recording',10000);
  const recordedAt=Date.now();const recording=path.join(directory,'program.wav');await host.command('junction.program.record.start',{path:recording});first.suspend();await pause(5000);first.resume();await pause(600);assert.equal(participant(await host.snapshot(),one.peerId).exchange.state,'connected','a screen-sharing-sized scheduling pause stays connected');first.suspend();await until(host.snapshot,s=>participant(s,one.peerId).exchange.state==='interrupted','detect a real transient interruption',15000);first.resume();await until(host.snapshot,s=>participant(s,one.peerId).exchange.state==='connected','transient communication returns');assert.equal((await host.snapshot()).performerPeerId,created.localPeerId,'transient interruption never transfers ownership');
  assert(denied(await host.raw('junction.handoff.request',{targetPeerId:two.peerId})),'the nomination handoff is retired');
  // Fader start: the second DJ is next in the timetable, prepares with the fader down, then raises it.
  const secondSource=path.join(directory,'second.wav');await writeFile(secondSource,remoteTone(200,runSeed+7));
  await second.command('mixer.channel.gain',{deck:'A',gain:0});await second.command('deck.load',{deck:'A',track:{trackId:'second-tone',path:secondSource,title:'Second tone'}});await until(()=>second.command('state.snapshot'),s=>['ready','paused'].includes(s.decks.A.status),'second tone decode');await second.command('deck.play',{deck:'A'});
  await until(host.snapshot,s=>s.turn.nextPeerId===two.peerId,'second DJ is next');
  await until(second.snapshot,s=>s.turn.signal==='ready'&&s.junctionInput.receiving,'second DJ READY on the host J relay',60000);
  await second.command('mixer.channel.gain',{deck:'A',gain:1});
  const owner=await until(second.snapshot,s=>s.performerPeerId===s.localPeerId&&BigInt(s.epoch)>BigInt(epoch),'fader start puts the second DJ on air',20000);
  await until(host.snapshot,s=>s.performerPeerId===two.peerId&&s.turn.outgoingPeerId===s.localPeerId&&s.turn.tailDecks.includes('A'),'host sends its tail');
  await second.command('junction.input.set',{volume:0});
  await until(host.snapshot,s=>s.turn.outgoingPeerId==='','the new DJ fades the host tail out',15000);
  const afterHandoff=await host.snapshot();assert.equal(participant(afterHandoff,created.localPeerId).rosterStatus,'finished');assert.deepEqual(afterHandoff.participants.map(row=>row.peerId),[two.peerId,one.peerId,created.localPeerId],'current, waiting, then finished order is preserved');
  await until(host.snapshot,s=>s.turn.nextPeerId===one.peerId,'the next queued DJ becomes STANDBY after the release');
  await until(first.snapshot,s=>s.junctionInput.receiving,'guest to guest J relays through the host',20000);
  console.info('re-exchange begins seconds', (Date.now()-recordedAt)/1000);const reconnect=await newInvite(host,two.peerId);await second.command('junction.exchange.import',{text:reconnect.text});const fresh=await until(second.snapshot,s=>s.exchange?.responseText&&s.exchange.responseText!==answerTwo,'live replacement response');await host.command('junction.exchange.import',{text:fresh.exchange.responseText});await host.command('junction.peer.approve',{peerId:two.peerId,accept:true});await until(host.snapshot,s=>participant(s,two.peerId)?.exchange.state==='connected','replacement P2P connection');
  s=await host.snapshot();assert.equal(s.performerPeerId,two.peerId);assert.equal(s.epoch,owner.epoch,'reconnection never changes epoch');assert.equal(participant(s,one.peerId).exchange.state,'connected','other DJ unaffected by re-exchange');assert(denied(await host.raw('junction.exchange.import',{text:answerTwo})),'old response cannot replace new connection');
  await pause(700);await host.command('junction.program.record.stop');console.info('recording ends seconds',(Date.now()-recordedAt)/1000);recordedPcm(await readFile(recording));second.suspend();await pause(650);second.resume();await until(host.snapshot,s=>s.handoffState==='playing'&&s.performerPeerId===two.peerId&&Number(s.program.rms)>.001,'current performer resumes after brief outage');assert.equal((await host.snapshot()).epoch,owner.epoch);
  first.suspend();await until(host.snapshot,s=>participant(s,one.peerId).exchange.state==='interrupted','prolonged outage enters automatic recovery',15000);await pause(17000);const kept=await host.snapshot();assert.equal(participant(kept,one.peerId).exchange.state,'interrupted','the old 15 second deadline no longer destroys a recoverable connection');assert.equal(kept.performerPeerId,two.peerId);assert.equal(kept.epoch,owner.epoch);assert.equal(participant(kept,two.peerId).exchange.state,'connected');
  const renewed=await newInvite(host,one.peerId);first.resume();await first.command('junction.exchange.import',{text:renewed.text});const renewedReply=await until(first.snapshot,s=>s.exchange?.responseText,'new reply after prolonged outage');await host.command('junction.exchange.import',{text:renewedReply.exchange.responseText});await host.command('junction.peer.approve',{peerId:one.peerId,accept:true});await until(host.snapshot,s=>participant(s,one.peerId).exchange.state==='connected'&&['waiting','next','ready'].includes(participant(s,one.peerId).rosterStatus),'same peer after prolonged outage');await until(first.snapshot,s=>s.exchange.state==='connected'&&s.connection.state==='connected','guest sees restored connection');await pause(3500);assert.equal(participant(await host.snapshot(),one.peerId).exchange.state,'connected','old heartbeat does not immediately interrupt the fresh link');
  await host.command('junction.end');await until(host.snapshot,s=>!s.active,'graceful host end');for(const guest of [first,second])await until(guest.snapshot,s=>!s.active,'guest end');
 }finally{const closed=await Promise.allSettled(peers.map(peer=>peer.close()));await rm(directory,{recursive:true,force:true});for(const result of closed)if(result.status==='rejected')throw result.reason;}
});
