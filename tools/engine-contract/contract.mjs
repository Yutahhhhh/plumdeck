// Engine contract v1, machine-readable (engine-and-junction.md W2-01).
//
//   node --experimental-strip-types tools/engine-contract/contract.mjs          # check
//   node --experimental-strip-types tools/engine-contract/contract.mjs --write  # regenerate
//
// Collects what each side of the NDJSON protocol names today and writes
// contracts/dj-engine/v1/contract.json:
// - the client's constants (src/types/dj-engine.ts: DJ_ENGINE_OPS, _EVENTS,
//   _CAPABILITIES, _RANGES, error codes, enums);
// - the Mixxx host (native/mixxx-engine-host/src/host.cpp: its accepted op
//   sets and dispatch, the
//   waveform commands in mixxx_backend.cpp, the Junction runtime's
//   sub-commands under `junction.`, its events, declared capabilities and
//   error codes);
// - the Rust simulator (native/dj-engine-host/src/protocol.rs `op` and
//   `event` modules, the capabilities in engine.rs).
// Every op, event and capability is classified (category = its first name
// segment, the capability that gates it, who implements it). Check mode
// rebuilds the file and fails on any difference, so an op added or renamed
// on any side without updating the contract is caught in CI.

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const read = (path) => readFileSync(join(root, path), "utf8");
const OUT = "contracts/dj-engine/v1/contract.json";

const types = await import(join(root, "src/types/dj-engine.ts"));

const sorted = (values) => [...new Set(values)].sort();
const all = (pattern, text, group = 1) => [...text.matchAll(pattern)].map((match) => match[group]);

function functionBody(text, signature) {
  const start = text.indexOf(signature);
  if (start < 0) throw new Error(`missing ${signature}`);
  let depth = 0;
  for (let at = text.indexOf("{", start); at < text.length; at++) {
    if (text[at] === "{") depth++;
    if (text[at] === "}" && --depth === 0) return text.slice(start, at + 1);
  }
  throw new Error(`unterminated ${signature}`);
}

// ------------------------------------------------------------------ client
const clientErrors = (() => {
  const text = read("src/types/dj-engine.ts");
  const union = /export type EngineErrorCode\s*=([\s\S]*?);/.exec(text);
  if (!union) throw new Error("EngineErrorCode union not found");
  return sorted(all(/"([a-z_]+)"/g, union[1]));
})();
const client = {
  ops: sorted(Object.values(types.DJ_ENGINE_OPS)),
  events: sorted(Object.values(types.DJ_ENGINE_EVENTS)),
  capabilities: sorted(Object.values(types.DJ_ENGINE_CAPABILITIES)),
  errors: clientErrors,
};

// ------------------------------------------------------------------ Mixxx host
const host = read("native/mixxx-engine-host/src/host.cpp");
const backend = read("native/mixxx-engine-host/src/mixxx_backend.cpp");
const runtime = functionBody(read("native/mixxx-engine-host/src/junction/runtime.cpp"), "QJsonObject Runtime::command(");
const hostCapabilities = (() => {
  const body = functionBody(host, "QJsonObject Host::info() const");
  return sorted([...all(/capabilities\s*=\s*\{([^}]*)\}/g, body).flatMap((list) => all(/"([^"]+)"/g, list)),
    ...all(/capabilities\.append\("([^"]+)"\)/g, body)]);
})();
// The op sets host.cpp accepts before dispatching (`known`, `samplerOps`, ...).
const hostOpSets = ["samplerOps", "known", "transport", "deckControls", "mixerOps"].flatMap((name) => {
  const match = new RegExp(`const QSet<QString> ${name} = \\{([^}]*)\\}`).exec(host);
  if (!match) throw new Error(`host.cpp op set ${name} not found`);
  return all(/"([^"]+)"/g, match[1]);
});
const mixxx = {
  ops: sorted([
    ...hostOpSets,
    ...all(/\bop\s*==\s*"([^"]+)"/g, host),
    ...all(/\bop\s*!=\s*"([^"]+)"/g, host),
    ...all(/\bop\s*==\s*"(waveform\.[^"]+)"/g, backend),
    ...all(/\bop\s*==\s*"([^"]+)"/g, runtime).map((op) => `junction.${op}`),
  ]),
  prefixes: sorted(all(/op\.startsWith\("([^"]+)"\)/g, host)),
  events: sorted(all(/\bevent\("([^"]+)"/g, host)),
  capabilities: hostCapabilities,
  errors: sorted(all(/error\(\s*cmd\s*,\s*"([a-z_]+)"/g, host)),
};

// ------------------------------------------------------------------ simulator
const simProtocol = read("native/dj-engine-host/src/protocol.rs");
const simEngine = read("native/dj-engine-host/src/engine.rs");
const simModule = (name) => {
  const match = new RegExp(`pub mod ${name} \\{([\\s\\S]*?)\\n\\}`).exec(simProtocol);
  if (!match) throw new Error(`simulator module ${name} not found`);
  return sorted(all(/pub const [A-Z_]+: &str = "([^"]+)"/g, match[1]));
};
const simulator = {
  ops: simModule("op"),
  events: simModule("event"),
  capabilities: sorted(all(/"([a-z]+(?:\.[a-zA-Z0-9]+)+)"\.to_string\(\)/g, simEngine)),
};

// ------------------------------------------------------------------ contract
const capabilityNames = sorted([...client.capabilities, ...mixxx.capabilities, ...simulator.capabilities]);

/** The capability gating an op: the longest declared name that equals it or prefixes it at a segment. */
function gate(op) {
  return capabilityNames.filter((cap) => op === cap || op.startsWith(`${cap}.`)).sort((a, b) => b.length - a.length)[0] ?? null;
}

const implementers = (name, kind) => ({
  client: client[kind].includes(name),
  mixxx: mixxx[kind].includes(name),
  simulator: simulator[kind]?.includes(name) ?? false,
});

const ops = Object.fromEntries(sorted([...client.ops, ...mixxx.ops, ...simulator.ops]).map((op) => [op, {
  category: op.split(".")[0],
  capability: gate(op),
  ...implementers(op, "ops"),
}]));
const events = Object.fromEntries(sorted([...client.events, ...mixxx.events, ...simulator.events]).map((name) => [name, implementers(name, "events")]));
const capabilities = Object.fromEntries(capabilityNames.map((name) => [name, implementers(name, "capabilities")]));
const errors = Object.fromEntries(sorted([...client.errors, ...mixxx.errors]).map((code) => [code, {
  client: client.errors.includes(code),
  mixxx: mixxx.errors.includes(code),
}]));

const contract = {
  comment: "Generated by tools/engine-contract/contract.mjs --write. Do not edit by hand.",
  protocol: "plumdeck dj-engine NDJSON",
  version: 1,
  transport: {
    framing: "one JSON object per line on stdin/stdout",
    maxLineBytes: 1048576,
    command: { id: "string", op: "string", sessionId: "string (after session.hello)", params: "object" },
    result: { id: "string", ok: true, result: "object" },
    error: { id: "string", ok: false, error: { code: "string", message: "string" } },
    event: { event: "string", data: "object", rev: "number" },
  },
  prefixDispatch: mixxx.prefixes,
  constants: {
    decks: types.DECK_IDS,
    eqBands: types.EQ_BANDS,
    crossfaderCurves: types.CROSSFADER_CURVES,
    padEffects: types.PAD_EFFECTS,
    ranges: types.DJ_ENGINE_RANGES,
  },
  ops,
  events,
  capabilities,
  errors,
};

const text = `${JSON.stringify(contract, null, 2)}\n`;
if (process.argv.includes("--write")) {
  mkdirSync(dirname(join(root, OUT)), { recursive: true });
  writeFileSync(join(root, OUT), text);
  console.log(`wrote ${OUT}: ${Object.keys(ops).length} ops, ${Object.keys(events).length} events, ${capabilityNames.length} capabilities, ${Object.keys(errors).length} error codes`);
} else {
  let committed = "";
  try { committed = read(OUT); } catch { /* missing */ }
  if (committed !== text) {
    const before = committed ? JSON.parse(committed) : { ops: {}, events: {}, capabilities: {}, errors: {} };
    for (const kind of ["ops", "events", "capabilities", "errors"]) {
      for (const name of sorted([...Object.keys(before[kind] ?? {}), ...Object.keys(contract[kind])])) {
        const a = JSON.stringify(before[kind]?.[name]), b = JSON.stringify(contract[kind][name]);
        if (a !== b) console.error(`${kind} ${name}: contract ${a ?? "(none)"} vs sources ${b ?? "(none)"}`);
      }
    }
    console.error(`${OUT} is out of date with the sources: review the change and run with --write`);
    process.exit(1);
  }
  console.log(`${OUT} matches the sources`);
}
