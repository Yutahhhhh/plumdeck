import assert from "node:assert/strict";
import test from "node:test";
import { createKeyboardRuntime, dispatchKeyboardLoad } from "./keyboard-runtime.ts";

test("Performance 1 dispatches once per physical press, including non-momentary keys", () => {
  const actions = [];
  const runtime = createKeyboardRuntime(() => "performance1", action => { actions.push(action); });
  assert.equal(runtime.keyDown({ code: "KeyZ" }), true);
  assert.equal(runtime.keyDown({ code: "KeyZ", repeat: true }), true);
  assert.deepEqual(actions, [{ kind: "play", side: 0 }]);
  assert.equal(runtime.keyUp("KeyZ"), true);
  runtime.keyDown({ code: "KeyZ" });
  assert.equal(actions.length, 2);
});

test("focus guards, modified keys and repeats without a press never operate a deck", () => {
  const runtime = createKeyboardRuntime(() => "classic", () => { assert.fail("unexpected action"); });
  for (const event of [{ code: "KeyQ", metaKey: true }, { code: "KeyQ", ctrlKey: true }, { code: "KeyE", altKey: true }, { code: "Space", repeat: true }]) {
    assert.equal(runtime.keyDown(event), false);
  }
  assert.equal(runtime.keyDown({ code: "Space" }, true), false);
});

test("a held action releases using its original context after a preset change", () => {
  let preset = "performance1", released = 0;
  const runtime = createKeyboardRuntime(() => preset, action => {
    assert.deepEqual(action, { kind: "bend", side: 0, direction: -1 });
    return () => { released++; };
  });
  runtime.keyDown({ code: "KeyW" });
  preset = "classic";
  assert.equal(runtime.keyUp("KeyW"), true);
  assert.equal(runtime.keyUp("KeyW"), false);
  assert.equal(released, 1);
});

test("blur/unmount releases every held action exactly once", () => {
  const releases = [];
  const runtime = createKeyboardRuntime(() => "performance1", action => () => releases.push(action.kind));
  runtime.keyDown({ code: "KeyS" });
  runtime.keyDown({ code: "KeyW" });
  runtime.releaseAll();
  runtime.releaseAll();
  assert.equal(runtime.keyUp("KeyS"), false);
  assert.deepEqual(releases, ["slipReverse", "bend"]);
});

test("unavailable actions do not consume keys or create a held action", () => {
  let ready = false, actions = 0;
  const runtime = createKeyboardRuntime(() => "performance1", () => {
    if (!ready) return false;
    actions++;
  });
  assert.equal(runtime.keyDown({ code: "KeyZ" }), false);
  assert.equal(runtime.keyUp("KeyZ"), false);
  ready = true;
  assert.equal(runtime.keyDown({ code: "KeyZ" }), true);
  assert.equal(actions, 1);
});


test("an OS modifier chord is not consumed when its plain key was already held", () => {
  let released = false;
  const runtime = createKeyboardRuntime(() => "classic", () => () => { released = true; });
  assert.equal(runtime.keyDown({ code: "KeyQ" }), true);
  assert.equal(runtime.keyDown({ code: "KeyQ", metaKey: true }), false);
  assert.equal(runtime.keyUp("KeyQ"), true);
  assert.equal(released, true);
});


test("a failed release cannot leave other held controls latched", () => {
  let released = 0;
  const runtime = createKeyboardRuntime(() => "performance1", action => () => {
    if (action.kind === "slipReverse") throw new Error("route denied");
    released++;
  });
  runtime.keyDown({ code: "KeyS" });
  runtime.keyDown({ code: "KeyW" });
  assert.throws(() => runtime.releaseAll(), /route denied/);
  assert.equal(released, 1);
  runtime.releaseAll();
  assert.equal(released, 1);
});


test("load shortcuts dispatch once per press with the correct deck", () => {
  const commands = [];
  const keyboard = createKeyboardRuntime(() => "performance1", action => {
    if (action.kind === "load") dispatchKeyboardLoad(action, command => commands.push(command));
  });
  assert.equal(keyboard.keyDown({ code: "ArrowLeft", shiftKey: true }), true);
  assert.equal(keyboard.keyDown({ code: "ArrowLeft", shiftKey: true, repeat: true }), true);
  assert.equal(keyboard.keyUp("ArrowLeft"), true);
  keyboard.keyDown({ code: "ArrowRight", shiftKey: true });
  keyboard.keyUp("ArrowRight");
  assert.deepEqual(commands, [
    { control: "load", deck: "A", value: 1, pressed: true },
    { control: "load", deck: "B", value: 1, pressed: true },
  ]);
});
