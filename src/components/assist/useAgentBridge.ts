import { useCallback, useEffect, useRef, useState } from "react";
import { assistService, type AssistAgentList, type AssistAgentSettings, type AssistWindowState } from "@/services/assist";

const POLL_MS = 1500;
const HEARTBEAT_MS = 10_000;
const REPORT_DEBOUNCE_MS = 300;

// Survives the assist view unmounting, so settings an agent sent once are not
// re-applied every time the DJ comes back to this window.
let appliedSettingsRevision = 0;

/**
 * The MCP side of assist. The window reports what it shows; an agent (Claude
 * Code, Codex…) reads that through plumdeck's MCP tools and answers by changing
 * the window's settings or by putting its own picks on screen.
 */
/** `onSettings` returns false when it cannot apply them yet (mid-drag); they are retried. */
export function useAgentBridge(state: AssistWindowState, onSettings: (settings: AssistAgentSettings) => boolean) {
  const [list, setList] = useState<AssistAgentList | null>(null);
  const [lastSettings, setLastSettings] = useState<AssistAgentSettings | null>(null);
  const stateKey = JSON.stringify(state);
  const latestState = useRef(state);
  const applySettings = useRef(onSettings);
  const dismissed = useRef(0);
  latestState.current = state;
  applySettings.current = onSettings;

  useEffect(() => {
    const timer = window.setTimeout(() => { void assistService.reportWindow(latestState.current).catch(() => undefined); }, REPORT_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [stateKey]);

  useEffect(() => {
    const report = () => { void assistService.reportWindow(latestState.current).catch(() => undefined); };
    const timer = window.setInterval(report, HEARTBEAT_MS);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    let alive = true;
    const poll = async () => {
      try {
        const updates = await assistService.agentUpdates();
        if (!alive) return;
        if (updates.revision < Math.max(appliedSettingsRevision, dismissed.current)) {
          // The backend restarted and its in-memory revisions began again.
          appliedSettingsRevision = 0;
          dismissed.current = 0;
        }
        const settings = updates.settings;
        if (settings && settings.revision > appliedSettingsRevision && applySettings.current(settings)) {
          appliedSettingsRevision = settings.revision;
          setLastSettings(settings);
        }
        const next = updates.list && updates.list.revision > dismissed.current ? updates.list : null;
        setList(current => (current?.revision === next?.revision ? current : next));
      } catch {
        // The backend may be restarting; the next poll retries.
      }
    };
    void poll();
    const timer = window.setInterval(() => { void poll(); }, POLL_MS);
    return () => { alive = false; window.clearInterval(timer); };
  }, []);

  const dismiss = useCallback(async () => {
    setList(current => {
      if (current) dismissed.current = Math.max(dismissed.current, current.revision);
      return null;
    });
    await assistService.dismissAgentList().catch(() => undefined);
  }, []);

  return { list, lastSettings, dismiss };
}
