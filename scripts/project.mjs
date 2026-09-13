// OS-aware commands. Paths are passed as arguments, including spaces and Unicode.
import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, copyFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { createServer } from 'node:net';
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const win = process.platform === 'win32';
const python = process.env.PLUMDECK_PYTHON || (win ? 'python' : 'python3');
const venv = join(root, 'backend', '.venv', win ? 'Scripts/python.exe' : 'bin/python');
const host = join(root, 'native/mixxx-engine-host');
const stage = win ? join(host, 'stage/PlumdeckMixxxHost/plumdeck-mixxx-engine-host.exe')
    : join(host, 'stage/PlumdeckMixxxHost.app/Contents/MacOS/plumdeck-mixxx-engine-host');
const tauri = join(root, 'node_modules/@tauri-apps/cli/tauri.js');
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
    requireVenv();
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
function requireVenv() {
    if (!existsSync(venv)) throw new Error('Run pnpm backend:install first.');
}
async function backendBuild() {
    requireVenv();
    if (!win && process.platform !== 'darwin') throw new Error('Desktop packages target macOS or Windows.');
    if (win && process.arch !== 'x64') throw new Error('Use the Windows x64 Node/Python/MSVC toolchain.');
    const machines = process.arch === 'arm64' ? ['arm64', 'aarch64'] : ['amd64', 'x86_64'];
    await run(venv, ['-c', 'import platform,sys; assert sys.maxsize>2**32 and platform.machine().lower() in sys.argv[1:], "Use a 64-bit Python matching the Node/desktop architecture"', ...machines]);
    await run(venv, ['-m', 'PyInstaller', '--clean', '--noconfirm', 'plumdeck-server.spec'], { cwd: join(root, 'backend') });
    const triple = win ? 'x86_64-pc-windows-msvc' : `${process.arch === 'arm64' ? 'aarch64' : 'x86_64'}-apple-darwin`;
    const destination = join(root, `src-tauri/bin/plumdeck-server-${triple}${win ? '.exe' : ''}`);
    mkdirSync(join(root, 'src-tauri/bin'), { recursive: true });
    copyFileSync(join(root, `backend/dist/plumdeck-server${win ? '.exe' : ''}`), destination);
    return destination;
}
async function engineBuild() {
    if (win) await run(python, [join(host, 'scripts/build-windows.py')]);
    else if (process.platform === 'darwin') await run('bash', [join(host, 'scripts/build-macos.sh')]);
    else throw new Error('Desktop audio builds target macOS or Windows.');
}
async function engineStage() {
    if (!win) await run('bash', [join(host, 'scripts/stage-macos.sh')]);
    if (!existsSync(stage)) throw new Error('Run pnpm dj-engine:build first.');
}
async function appBuild() {
    if (!existsSync(stage)) throw new Error('Build and stage the audio engine first.');
    await run(process.execPath, [tauri, 'build', '--config', win ? 'src-tauri/tauri.windows.conf.json' : 'src-tauri/tauri.mixxx.conf.json']);
    if (!win) await run('bash', [join(host, 'scripts/sign-tauri-bundle-macos.sh')]);
}
try {
    switch (process.argv[2]) {
        case 'backend-install':
            if (!existsSync(venv)) await run(python, ['-m', 'venv', join(root, 'backend/.venv')]);
            await run(venv, ['-m', 'pip', 'install', '-r', 'requirements.txt'], { cwd: join(root, 'backend') });
            break;
        case 'backend-dev': {
            requireVenv();
            if (existsSync(join(root, '.env'))) process.loadEnvFile(join(root, '.env'));
            const env = { ...process.env, ENV: 'dev', DB_PATH: process.env.DB_PATH || join(root, 'db_data/plumdeck.duckdb'),
                MUSIC_DIR: process.env.MUSIC_DIR || join(root, 'music_data'), TF_CPP_MIN_LOG_LEVEL: '3' };
            const port = env.PLUMDECK_PORT && env.PLUMDECK_PORT !== '0' ? env.PLUMDECK_PORT : '8001';
            const args = ['-m', 'uvicorn', 'main:app', '--host', '127.0.0.1', '--port', port, '--reload',
                '--reload-exclude', '*.duckdb', '--reload-exclude', '*.duckdb.wal', '--reload-exclude', '*.log'];
            if (existsSync(join(root, '.env'))) args.push('--env-file', join(root, '.env'));
            await run(venv, args, { cwd: join(root, 'backend'), env });
            break;
        }
        case 'tauri-dev':
            await run(process.execPath, [tauri, 'dev'], { env: { ...process.env, TAURI_SKIP_SIDECAR: '1', TAURI_CONFIG: JSON.stringify({ bundle: { resources: [] } }) } });
            break;
        case 'tauri-stack-dev': await tauriStackDev(); break;
        case 'engine-build': await engineBuild(); break;
        case 'engine-stage': await engineStage(); break;
        case 'app-build': await appBuild(); break;
        case 'backend-tool':
            requireVenv();
            await run(venv, [join(root, 'scripts/maintenance.py'), ...process.argv.slice(3)]);
            break;
        case 'backend-test':
            requireVenv();
            await run(venv, ['-m', 'pytest', 'backend/tests', ...process.argv.slice(3)]);
            break;
        case 'backend-build': await backendBuild(); break;
        case 'junction-test': {
            const executable = join(host, win ? 'build-seam/junction-core-tests.exe' : 'build-junction/junction-core-tests');
            await run(executable, process.argv.slice(3), { env: { ...process.env, JUNCTION_AUDIO_DEVICE_TEST: '1' } });
            break;
        }
        case 'pre-release':
        case 'package': {
            const sidecar = await backendBuild();
            await engineBuild(); await engineStage(); await appBuild();
            if (process.argv[2] === 'pre-release') {
                await run(venv, [join(root, 'scripts/smoke-backend.py'), sidecar]);
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
