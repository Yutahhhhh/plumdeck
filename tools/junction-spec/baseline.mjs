// Junction specification baseline and porting ledger (engine-and-junction.md W2-30).
//
//   node tools/junction-spec/baseline.mjs          # check
//   node tools/junction-spec/baseline.mjs --write  # regenerate
//
// Reads what the C++ Junction (native/mixxx-engine-host/src/junction) and the
// code around it define today and writes native/junction-spec/:
// - TESTMAP.json   every test that exercises Junction, wherever it lives (the
//                  C++ JTEST cases, the native Node runtime tests, the
//                  frontend, the signalling service, the MCP bridge and the
//                  Python MCP tools), each with a content hash and its porting
//                  target. The `port` fields are the ledger: --write keeps
//                  what is committed and only fills new entries.
// - OPMAP.json     the `junction.*` commands (who implements and who calls
//                  each, the parameters read and the rejection messages per
//                  command, heuristically from the dispatch text), the control
//                  channel and signalling message types, the engine ops the
//                  session authority treats specially, and the MCP tools; plus
//                  the mismatches between those sides.
// - CONSTANTS.json the named constants of the Junction sources.
// - BASELINE.json  hashes of every Junction source file, test and op.
// - PORTING-LEDGER.md the ledger rendered from TESTMAP.json.
// Check mode rebuilds everything and fails when anything differs, listing
// tests and ops that were added, removed or changed, source files that
// changed, ledger entries that point at missing Rust tests, and commands or
// message types that SPEC.md does not mention.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const read = (path) => readFileSync(join(root, path), "utf8");
const OUT = "native/junction-spec";
const JUNCTION_SRC = "native/mixxx-engine-host/src/junction/";

const sorted = (values) => [...new Set(values)].sort();
const all = (pattern, text, group = 1) => [...text.matchAll(pattern)].map((match) => match[group]);
const hash = (text) => createHash("sha256").update(text.replace(/[ \t]+$/gm, "").replace(/\r\n/g, "\n")).digest("hex").slice(0, 16);
const files = execFileSync("git", ["ls-files", "-z"], { cwd: root }).toString().split("\0").filter(Boolean).sort();

function body(text, signature, open = "{", close = "}") {
  const start = text.indexOf(signature);
  if (start < 0) throw new Error(`missing ${signature}`);
  let depth = 0;
  for (let at = text.indexOf(open, start); at < text.length; at++) {
    if (text[at] === open) depth++;
    if (text[at] === close && --depth === 0) return text.slice(start, at + 1);
  }
  throw new Error(`unterminated ${signature}`);
}

// ------------------------------------------------------------------ tests
const junctionPath = (path) => /junction/i.test(path);
const isTestFile = (path) =>
  /_test\.cpp$/.test(path) || /\.test\.(mjs|js|ts|tsx)$/.test(path) || /(^|\/)test_[^/]*\.py$/.test(path) || path.endsWith(".rs");

/** Test cases of one file: {suite, name, start}. */
function cases(path, text) {
  if (path.endsWith(".cpp")) {
    return [...text.matchAll(/^JTEST\(\s*"((?:\\.|[^"])*)"\s*,\s*"((?:\\.|[^"])*)"\s*\)/gm)].map((m) => ({ suite: m[1], name: m[2], start: m.index }));
  }
  if (path.endsWith(".py")) {
    const found = [];
    let klass = null;
    for (const m of text.matchAll(/^(class (\w+)|(\s*)(?:async )?def (test_\w+))/gm)) {
      if (m[2]) klass = m[2];
      else found.push({ suite: m[3] && klass ? klass : null, name: m[4], start: m.index });
    }
    return found;
  }
  if (path.endsWith(".rs")) {
    return [...text.matchAll(/#\[(?:tokio::)?test\][^\n]*\n(?:\s*#\[[^\n]*\n)*\s*(?:async )?fn (\w+)/g)].map((m) => ({ suite: null, name: m[1], start: m.index }));
  }
  const found = [];
  const describes = [];
  for (const m of text.matchAll(/^([ \t]*)(describe|test|it)(?:\.(?:skip|only|todo))?\(\s*(['"`])((?:\\.|(?!\3)[^\\])*)\3/gm)) {
    const indent = m[1].length;
    while (describes.length && describes.at(-1).indent >= indent) describes.pop();
    if (m[2] === "describe") describes.push({ indent, name: m[4] });
    else found.push({ suite: describes.map((d) => d.name).join(" > ") || null, name: m[4], start: m.index });
  }
  return found;
}

/**
 * End of the case starting at `start`: the brace that closes a C++ or Rust
 * body, or the parenthesis that closes a JS test call. Strings, template
 * literals and comments are skipped so their brackets do not count.
 */
function caseEnd(text, start, path) {
  const [open, close] = path.endsWith(".cpp") || path.endsWith(".rs") ? ["{", "}"] : ["(", ")"];
  let depth = 0;
  for (let at = text.indexOf(open, start); at >= 0 && at < text.length; at++) {
    const ch = text[at];
    if (ch === "/" && text[at + 1] === "/") { at = text.indexOf("\n", at); if (at < 0) break; continue; }
    if (ch === "/" && text[at + 1] === "*") { at = text.indexOf("*/", at + 2) + 1; if (at <= 0) break; continue; }
    if (ch === '"' || ch === "`" || (ch === "'" && !path.endsWith(".rs"))) {
      for (at++; at < text.length && text[at] !== ch; at++) if (text[at] === "\\") at++;
      continue;
    }
    if (ch === open) depth++;
    if (ch === close && --depth === 0) return at + 1;
  }
  return text.length;
}

/** Porting target per test: [path pattern, suite pattern | null, port]. First match wins. */
const TARGETS = [
  [/tests\/junction\/.*_test\.cpp$/, /^(authority|manual-exchange|network-settings|program_mixer|turn|ice-servers)$/, { layer: "core", item: "W2-33", action: "port" }],
  [/tests\/junction\/.*_test\.cpp$/, /^(media-timeline|clock-estimator|audio-clock-anchor|asrc|pcm-ring|producer-tap|junction[ -]input)$/, { layer: "core", item: "W2-34", action: "port" }],
  [/tests\/junction\/.*_test\.cpp$/, /^(media-transport|media-relay)$/, { layer: "transport", item: "W2-38", action: "port" }],
  [/tests\/junction\/.*_test\.cpp$/, /^program-output/, { layer: "runtime", item: "W2-44", action: "port" }],
  [/tests\/junction\/cross-platform-runtime\.test\.mjs$/, null, { layer: "interop", item: "W2-45", action: "port" }],
  [/tests\/junction\/(manual-)?runtime\.test\.mjs$/, null, { layer: "runtime", item: "W2-42", action: "port" }],
  // Junction v2 (owner decision 2026-10-29): not a port of the C++ runtime.
  [/tests\/conformance\/jlink\.test\.mjs$/, null, { layer: "junction-v2", item: null, action: "keep" }],
  [/^services\/junction-signaling\//, null, { layer: "signaling", item: null, action: "keep" }],
  [/^src\//, null, { layer: "frontend", item: "W2-47", action: "rerun" }],
  [/^backend\//, null, { layer: "mcp", item: "S12", action: "port" }],
  [/^src-tauri\//, null, { layer: "bridge", item: "S12", action: "port" }],
];

function target(path, suite) {
  for (const [file, suites, port] of TARGETS) {
    if (file.test(path) && (!suites || suites.test(suite ?? ""))) return port;
  }
  throw new Error(`no porting target for ${path} (${suite})`);
}

const tests = {};
const testFiles = {};
for (const path of files.filter(isTestFile)) {
  if (path.startsWith(OUT)) continue;
  const text = read(path);
  const whole = junctionPath(path);
  if (!whole && !/junction/i.test(text)) continue;
  const found = cases(path, text);
  const taken = [];
  found.forEach((test, index) => {
    const limit = found[index + 1]?.start ?? text.length;
    const segment = text.slice(test.start, path.endsWith(".py") ? limit : Math.min(limit, caseEnd(text, test.start, path)));
    // Outside the Junction files only the cases that name Junction belong here.
    if (!whole && !/junction/i.test(segment)) return;
    let id = `${path}::${test.suite ? `${test.suite} > ` : ""}${test.name}`;
    for (let n = 2; tests[id]; n++) id = `${path}::${test.suite ? `${test.suite} > ` : ""}${test.name} #${n}`;
    tests[id] = { file: path, suite: test.suite, name: test.name, hash: hash(segment), port: target(path, test.suite) };
    taken.push(id);
  });
  if (taken.length) testFiles[path] = hash(text);
}

// Keep the committed ledger fields (status, rust, note) for known tests.
const committedTestmap = existsSync(join(root, OUT, "TESTMAP.json")) ? JSON.parse(read(`${OUT}/TESTMAP.json`)) : { tests: {} };
for (const [id, test] of Object.entries(tests)) {
  const before = committedTestmap.tests?.[id]?.port;
  const defaults = { status: test.port.action === "keep" ? "kept" : "todo", rust: null, note: null };
  test.port = { ...test.port, ...defaults, ...(before ? { status: before.status, rust: before.rust ?? null, note: before.note ?? null } : {}) };
}

// ------------------------------------------------------------------ ops
const runtimeText = read(`${JUNCTION_SRC}runtime.cpp`);
const dispatch = body(runtimeText, "QJsonObject Runtime::command(");
const opSites = [...dispatch.matchAll(/\bop\s*==\s*"([^"]+)"/g)];
const prefixes = sorted(all(/\bop\.startsWith\("([^"]+)"\)/g, dispatch));

/**
 * Text that handles `op`. A comparison that opens a block (`if(op=="x"){`)
 * covers the block; any other comparison covers the text up to the next
 * comparison of a different op.
 */
function opText(op) {
  let text = "";
  opSites.forEach((site, index) => {
    if (site[1] !== op) return;
    let end = dispatch.length;
    for (let next = index + 1; next < opSites.length; next++) {
      if (opSites[next][1] !== op && opSites[next].index > site.index + site[0].length + 40) { end = opSites[next].index; break; }
    }
    const lineStart = dispatch.lastIndexOf("\n", site.index) + 1;
    const indent = /^[ \t]*/.exec(dispatch.slice(lineStart))[0].length;
    const lineEnd = dispatch.indexOf("\n", site.index);
    if (/^[ \t]*if\(/.test(dispatch.slice(lineStart)) && /\{\s*$/.test(dispatch.slice(lineStart, lineEnd))) {
      const rest = dispatch.slice(lineEnd + 1).split("\n");
      let at = lineEnd + 1;
      for (const line of rest) {
        if (/^[ \t]*\}/.test(line) && /^[ \t]*/.exec(line)[0].length <= indent) { at += line.length; break; }
        at += line.length + 1;
      }
      end = Math.min(end, at);
    }
    text += dispatch.slice(site.index, end);
  });
  return text;
}
const params = (text) => sorted([...all(/\bp\["(\w+)"\]/g, text), ...all(/\bp\.(?:contains|value)\("(\w+)"/g, text)]);
const rejects = (text) => sorted([...all(/reject\(\s*(?:QStringLiteral\()?"((?:\\.|[^"])*)"/g, text), ...all(/return\s+"((?:\\.|[^"])*)"/g, text)]);

const preview = read(`${JUNCTION_SRC}private_preview.cpp`);
const previewDispatch = body(preview, "QString PrivatePreview::command(");
const previewSites = [...previewDispatch.matchAll(/op\.endsWith\("\.(\w+)"\)/g)];
const previewOps = sorted(previewSites.map((m) => `private.${m[1]}`));
/** The private preview branch for `op`, up to the next suffix test that is not on the same line. */
function previewText(op) {
  const site = previewSites.find((m) => `private.${m[1]}` === op);
  const lineEnd = previewDispatch.indexOf("\n", site.index);
  const next = previewSites.find((m) => m.index > lineEnd);
  return previewDispatch.slice(site.index, next ? next.index : lineEnd);
}

const clientOps = (() => {
  const union = /export type JunctionOp\s*=([\s\S]*?);/.exec(read("src/types/junction.ts"));
  if (!union) throw new Error("JunctionOp union not found");
  return sorted(all(/'([^']+)'/g, union[1]));
})();

const bridgeText = read("src-tauri/src/junction_mcp_bridge.rs");
const bridgeActions = Object.fromEntries([...bridgeText.matchAll(/"([a-z.]+)"\s*=>\s*(?:\{[^}]*?)?\(?\s*"(junction\.[a-z.]+|audio\.[a-z.]+)"/g)].map((m) => [m[1], m[2]]));
for (const m of bridgeText.matchAll(/^\s*"([a-z.]+)" => (?:match|export_exchange|\{)/gm)) bridgeActions[m[1]] ??= null;

// The MCP Junction tools (routes/junction.rs `request`): each match arm names
// its tools and the bridge action it posts; retired tools answer an error.
const mcpText = body(read("src-tauri/crates/plumdeck-backend/src/routes/junction.rs"), "fn request(");
const mcpArms = [...mcpText.matchAll(/^ {8}((?:"junction_\w+"\s*\|?\s*)+)=>/gm)];
const mcpTools = Object.fromEntries(mcpArms.flatMap((m, index) => {
  const text = mcpText.slice(m.index + m[0].length, mcpArms[index + 1]?.index ?? mcpText.length);
  const entry = { actions: sorted(all(/\("([a-z.]+)",\s*(?:json!|o\()/g, text)), retired: /RETIRED_/.test(text) };
  return all(/"(junction_\w+)"/g, m[1]).map((name) => [name, entry]);
}));

const commandNames = sorted([...opSites.map((m) => m[1]), ...previewOps]);
const commands = Object.fromEntries(sorted([...commandNames, ...clientOps, ...Object.values(bridgeActions).filter((op) => op?.startsWith("junction.")).map((op) => op.slice(9))]).map((op) => {
  const text = op.startsWith("private.") ? (previewOps.includes(op) ? previewText(op) : "") : opText(op);
  return [op, {
    runtime: op.startsWith("private.") ? (previewOps.includes(op) ? "private-preview" : null) : (opSites.some((m) => m[1] === op) ? "runtime" : null),
    client: clientOps.includes(op),
    bridge: sorted(Object.entries(bridgeActions).filter(([, mapped]) => mapped === `junction.${op}`).map(([action]) => action)),
    params: params(text),
    rejects: rejects(text),
  }];
}));

const protocolText = read(`${JUNCTION_SRC}session_protocol.cpp`);
const controlTypes = sorted(all(/\{MessageType::\w+,\s*"([^"]+)"\}/g, protocolText));
const signalingText = read("services/junction-signaling/src/protocol.ts");
const signalingTypes = sorted(all(/type:\s*"([a-z_.]+)"/g, signalingText));
const authorityText = read(`${JUNCTION_SRC}authority.cpp`);
const opSet = (fn) => sorted(all(/"([^"]+)"/g, /allowed=\{([^}]*)\}/.exec(body(authorityText, `bool Authority::${fn}(`))[1]));
const tailLock = body(read(`${JUNCTION_SRC}turn_state.h`), "struct TailLock {");

const mismatches = [];
for (const [op, entry] of Object.entries(commands)) {
  if (entry.client && !entry.runtime) mismatches.push(`command ${op}: in JunctionOp but not handled by the runtime`);
  if (entry.runtime && !entry.client && !["audio.config.set", "input.probe"].includes(op)) mismatches.push(`command ${op}: handled by the runtime but missing from JunctionOp`);
  if (!entry.runtime && entry.bridge.length) mismatches.push(`command ${op}: the MCP bridge maps ${entry.bridge.join(", ")} to it but the runtime does not handle it`);
}
const handledActions = new Set(Object.keys(bridgeActions));
for (const [tool, { actions, retired }] of Object.entries(mcpTools)) {
  if (!actions.length && !retired) mismatches.push(`MCP tool ${tool}: calls no bridge action`);
  for (const action of actions) if (!handledActions.has(action)) mismatches.push(`MCP tool ${tool}: action ${action} is not handled by the bridge`);
}
const knownTestSuites = Object.values(tests).filter((t) => t.file.endsWith(".cpp")).map((t) => t.suite);
for (const [a, b] of [["junction input", "junction-input"]]) {
  if (knownTestSuites.includes(a) && knownTestSuites.includes(b)) mismatches.push(`C++ suites "${a}" and "${b}" name the same module; a same-name port keeps both`);
}

const opmap = {
  comment: "Generated by tools/junction-spec/baseline.mjs --write. Do not edit by hand.",
  dispatch: {
    engine: "host.cpp hands `junction.<command>` to junction::Runtime::command (the prefix is stripped); other ops pass through Runtime::authorize while a session is active",
    prefixes,
    note: "params and rejects are read from the text after each `op==` comparison up to the next command, so shared branches list the union",
  },
  commands,
  control_messages: controlTypes,
  signaling_messages: signalingTypes,
  authority: {
    local_only: opSet("localOnly"),
    read_only: opSet("readOnlyQuery"),
    tail_lock_master: sorted(all(/op=="([^"]+)"/g, /\/\/ Program-wide[\s\S]*?return/.exec(tailLock)[0])),
    tail_lock_allowed: sorted([...all(/op\.startsWith\("([^"]+)"\)\|\|op=="mixer\.channel\.pfl"/g, tailLock).map((p) => `${p}*`), "deck.timing.trace", "mixer.channel.pfl"]),
    tail_lock_rejects: sorted(all(/QStringLiteral\("((?:\\.|[^"])*)"\)/g, tailLock)),
  },
  mcp: { tools: mcpTools, bridge_actions: Object.fromEntries(Object.entries(bridgeActions).sort()) },
  mismatches: sorted(mismatches),
};

// ------------------------------------------------------------------ constants
const sourceFiles = files.filter((path) => path.startsWith(JUNCTION_SRC));
const constants = {};
for (const path of sourceFiles) {
  const text = read(path);
  for (const m of text.matchAll(/^[ \t]*(?:inline\s+|static\s+)*constexpr\s+([\w:<>,\s*]+?)\s+(\w+)\s*(?:\[\])?\s*=\s*([^;]+);[ \t]*(?:\/\/\s*(.*))?$/gm)) {
    let key = `${path.slice(JUNCTION_SRC.length)}::${m[2]}`;
    for (let n = 2; constants[key]; n++) key = `${path.slice(JUNCTION_SRC.length)}::${m[2]} #${n}`;
    constants[key] = { type: m[1].replace(/\s+/g, " ").trim(), value: m[3].replace(/\s+/g, " ").trim(), ...(m[4] ? { comment: m[4].trim() } : {}) };
  }
  for (const m of text.matchAll(/^\s*(?:static\s+)?constexpr\s+unsigned\s+(\w+)\s*=\s*(\d+)\s*,\s*(\w+)\s*=\s*(\d+)\s*;/gm)) {
    constants[`${path.slice(JUNCTION_SRC.length)}::${m[1]}`] = { type: "unsigned", value: m[2] };
    constants[`${path.slice(JUNCTION_SRC.length)}::${m[3]}`] = { type: "unsigned", value: m[4] };
  }
}
for (const path of ["src/services/junction/client.ts", "src/types/junction.ts"]) {
  for (const m of read(path).matchAll(/^export const ([A-Z][A-Z0-9_]+)\s*=\s*([^;]+);/gm)) constants[`${path}::${m[1]}`] = { type: "ts", value: m[2].trim() };
}

// ------------------------------------------------------------------ spec coverage
const spec = existsSync(join(root, OUT, "SPEC.md")) ? read(`${OUT}/SPEC.md`) : "";
const specMissing = [
  ...commandNames.filter((op) => !spec.includes(`\`${op}\``)).map((op) => `command ${op}`),
  ...controlTypes.filter((type) => !spec.includes(`\`${type}\``)).map((type) => `control message ${type}`),
];

// ------------------------------------------------------------------ baseline
const baseline = {
  comment: "Generated by tools/junction-spec/baseline.mjs --write. Do not edit by hand.",
  sources: Object.fromEntries(sourceFiles.map((path) => [path, hash(read(path))])),
  test_files: testFiles,
  tests: Object.fromEntries(Object.entries(tests).map(([id, test]) => [id, test.hash])),
  commands: Object.fromEntries(Object.entries(commands).map(([op, entry]) => [op, hash(JSON.stringify(entry))])),
  constants: hash(JSON.stringify(constants)),
};

const testmap = {
  comment: "Generated by tools/junction-spec/baseline.mjs --write. `port.status`, `port.rust` and `port.note` are the ledger and survive regeneration; everything else is read from the sources.",
  statuses: { todo: "not ported yet", ported: "a Rust test of the same name passes (port.rust: crate::path::fn)", rerun: "runs unchanged against the Rust side", kept: "stays as is (not part of the migration)", retired: "dropped on purpose (port.note says why)" },
  tests,
};

// ------------------------------------------------------------------ ledger
function ledger() {
  const rows = new Map();
  for (const test of Object.values(tests)) {
    const key = `${test.port.item ?? "—"}|${test.port.layer}|${test.file}${test.suite && test.file.endsWith(".cpp") ? `|${test.suite}` : ""}`;
    const row = rows.get(key) ?? { item: test.port.item ?? "—", layer: test.port.layer, file: test.file, suite: test.file.endsWith(".cpp") ? test.suite : null, action: test.port.action, total: 0, status: {} };
    row.total++;
    row.status[test.port.status] = (row.status[test.port.status] ?? 0) + 1;
    rows.set(key, row);
  }
  const ordered = [...rows.values()].sort((a, b) => a.item.localeCompare(b.item) || a.file.localeCompare(b.file) || (a.suite ?? "").localeCompare(b.suite ?? ""));
  const totals = {};
  for (const test of Object.values(tests)) totals[test.port.status] = (totals[test.port.status] ?? 0) + 1;
  const lines = [
    "# Junction 移植台帳",
    "",
    "`tools/junction-spec/baseline.mjs --write` が TESTMAP.json から生成する。手で編集しない。状態を変えるときは TESTMAP.json の `port.status`（と `port.rust`、`port.note`）を書き換えてから `--write` する。",
    "",
    `テスト ${Object.keys(tests).length} 件：${Object.entries(totals).sort().map(([s, n]) => `${s} ${n}`).join("、")}。`,
    "",
    "| 項目 | 層 | ファイル | スイート | 方針 | 件数 | 状態 |",
    "|---|---|---|---|---|---|---|",
    ...ordered.map((r) => `| ${r.item} | ${r.layer} | \`${r.file}\` | ${r.suite ? `\`${r.suite}\`` : ""} | ${r.action} | ${r.total} | ${Object.entries(r.status).sort().map(([s, n]) => `${s} ${n}`).join("、")} |`),
    "",
    "## op と実装の不一致",
    "",
    ...(opmap.mismatches.length ? opmap.mismatches.map((m) => `- ${m}`) : ["なし"]),
    "",
  ];
  return lines.join("\n");
}

// ------------------------------------------------------------------ write / check
const outputs = {
  "TESTMAP.json": `${JSON.stringify(testmap, null, 2)}\n`,
  "OPMAP.json": `${JSON.stringify(opmap, null, 2)}\n`,
  "CONSTANTS.json": `${JSON.stringify({ comment: "Generated by tools/junction-spec/baseline.mjs --write. Do not edit by hand.", constants }, null, 2)}\n`,
  "BASELINE.json": `${JSON.stringify(baseline, null, 2)}\n`,
  "PORTING-LEDGER.md": ledger(),
};

const problems = [];
if (specMissing.length) problems.push(...specMissing.map((what) => `SPEC.md does not mention ${what}`));
const rustSources = files.filter((path) => path.startsWith("src-tauri/") && path.endsWith(".rs")).map((path) => read(path)).join("\n");
for (const [id, test] of Object.entries(tests)) {
  if (test.port.status === "ported") {
    const fn = test.port.rust?.split("::").at(-1);
    if (!fn || !new RegExp(`fn ${fn}\\b`).test(rustSources)) problems.push(`ledger: ${id} is ported to ${test.port.rust ?? "(none)"}, which is not a Rust function`);
  }
}
if (process.argv.includes("--write")) {
  if (problems.length) {
    for (const problem of problems) console.error(problem);
    process.exit(1);
  }
  mkdirSync(join(root, OUT), { recursive: true });
  for (const [name, text] of Object.entries(outputs)) writeFileSync(join(root, OUT, name), text);
  console.log(`wrote ${OUT}: ${Object.keys(tests).length} tests in ${Object.keys(testFiles).length} files, ${Object.keys(commands).length} commands, ${controlTypes.length} control messages, ${Object.keys(constants).length} constants, ${opmap.mismatches.length} mismatches`);
} else {
  const before = existsSync(join(root, OUT, "BASELINE.json")) ? JSON.parse(read(`${OUT}/BASELINE.json`)) : { sources: {}, test_files: {}, tests: {}, commands: {} };
  for (const kind of ["sources", "tests", "commands"]) {
    for (const name of sorted([...Object.keys(before[kind] ?? {}), ...Object.keys(baseline[kind])])) {
      const a = before[kind]?.[name], b = baseline[kind][name];
      if (a === b) continue;
      problems.push(`${kind === "sources" ? "source" : kind.slice(0, -1)} ${a === undefined ? "added" : b === undefined ? "removed" : "changed"}: ${name}`);
    }
  }
  if (before.constants !== baseline.constants) problems.push("constants changed (see CONSTANTS.json after --write)");
  for (const [name, text] of Object.entries(outputs)) {
    let committed = "";
    try { committed = read(`${OUT}/${name}`); } catch { /* missing */ }
    if (committed !== text) problems.push(`${OUT}/${name} is out of date`);
  }
  if (problems.length) {
    for (const problem of sorted(problems)) console.error(problem);
    console.error(`${OUT} does not match the sources: review each change (update SPEC.md and the ledger) and run with --write`);
    process.exit(1);
  }
  console.log(`${OUT} matches the sources (${Object.keys(tests).length} tests, ${Object.keys(commands).length} commands)`);
}
