// Local visual/interaction fixture. No native commands, sessions or remote peers.
import React, {useState} from 'react';
import {createRoot} from 'react-dom/client';
import {JunctionPanel} from '../../src/components/junction/JunctionPanel';
import {junctionState} from '../../src/services/junction/state';
import {endLink, publishSession, refreshAccount} from '../../src/services/junction/share-musics/coordinator';
import '../../src/App.css';
import '../../src/index.css';
const states = ['invite_ready', 'approval_pending', 'connected', 'interrupted', 'needs_exchange', 'failed', 'response_ready', 'no_session'];
// share-musics: only the Tauri bridge commands for it are answered here, from fixed data.
// Native commands fail and nothing leaves this page.
const shareMember = (peerId: string, displayName: string, status: string, client: string, role = 'guest') => ({peerId, displayName, status, client, role, owner: false});
let shareRole: 'host' | 'guest' = 'host';
async function shareMusicsBridge(command: string, args: {action?: string} = {}) {
  if (command === 'share_musics_status' || command === 'share_musics_login') return {available: true, signedIn: true, email: 'yuta@example.com', serviceUrl: 'fixture'};
  if (command !== 'share_musics_request') throw new Error('fixture: native commands are unavailable');
  switch (args.action) {
    case 'listJunctionSessions': return [
      {id: 'kenji', name: 'Saturday Night', hostName: 'DJ KENJI', hostClient: 'desktop', participantCount: 2, mine: false, createdAt: ''},
      {id: 'aoi', name: 'Chill Session', hostName: 'DJ AOI', hostClient: 'lite', participantCount: 1, mine: false, createdAt: ''},
    ];
    case 'createJunctionSession': shareRole = 'host'; return {sessionId: 'fixture-session', peerId: 'me'};
    case 'joinJunctionSession': shareRole = 'guest'; return {sessionId: 'kenji', peerId: 'me', role: 'guest'};
    case 'getJunctionSession': return shareRole === 'host'
      ? {session: {id: 'fixture-session', name: 'Friday Session', hostPeerId: 'me', ownerPeerId: 'me', status: 'open', members: [
          shareMember('me', 'DJ YUTA', 'approved', 'desktop', 'host'), shareMember('mika', 'DJ MIKA', 'approved', 'desktop'),
          shareMember('sora', 'DJ SORA', 'pending', 'desktop'), shareMember('aoi', 'DJ AOI', 'pending', 'lite')]},
        me: {peerId: 'me', role: 'host', status: 'approved', client: 'desktop'}, signals: []}
      : {session: {id: 'kenji', name: 'Saturday Night', hostPeerId: 'kenji', ownerPeerId: 'kenji', status: 'open', members: []},
        me: {peerId: 'me', role: 'guest', status: 'pending', client: 'desktop'}, signals: []};
    default: return {ok: true, signalId: 1};
  }
}
function setShareMusics(enabled: boolean) {
  const target = globalThis as unknown as {isTauri?: boolean; __TAURI_INTERNALS__?: unknown};
  if (enabled) { target.isTauri = true; target.__TAURI_INTERNALS__ = {invoke: shareMusicsBridge}; void refreshAccount(); }
  else { delete target.isTauri; delete target.__TAURI_INTERNALS__; void endLink(); }
}
function Fixture() {
  const [host, setHost] = useState(true), [state, setState] = useState('invite_ready'), [attempt, setAttempt] = useState(1), [share, setShare] = useState(false);
  React.useEffect(() => {
    if (state === 'no_session') {
      junctionState.set({active: false, sessionId: null, revision: attempt, epoch: '0', localPeerId: '', hostPeerId: '', performerPeerId: '', handoffState: 'IDLE', participants: [], readiness: {ready: false, reasons: []}, program: {state: 'idle'}, connection: {state: 'disconnected'}} as never);
      return;
    }
    const exchange = {state, inviteId: `attempt-${attempt}`, waitingFor: 'none', ...(host ? {inviteText: `PLUMDECK-JUNCTION-test-invite-${attempt}`} : {})};
    junctionState.set({active: true, sessionId: 'fixture', sessionName: 'Friday Session', revision: attempt, epoch: '1', localPeerId: host ? 'host' : 'guest', hostPeerId: 'host', performerPeerId: '', lifecycle: 'lobby', handoffState: 'IDLE', participants: [
      {peerId: 'host', djName: 'DJ YUTA', displayName: 'DJ YUTA', approved: true, isHost: true, orderIndex: 0, ...(host ? {} : {exchange})},
      {peerId: 'guest', djName: 'DJ MIKA', displayName: 'DJ MIKA', approved: state === 'connected', orderIndex: 1, ...(host ? {exchange} : {})},
    ], readiness: {ready: false, reasons: []}, program: {state: 'idle'}, connection: {state: state === 'connected' ? 'connected' : 'pending'}, exchange: {mode: 'manual', state, inviteId: `attempt-${attempt}`, responseText: `PLUMDECK-JUNCTION-test-answer-${attempt}`}} as never);
  }, [host, state, attempt]);
  return <><aside style={{position:'fixed',left:20,top:20,color:'white',display:'grid',gap:15}}>
    <h1>Junction UI check</h1><label>表示する側<select aria-label="表示する側" value={host ? 'host' : 'guest'} onChange={(e) => {setHost(e.target.value === 'host');setState(e.target.value === 'host' ? 'invite_ready' : 'response_ready');}}><option value="host">管理DJ</option><option value="guest">参加DJ</option></select></label>
    <label>接続状態<select aria-label="接続状態" value={state} onChange={(e) => setState(e.target.value)}>{states.map(s => <option key={s}>{s}</option>)}</select></label>
    <button onClick={() => {setAttempt(a => a + 1);setState(host ? 'invite_ready' : 'response_ready');}}>新しい招待の状態にする</button>
    <label className="junction-check"><input type="checkbox" checked={share} onChange={(e) => {setShare(e.target.checked);setShareMusics(e.target.checked);}} />PlumDeck Liteにログイン済み</label>
    {share && host && state !== 'no_session' && <button onClick={() => void publishSession('Friday Session', 'DJ YUTA')}>PlumDeck Liteに公開済みにする</button>}
    <p>テスト用データです。相手への送信・接続は行いません。</p>
  </aside><JunctionPanel open onClose={() => {}} incomingInvite={null} onConsumeIncoming={() => {}} /></>;
}
createRoot(document.getElementById('root')!).render(<Fixture/>);
