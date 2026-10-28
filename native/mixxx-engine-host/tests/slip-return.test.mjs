// Deterministic state test: no host process, audio output, or music files.
import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const header = path.resolve(import.meta.dirname, "../src/slip_return.h");
test("interrupting Slip return invalidates the old token and re-arms when needed", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "plumdeck-slip-return-"));
  try {
    const source = path.join(dir, "test.cpp"), binary = path.join(dir, "test");
    writeFileSync(source, `#include <cassert>
#include <cstdint>
#include "${header}"
int main() {
  // Old timer has switched Slip off. A new gesture must invalidate it and
  // restore the control before starting, regardless of the kind of gesture.
  for (int gesture = 0; gesture < 4; ++gesture) {
    uint64_t generation = 4, oldToken = generation; bool wanted = true, enabled = false;
    if (interruptSlipReturn(generation, wanted, enabled)) enabled = true;
    assert(enabled && generation != oldToken);
    if (generation == oldToken) enabled = false; // stale callback must not run
    assert(enabled);
  }
  uint64_t generation = 0;
  assert(!interruptSlipReturn(generation, false, false)); // explicitly off stays off
  assert(!interruptSlipReturn(generation, true, true)); // no redundant write
  assert(generation == 2);
}
`);
    execFileSync(process.env.CXX || "c++", ["-std=c++17", source, "-o", binary]);
    execFileSync(binary);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
