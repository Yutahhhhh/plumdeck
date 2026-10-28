// OS-aware commands. Paths are passed as arguments, including spaces and Unicode.
import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { createServer } from 'node:net';
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const win = process.platform === 'win32';
const manifest = join(root, 'src-tauri/Cargo.toml');
const host = join(root, 'native/mixxx-engine-host');
const stage = win ? join(host, 'stage/PlumdeckMixxxHost/plumdeck-mixxx-engine-host.exe')
    : join(host, 'stage/PlumdeckMixxxHost.app/Contents/MacOS/plumdeck-mixxx-engine-host');
const tauri = join(root, 'node_modules/@tauri-apps/cli/tauri.js');
// The Rust backend crates; plumdeck-backend serves the API and MCP, and
// plumdeck-maintenance replaces the old Python maintenance scripts.
const backendCrates = ['plumdeck-core', 'plumdeck-db', 'plumdeck-domain', 'plumdeck-media', 'plumdeck-key',
    'plumdeck-backend', 'plumdeck-analysis'];
function run(command, args, options = {}) {
    return new Promise((resolve, reject) => {
        const child = spawn(command, args, { cwd: root, stdio: 'inherit', ...options });
        child.on('error', reject);
        child.on('exit', (code, signal) => code === 0 ? resolve() : reject(new Error(`${command} exited (${signal || code})`)));
    });
}
function availableLoopbackPort() {
    return new Promise((resolve, reject) => {
        const server = createServer();
        server.once('error', reject);
        server.listen(0, '127.0.0.1', () => {
            const address = server.address();
            const port = typeof address === 'object' && address ? address.port : 0;
            server.close((error) => error ? reject(error) : resolve(port));
        });
    });
}
async function tauriStackDev() {
    const bridgePort = await availableLoopbackPort();
    const bridgeToken = randomBytes(32).toString('hex');
    const env = {
        ...process.env,
        PLUMDECK_JUNCTION_BRIDGE_URL: `http://127.0.0.1:${bridgePort}`,
        PLUMDECK_JUNCTION_BRIDGE_TOKEN: bridgeToken,
    };
    const children = [
        spawn(process.execPath, [fileURLToPath(import.meta.url), 'backend-dev'], {cwd: root, stdio: 'inherit', env}),
        spawn(process.execPath, [fileURLToPath(import.meta.url), 'tauri-dev'], {cwd: root, stdio: 'inherit', env}),
    ];
    const stop = (signal = 'SIGTERM') => {
        for (const child of children) if (child.exitCode === null && child.signalCode === null) child.kill(signal);
    };
    for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => stop(signal));
    await new Promise((resolve, reject) => {
        let settled = false;
        for (const child of children) {
            child.once('error', (error) => {
                if (settled) return;
                settled = true; stop(); reject(error);
            });
            child.once('exit', (code, signal) => {
                if (settled) return;
                settled = true; stop();
                if (code === 0 || signal === 'SIGINT' || signal === 'SIGTERM') resolve();
                else reject(new Error(`development process exited (${signal || code})`));
            });
        }
    });
}
async function backendDev() {
    if (existsSync(join(root, '.env'))) process.loadEnvFile(join(root, '.env'));
    const env = { ...process.env, ENV: 'dev', DB_PATH: process.env.DB_PATH || join(root, 'db_data/plumdeck.duckdb'),
        MUSIC_DIR: process.env.MUSIC_DIR || join(root, 'music_data') };
    env.PLUMDECK_PORT = env.PLUMDECK_PORT && env.PLUMDECK_PORT !== '0' ? env.PLUMDECK_PORT : '8001';
    await run('cargo', ['run', '--manifest-path', manifest, '-p', 'plumdeck-backend', '--bin', 'plumdeck-backend'], { env });
}
async function backendTest(args) {
    await run('cargo', ['test', '--manifest-path', manifest, ...backendCrates.flatMap((crate) => ['-p', crate]), ...args]);
}
async function engineBuild() {
    if (win) await run(process.execPath, [join(host, 'scripts/build-windows.mjs')]);
    else if (process.platform === 'darwin') await run('bash', [join(host, 'scripts/build-macos.sh')]);
    else throw new Error('Desktop audio builds target macOS or Windows.');
}
async function engineStage() {
    if (!win) await run('bash', [join(host, 'scripts/stage-macos.sh')]);
    if (!existsSync(stage)) throw new Error('Run pnpm dj-engine:build first.');
}
async function appBuild() {
    if (!existsSync(stage)) throw new Error('Build and stage the audio engine first.');
    // The release configs bundle FFmpeg (externalBin bin/ffmpeg, plus its dylibs
    // on macOS); stage it for the host triple. Dev builds use the base config and
    // the backend falls back to ffmpeg on PATH.
    await run(process.execPath, [join(root, 'scripts/packaging-ffmpeg.mjs')]);
    await run(process.execPath, [tauri, 'build', '--config', win ? 'src-tauri/tauri.windows.conf.json' : 'src-tauri/tauri.mixxx.conf.json']);
    if (!win) await run('bash', [join(host, 'scripts/sign-tauri-bundle-macos.sh')]);
}
try {
    switch (process.argv[2]) {
        case 'backend-dev': await backendDev(); break;
        case 'tauri-dev':
            await run(process.execPath, [tauri, 'dev'], { env: { ...process.env, TAURI_SKIP_SIDECAR: '1', TAURI_CONFIG: JSON.stringify({ bundle: { resources: [] } }) } });
            break;
        case 'tauri-stack-dev': await tauriStackDev(); break;
        case 'engine-build': await engineBuild(); break;
        case 'engine-stage': await engineStage(); break;
        case 'app-build': await appBuild(); break;
        case 'backend-tool':
            await run('cargo', ['run', '--manifest-path', manifest, '-p', 'plumdeck-backend', '--bin', 'plumdeck-maintenance',
                '--', ...process.argv.slice(3), '--root', root]);
            break;
        case 'backend-test': await backendTest(process.argv.slice(3)); break;
        case 'junction-test': {
            const executable = join(host, win ? 'build-seam/junction-core-tests.exe' : 'build-junction/junction-core-tests');
            await run(executable, process.argv.slice(3), { env: { ...process.env, JUNCTION_AUDIO_DEVICE_TEST: '1' } });
            break;
        }
        case 'pre-release':
        case 'package': {
            if (!win && process.platform !== 'darwin') throw new Error('Desktop packages target macOS or Windows.');
            // The backend is compiled into the Tauri app; there is no sidecar to build.
            await engineBuild(); await engineStage(); await appBuild();
            if (process.argv[2] === 'pre-release') {
                await run('cargo', ['build', '--release', '--manifest-path', manifest, '-p', 'plumdeck-backend', '--bin', 'plumdeck-backend']);
                const backend = join(root, `src-tauri/target/release/plumdeck-backend${win ? '.exe' : ''}`);
                await run(process.execPath, [join(root, 'scripts/smoke-backend.mjs'), backend]);
            }
            break;
        }
        case 'release': {
            const ref = process.argv[3];
            if (!ref || !/^v\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$/.test(ref)) throw new Error('Usage: pnpm release vX.Y.Z (an existing pushed tag)');
            await run('gh', ['workflow', 'run', 'release.yml', '--ref', ref]);
            break;
        }
        default: throw new Error('Unknown project command');
    }
} catch (error) { console.error(error.message); process.exitCode = 1; }
