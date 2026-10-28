// Exercise a real backend build (the headless `plumdeck-backend` binary, or the
// installed desktop app with --desktop) and its MCP server in an isolated
// user profile.
//
//   node scripts/smoke-backend.mjs <plumdeck-backend executable> [--ingest]
//   node scripts/smoke-backend.mjs <plumdeck desktop executable> --desktop
//
// Checks: GET / answers the API banner, MCP initialize / tools/list /
// search_tracks work on an empty library, on Windows (or with --ingest) a
// tagless WAV goes through /api/ingest in a real analysis worker, and a normal
// stop (stdin "plumdeck:shutdown" on Windows, SIGINT elsewhere, WM_CLOSE for
// the desktop app) exits 0 and releases the port.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { createConnection, createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { basename, extname, join, resolve } from 'node:path';

const win = process.platform === 'win32';
const delay = (ms) => new Promise((done) => setTimeout(done, ms));
const system32 = () => join(process.env.SystemRoot, 'System32');

function reservePort(port) {
    return new Promise((done, fail) => {
        const server = createServer();
        server.once('error', fail);
        server.listen(port, '127.0.0.1', () => {
            const address = server.address();
            server.close(() => done(address.port));
        });
    });
}

function portAcceptsConnections(port) {
    return new Promise((done) => {
        const socket = createConnection({ host: '127.0.0.1', port });
        socket.setTimeout(1000);
        socket.once('connect', () => { socket.destroy(); done(true); });
        socket.once('timeout', () => { socket.destroy(); done(false); });
        socket.once('error', () => done(false));
    });
}

/** Rhythmic 11.025 kHz mono 16-bit audio with no title/artist tags. */
function writeTaglessFixture(path) {
    const rate = 11025;
    const frames = rate * 12;
    const wave = Buffer.alloc(44 + frames * 2);
    wave.write('RIFF', 0); wave.writeUInt32LE(wave.length - 8, 4); wave.write('WAVEfmt ', 8);
    wave.writeUInt32LE(16, 16); wave.writeUInt16LE(1, 20); wave.writeUInt16LE(1, 22);
    wave.writeUInt32LE(rate, 24); wave.writeUInt32LE(rate * 2, 28); wave.writeUInt16LE(2, 32);
    wave.writeUInt16LE(16, 34); wave.write('data', 36); wave.writeUInt32LE(frames * 2, 40);
    for (let index = 0; index < frames; index++) {
        const seconds = index / rate;
        let value = 0.14 * Math.sin(2 * Math.PI * 261.626 * seconds)
            + 0.11 * Math.sin(2 * Math.PI * 329.628 * seconds)
            + 0.09 * Math.sin(2 * Math.PI * 391.995 * seconds);
        const sinceBeat = (((seconds - 0.25) % 0.5) + 0.5) % 0.5;
        if (sinceBeat < 0.02) value += 0.55 * Math.exp(-sinceBeat * 180);
        wave.writeInt16LE(Math.round(Math.max(-1, Math.min(1, value)) * 32767), 44 + index * 2);
    }
    writeFileSync(path, wave);
}

async function jsonApi(port, path, payload, timeout = 10_000) {
    const init = payload === undefined ? {} : {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(payload),
    };
    const response = await fetch(`http://127.0.0.1:${port}${path}`, { ...init, signal: AbortSignal.timeout(timeout) });
    assert.ok(response.ok, `${path} answered ${response.status}`);
    return response.json();
}

/** Library import -> background task -> `--analysis-worker` subprocess. */
async function exerciseIngestion(port, directory) {
    const fixture = join(directory, 'Tagless Analysis Fixture.wav');
    writeTaglessFixture(fixture);
    const started = await jsonApi(port, '/api/ingest', {
        targets: [fixture], force_update: true, analysis_profile: 'light',
    }, 15_000);
    assert.equal(started.status, 'success', JSON.stringify(started));
    const observed = new Set();
    let state;
    const deadline = Date.now() + 90_000;
    for (;;) {
        if (Date.now() > deadline) throw new Error('/api/ingest did not finish within 90s');
        state = await jsonApi(port, '/api/ingest/status', undefined, 5000);
        for (const label of Object.values(state.details?.active_progress || {})) {
            if (typeof label === 'string') observed.add(label);
        }
        if (['complete', 'error', 'cancelled'].includes(state.type)) break;
        await delay(100);
    }
    assert.equal(state.type, 'complete', JSON.stringify(state));
    assert.ok(state.processed === 1 && state.errors === 0, JSON.stringify(state));
    observed.delete('解析ワーカーを起動しています');
    assert.ok(observed.size, `Analysis worker reported no concrete audio stage: ${JSON.stringify([...observed])}`);
    const page = await jsonApi(port, '/api/tracks/page?limit=2');
    assert.ok(page.total === 1 && page.items?.length === 1, JSON.stringify(page));
    const saved = page.items[0];
    const stem = basename(fixture, extname(fixture));
    assert.ok(saved.title === stem && saved.artist === 'Unknown', JSON.stringify(saved));
    console.log(`Light ingestion registered tagless audio; progress=${JSON.stringify([...observed].sort())}`);
}

async function exerciseMcp(port) {
    const headers = { 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream' };
    const rpc = async (payload) => {
        const response = await fetch(`http://127.0.0.1:${port}/mcp`, {
            method: 'POST', headers, body: JSON.stringify(payload), signal: AbortSignal.timeout(15_000),
        });
        assert.ok(response.ok, `/mcp answered ${response.status}`);
        const session = response.headers.get('mcp-session-id');
        if (session) headers['Mcp-Session-Id'] = session;
        const body = await response.text();
        assert.ok(Buffer.byteLength(body) <= 2 * 1024 * 1024, 'MCP response exceeds 2 MiB');
        if (!body.trim()) return null;
        const events = body.split(/\r?\n/).filter((line) => line.startsWith('data:'))
            .map((line) => JSON.parse(line.slice(5).trim()));
        const messages = events.length ? events : [JSON.parse(body)];
        const result = messages.find((item) => item.id === payload.id);
        assert.ok(result, `No MCP reply for id ${payload.id}`);
        assert.ok(!('error' in result), JSON.stringify(result));
        return result.result;
    };
    const initialized = await rpc({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {
        protocolVersion: '2025-03-26', capabilities: {}, clientInfo: { name: 'distribution-smoke', version: '1' } } });
    assert.ok(initialized?.serverInfo, JSON.stringify(initialized));
    headers['MCP-Protocol-Version'] = initialized.protocolVersion;
    await rpc({ jsonrpc: '2.0', method: 'notifications/initialized' });
    const listed = await rpc({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} });
    assert.ok(listed.tools.some((tool) => tool.name === 'search_tracks'), 'search_tracks tool missing');
    const searched = await rpc({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: {
        name: 'search_tracks', arguments: { limit: 1 } } });
    assert.ok(!searched.isError, JSON.stringify(searched));
    let data = searched.structuredContent
        || JSON.parse(searched.content.find((item) => item.type === 'text').text);
    data = data.result ?? data; // MCP wraps generic dictionary return annotations.
    assert.ok(data.count === 0 && Array.isArray(data.tracks) && data.tracks.length === 0, JSON.stringify(data));
    console.log('API, MCP initialization, tool discovery and library search passed');
}

// Win32 helpers for the desktop app, through Windows PowerShell's Add-Type.
const WINDOW_HELPER = `
Add-Type -TypeDefinition @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices; using System.Text;
public static class PlumdeckWindows {
  delegate bool EnumProc(IntPtr hwnd, IntPtr context);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr context);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr hwnd, StringBuilder text, int size);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowTextW(IntPtr hwnd, StringBuilder text, int size);
  [DllImport("user32.dll")] static extern bool PostMessageW(IntPtr hwnd, uint message, IntPtr w, IntPtr l);
  public static List<object[]> List(uint owner, bool close) {
    var found = new List<object[]>();
    EnumWindows((hwnd, context) => {
      uint pid; GetWindowThreadProcessId(hwnd, out pid);
      if (pid != owner) return true;
      var cls = new StringBuilder(256); GetClassNameW(hwnd, cls, 256);
      var title = new StringBuilder(256); GetWindowTextW(hwnd, title, 256);
      // WM_CLOSE only the real application window: closing tao's event target
      // or the single-instance helper tears down the message pump, so
      // RunEvent::Exit never fires and the backend is never asked to stop.
      if (close && cls.ToString() == "Tauri Window") PostMessageW(hwnd, 0x10, IntPtr.Zero, IntPtr.Zero);
      found.Add(new object[] { hwnd.ToInt64(), IsWindowVisible(hwnd), cls.ToString(), title.ToString() });
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
'@
`;

function desktopWindows(pid, close = false) {
    const script = `${WINDOW_HELPER}\nConvertTo-Json -Compress -Depth 3 -InputObject ([PlumdeckWindows]::List(${pid}, $${close}))`;
    const result = spawnSync(join(system32(), 'WindowsPowerShell/v1.0/powershell.exe'),
        ['-NoProfile', '-NonInteractive', '-Command', script], { encoding: 'utf8', timeout: 60_000 });
    if (result.status !== 0) throw new Error(`window enumeration failed: ${result.stderr}`);
    const parsed = JSON.parse(result.stdout.trim() || '[]');
    return (Array.isArray(parsed) ? parsed : [parsed]).filter(Array.isArray);
}

function closeDesktop(pid) {
    const windows = desktopWindows(pid, true);
    const targets = windows.filter((window) => window[2] === 'Tauri Window');
    assert.ok(targets.length, `Tauri window not found; saw ${JSON.stringify(windows)}`);
    return targets;
}

function waitForExit(child, timeout) {
    if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve(true);
    return new Promise((done) => {
        const timer = setTimeout(() => done(false), timeout);
        child.once('exit', () => { clearTimeout(timer); done(true); });
    });
}

async function main() {
    const [target, ...flags] = process.argv.slice(2);
    if (!target) throw new Error('Usage: node scripts/smoke-backend.mjs <executable> [--desktop] [--ingest]');
    const executable = resolve(target);
    const desktop = flags.includes('--desktop');
    const ingest = win || flags.includes('--ingest');
    const directory = mkdtempSync(join(tmpdir(), 'Plumdeck 配布 '));
    // Release desktop builds always serve 48123; the headless binary takes any port.
    const port = await reservePort(desktop ? 48123 : 0);
    const env = {
        ...process.env,
        USER_DATA_DIR: directory,
        DB_PATH: join(directory, 'library.duckdb'),
        PLUMDECK_LOG_DIR: join(directory, 'logs'),
        PLUMDECK_PORT: String(port),
        ENV: 'prod',
        PLUMDECK_MANAGED_SIDECAR: '1',
    };
    // Prove the build needs nothing from a developer PATH.
    if (win) env.PATH = [system32(), process.env.SystemRoot].join(';');
    let log = Buffer.alloc(0);
    const keep = (chunk) => { log = Buffer.concat([log, chunk]).subarray(-20_000); };
    const child = spawn(executable, [], { env, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    child.stdout.on('data', keep);
    child.stderr.on('data', keep);
    let spawnError;
    child.once('error', (error) => { spawnError = error; });
    try {
        const deadline = Date.now() + 120_000;
        for (;;) {
            if (spawnError) throw spawnError;
            if (child.exitCode !== null || child.signalCode !== null) {
                throw new Error(`Backend exited: ${child.signalCode || child.exitCode}`);
            }
            if (Date.now() > deadline) throw new Error('Backend did not become ready');
            try {
                const response = await fetch(`http://127.0.0.1:${port}/`, { signal: AbortSignal.timeout(2000) });
                const text = await response.text();
                assert.ok(text.includes('plumdeck Backend API is running'), `Unexpected GET / answer: ${text.slice(0, 200)}`);
                break;
            } catch (error) {
                if (error instanceof assert.AssertionError) throw error;
                await delay(500);
            }
        }
        await exerciseMcp(port);
        if (ingest) await exerciseIngestion(port, directory);
        if (desktop) {
            const windows = closeDesktop(child.pid);
            console.log(`Posted WM_CLOSE to ${windows.length} window(s): ${JSON.stringify(windows)}`);
            const started = Date.now();
            let exited = false;
            while (!exited && Date.now() - started < 150_000) {
                exited = await waitForExit(child, 5000);
                if (!exited) {
                    const elapsed = Math.round((Date.now() - started) / 1000);
                    console.log(`  +${elapsed}s: still running, windows=${JSON.stringify(desktopWindows(child.pid))}`);
                }
            }
            if (!exited) {
                spawnSync(join(system32(), 'tasklist.exe'), ['/v'], { stdio: 'inherit' });
                throw new Error('Desktop app did not exit within 150s of WM_CLOSE');
            }
        } else {
            if (win) {
                child.stdin.write('plumdeck:shutdown\n');
                child.stdin.end();
            } else {
                child.kill('SIGINT');
            }
            if (!await waitForExit(child, 60_000)) throw new Error('Normal shutdown timed out');
        }
        assert.equal(child.exitCode, 0, `Exited with ${child.signalCode || child.exitCode}`);
        assert.equal(await portAcceptsConnections(port), false, 'Backend listener survived shutdown');
        console.log('Normal shutdown released the API/MCP port');
    } catch (error) {
        process.stderr.write(log.toString('utf8'));
        throw error;
    } finally {
        if (child.exitCode === null && child.signalCode === null) {
            if (win) spawnSync(join(system32(), 'taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
            else child.kill('SIGTERM');
            if (!await waitForExit(child, 10_000)) child.kill('SIGKILL');
        }
        child.stdin.destroy();
        rmSync(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
    }
}

main().catch((error) => {
    console.error(error.stack || error.message);
    process.exitCode = 1;
});
