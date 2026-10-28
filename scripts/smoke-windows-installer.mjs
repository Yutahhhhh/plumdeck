// Install this build in a relocated directory and exercise the installed desktop app.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const SYSTEM32 = join(process.env.SystemRoot || 'C:\\Windows', 'System32');
const MSIEXEC = join(SYSTEM32, 'msiexec.exe');
const POWERSHELL = join(SYSTEM32, 'WindowsPowerShell/v1.0/powershell.exe');
const repository = dirname(dirname(fileURLToPath(import.meta.url)));

/** Best-effort snapshot when an msiexec call refuses to return. */
function dumpInstallerState(log) {
    try {
        spawnSync(join(SYSTEM32, 'tasklist.exe'), ['/v', '/fi', 'imagename eq msiexec.exe'], { stdio: 'inherit', timeout: 30_000 });
    } catch (error) {
        console.error(`tasklist failed: ${error.message}`);
    }
    const script = `if (Test-Path -LiteralPath '${log}') { Get-Content -LiteralPath '${log}' -Tail 250 } `
        + "else { 'no install.log was created' }; "
        + "Get-WinEvent -FilterHashtable @{LogName='Application';ProviderName='MsiInstaller';"
        + 'StartTime=(Get-Date).AddMinutes(-30)} -ErrorAction SilentlyContinue | '
        + 'Select-Object -First 30 TimeCreated,Id,LevelDisplayName,Message | Format-List';
    try {
        spawnSync(POWERSHELL, ['-NoProfile', '-NonInteractive', '-Command', script], { stdio: 'inherit', timeout: 90_000 });
    } catch (error) {
        console.error(`state dump failed: ${error.message}`);
    }
}

/**
 * Run msiexec with a pre-quoted argument string and force-kill its tree on overrun.
 *
 * The arguments are passed verbatim so msiexec receives exactly the quoting we
 * intend: property values such as INSTALLDIR="C:\dir with spaces" must keep the
 * quotes around the value, which per-argument quoting cannot express (it quotes
 * the whole "INSTALLDIR=..." token, and msiexec then splits the value at the
 * first space).
 */
function runMsi(argumentsLine, timeout, log) {
    return new Promise((resolve, reject) => {
        const child = spawn(MSIEXEC, [argumentsLine], {
            argv0: `"${MSIEXEC}"`, windowsVerbatimArguments: true, stdio: 'inherit',
        });
        const timer = setTimeout(() => {
            if (log) dumpInstallerState(log);
            spawnSync(join(SYSTEM32, 'taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
            child.kill();
            reject(new Error(`msiexec did not finish within ${timeout / 1000}s`));
        }, timeout);
        child.once('error', (error) => { clearTimeout(timer); reject(error); });
        child.once('exit', (code) => { clearTimeout(timer); resolve(code); });
    });
}

function readLog(path) {
    const data = readFileSync(path);
    // msiexec writes UTF-16LE with a BOM.
    if (data[0] === 0xff && data[1] === 0xfe) return data.subarray(2).toString('utf16le');
    return data.toString('utf8');
}

async function main() {
    if (process.platform !== 'win32') throw new Error('The installer smoke test runs on Windows');
    const msiDirectory = join(repository, 'src-tauri/target/release/bundle/msi');
    const installer = readdirSync(msiDirectory).filter((name) => name.endsWith('.msi')).map((name) => join(msiDirectory, name))[0];
    if (!installer) throw new Error(`No MSI found in ${msiDirectory}`);
    const temporary = mkdtempSync(join(tmpdir(), 'Plumdeck インストール '));
    const directory = join(temporary, 'Application files');
    const log = join(temporary, 'install.log');
    try {
        try {
            const started = Date.now();
            const code = await runMsi(`/i "${installer}" /qn /norestart INSTALLDIR="${directory}" /l*v "${log}"`, 600_000, log);
            console.log(`Installer finished in ${Math.round((Date.now() - started) / 1000)}s with code ${code}`);
            if (code !== 0 && code !== 3010) throw new Error(`Installer exited: ${code}`);
            const app = join(directory, 'plumdeck.exe');
            if (!existsSync(app) || !statSync(app).isFile()) throw new Error(`Installed app missing: ${directory}`);
            if (!existsSync(join(directory, 'engine/plumdeck-mixxx-engine-host.exe'))) {
                throw new Error('Installed audio engine missing');
            }
            for (let run = 0; run < 2; run++) {
                const result = spawnSync(process.execPath, [join(repository, 'scripts/smoke-backend.mjs'), app, '--desktop'],
                    { stdio: 'inherit', timeout: 240_000 });
                if (result.error) throw result.error;
                if (result.status !== 0) throw new Error(`Desktop smoke run ${run + 1} failed (${result.signal || result.status})`);
            }
            console.log('Installed Windows desktop app and MCP passed');
        } catch (error) {
            if (existsSync(log)) console.error(readLog(log).slice(-16000));
            throw error;
        } finally {
            try {
                await runMsi(`/x "${installer}" /qn /norestart`, 300_000);
            } catch (error) {
                console.error(`Cleanup uninstall did not complete: ${error.message}`);
            }
        }
    } finally {
        rmSync(temporary, { recursive: true, force: true, maxRetries: 5, retryDelay: 1000 });
    }
}

main().catch((error) => {
    console.error(error.stack || error.message);
    process.exitCode = 1;
});
