import { resolveShortcut, type KeyInput, type ShortcutAction, type ShortcutPreset } from "./keyboard-shortcuts.ts";

export type ShortcutPress = (action: ShortcutAction) => (() => void) | void | false;

/** Own each physical key until key-up, even if focus or the preset changes. */
export function createKeyboardRuntime(preset: () => ShortcutPreset, press: ShortcutPress) {
  const held = new Map<string, (() => void) | void>();
  return {
    keyDown(event: KeyInput & { repeat?: boolean }, blocked = false): boolean {
      if (blocked || event.altKey || event.ctrlKey || event.metaKey) return false;
      if (held.has(event.code)) return true;
      if (event.repeat) return false;
      const action = resolveShortcut(preset(), event);
      if (!action) return false;
      const release = press(action);
      if (release === false) return false;
      held.set(event.code, release);
      return true;
    },
    keyUp(code: string): boolean {
      if (!held.has(code)) return false;
      const release = held.get(code);
      held.delete(code);
      release?.();
      return true;
    },
    releaseAll() {
      const releases = [...held.values()];
      held.clear();
      const failures: unknown[] = [];
      for (const release of releases) {
        try { release?.(); } catch (error) { failures.push(error); }
      }
      if (failures.length) throw failures[0];
    },
  };
}

/** Route a load shortcut through the controller's playing-deck and Junction guards. */
export function dispatchKeyboardLoad(action: Extract<ShortcutAction, { kind: "load" }>, dispatch: (command: { control: "load"; deck: "A" | "B"; value: number; pressed: boolean }) => void) {
  dispatch({ control: "load", deck: action.side === 0 ? "A" : "B", value: 1, pressed: true });
}
