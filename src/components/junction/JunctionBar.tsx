import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { useJunction } from '@/hooks/useJunction';
import { junctionState } from '@/services/junction/state';
import { junctionCommand } from '@/services/junction/client';
import { djEngineClient } from '@/services/dj-engine/client';
import { startShareMusics } from '@/services/junction/share-musics/coordinator';
import { JunctionRootButton } from './JunctionRootButton';
import { JunctionPanel } from './JunctionPanel';
import './junction.css';

/**
 * Root Junction control in the top bar plus the nonmodal side panel. The panel
 * stays mounted (only visually toggled) so no in-progress exchange or form state
 * is lost when it is closed, and the decks behind it remain fully usable.
 */
export function JunctionBar() {
  const s = useJunction();
  const [open, setOpen] = useState(false);
  const [incomingInvite, setIncomingInvite] = useState<string | null>(null);
  const [notice, setNotice] = useState('');

  useEffect(() => {
    if (!isTauri()) return;
    startShareMusics();
    let live = true;
    let running = false;
    const poll = async () => {
      if (running) return;
      running = true;
      try {
        if ((await djEngineClient.status()).running && live) await junctionCommand('snapshot');
      } catch {
        junctionState.unavailable();
      } finally {
        running = false;
      }
    };
    void invoke<string | null>('junction_pending_invite').then((value) => {
      if (value && live) { setIncomingInvite(value); setOpen(true); }
    });
    void poll();
    const timer = setInterval(() => void poll(), 1000);
    const inviteListener = listen<string>('junction://invite', (e) => {
      setIncomingInvite(e.payload);
      setOpen(true);
    });
    const closeBlocked = listen('junction://close-blocked', () => {
      setNotice('アプリを終了する前に、引き継ぎ・退出、またはセッション終了を完了してください。');
      setOpen(true);
    });
    return () => {
      live = false;
      clearInterval(timer);
      void inviteListener.then((fn) => fn());
      void closeBlocked.then((fn) => fn());
    };
  }, []);

  return (
    <>
      <JunctionRootButton snapshot={s} open={open} onToggle={() => setOpen((v) => !v)} />
      <JunctionPanel
        open={open}
        onClose={() => { setOpen(false); requestAnimationFrame(() => document.getElementById("junction-toggle")?.focus()); }}
        incomingInvite={incomingInvite}
        onConsumeIncoming={() => setIncomingInvite(null)}
        notice={notice}
        onDismissNotice={() => setNotice('')}
      />
    </>
  );
}
