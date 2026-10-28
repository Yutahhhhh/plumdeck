import assert from "node:assert/strict";
import test from "node:test";

import { DEFAULT_KEY_NOTATION, KEYS, TRAFFIC_LIGHT, formatKeyId, keyRelation } from "./keys.generated.ts";

test("generated key tables cover every key and relation", () => {
  assert.equal(KEYS.length, 24);
  assert.equal(DEFAULT_KEY_NOTATION, "alphanumeric");
  const am = KEYS.findIndex((key) => key.classic === "Am");
  assert.equal(formatKeyId(am, "alphanumeric"), "8A");
  assert.equal(formatKeyId(am, "open_key"), "1m");
  const c = KEYS.findIndex((key) => key.classic === "C");
  assert.equal(keyRelation(am, c), "relative");
  assert.equal(keyRelation(am, am), "same");
  assert.equal(TRAFFIC_LIGHT[keyRelation(am, KEYS.findIndex((key) => key.alphanumeric === "9B"))], 2);
  assert.equal(formatKeyId(99, "classic"), "");
});
