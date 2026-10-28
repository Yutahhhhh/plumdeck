// Prepare signed updates after final bundle sealing; combine only complete releases.
// Usage:
//   node scripts/release-artifacts.mjs validate
//   node scripts/release-artifacts.mjs <darwin-aarch64|windows-x86_64> <output dir>
//   node scripts/release-artifacts.mjs combine <dir>
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, unlinkSync, utimesSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
export const CONFIG = JSON.parse(readFileSync(join(ROOT, 'src-tauri/tauri.conf.json'), 'utf8'));
export const VERSION = CONFIG.version;
const TAURI = join(ROOT, 'node_modules/@tauri-apps/cli/tauri.js');

const glob = (directory, suffix) => existsSync(directory)
    ? readdirSync(directory).filter((name) => name.endsWith(suffix)).sort().map((name) => join(directory, name))
    : [];
const secondLine = (base64) => Buffer.from(base64, 'base64').toString('utf8').split(/\r\n|\r|\n/)[1];
const json = (value) => `${JSON.stringify(value, null, 2)}\n`;

/** urllib.parse.quote with its default safe='/'. */
function quote(text) {
    return encodeURIComponent(text).replace(/[!'()*]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`)
        .replace(/%2F/g, '/');
}

function copy2(source, destination) {
    copyFileSync(source, destination);
    const { atime, mtime } = statSync(source);
    utimesSync(destination, atime, mtime);
}

export function validate() {
    if (process.env.GITHUB_REF_NAME !== `v${VERSION}`) throw new Error('Release tag must match the version in tauri.conf.json');
    if (!process.env.TAURI_SIGNING_PRIVATE_KEY) throw new Error('Configure TAURI_SIGNING_PRIVATE_KEY before publishing updates');
}

export function sign(path) {
    const env = {
        ...process.env,
        TAURI_PRIVATE_KEY: process.env.TAURI_SIGNING_PRIVATE_KEY,
        TAURI_PRIVATE_KEY_PASSWORD: process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD || '',
    };
    const result = spawnSync(process.execPath, [TAURI, 'signer', 'sign', String(path)], { env, encoding: 'utf8' });
    if (result.error) throw result.error;
    if (result.status !== 0) {
        // The CLI's own message (e.g. "Wrong password for that key") never
        // includes the key or password; surface it so a bad
        // TAURI_SIGNING_PRIVATE_KEY_PASSWORD is obvious from the workflow log.
        throw new Error(`tauri signer sign failed: ${(result.stderr || result.stdout || '').trim()}`);
    }
    const signature = readFileSync(`${path}.sig`, 'utf8').trim();
    const publicKey = Buffer.from(secondLine(CONFIG.plugins.updater.pubkey), 'base64').subarray(2, 10);
    const signedKey = Buffer.from(secondLine(signature), 'base64').subarray(2, 10);
    if (!publicKey.equals(signedKey)) throw new Error('Signing key does not match the public key embedded in the app');
    return signature;
}

function archiveApp(app, update) {
    // Same layout as tarfile's add(app, arcname=app.name): symlinks inside the
    // bundle are stored as links. COPYFILE_DISABLE keeps macOS bsdtar from
    // adding AppleDouble "._" entries.
    const result = spawnSync('tar', ['-czf', update, '-C', dirname(app), basename(app)],
        { stdio: 'inherit', env: { ...process.env, COPYFILE_DISABLE: '1' } });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`tar exited (${result.signal || result.status})`);
}

export function prepare(platform, output) {
    validate();
    mkdirSync(output, { recursive: true });
    let files;
    let update;
    let msi;
    if (platform === 'darwin-aarch64') {
        const bundle = join(ROOT, 'src-tauri/target/aarch64-apple-darwin/release/bundle');
        const app = join(bundle, 'macos/plumdeck.app');
        assert.ok(existsSync(app) && statSync(app).isDirectory(), app);
        update = join(output, `plumdeck_${VERSION}_aarch64.app.tar.gz`);
        archiveApp(app, update);
        files = glob(join(bundle, 'dmg'), '.dmg');
        assert.ok(files.length, 'DMG missing');
    } else if (platform === 'windows-x86_64') {
        const bundle = join(ROOT, 'src-tauri/target/release/bundle');
        const nsis = glob(join(bundle, 'nsis'), '.exe');
        msi = glob(join(bundle, 'msi'), '.msi');
        assert.ok(nsis.length === 1 && msi.length === 1, 'Windows installers missing or ambiguous');
        files = [...nsis, ...msi];
        update = join(output, basename(nsis[0]));
    } else {
        throw new Error(`Unknown platform: ${platform}`);
    }
    for (const file of files) copy2(file, join(output, basename(file)));
    const signature = sign(update);
    // MSI downloads are signed too, even though automatic updates use NSIS.
    if (platform === 'windows-x86_64') sign(join(output, basename(msi[0])));
    const repository = process.env.GITHUB_REPOSITORY;
    if (!repository) throw new Error('GITHUB_REPOSITORY is not set');
    const base = (process.env.PLUMDECK_RELEASE_DOWNLOAD_BASE || '').replace(/\/+$/, '')
        || `https://github.com/${repository}/releases/download`;
    const part = { version: VERSION, platform, signature, url: `${base}/v${VERSION}/${quote(basename(update))}` };
    writeFileSync(join(output, `update-${platform}.json`), json(part));
}

export function combine(directory) {
    const paths = readdirSync(directory).filter((name) => /^update-.*\.json$/.test(name)).map((name) => join(directory, name));
    const parts = paths.map((path) => JSON.parse(readFileSync(path, 'utf8')));
    const platforms = new Set(parts.map((part) => part.platform));
    assert.ok(parts.length === 2 && platforms.size === 2 && platforms.has('darwin-aarch64') && platforms.has('windows-x86_64'),
        'Both darwin-aarch64 and windows-x86_64 update parts are required');
    assert.ok(parts.every((part) => part.version === VERSION && part.signature && String(part.url).startsWith('https://')),
        'Update parts must match the version, be signed and use https');
    const manifest = {
        version: VERSION,
        notes: `plumdeck ${VERSION}`,
        // datetime.now(timezone.utc).isoformat() form; RFC 3339 either way.
        pub_date: new Date().toISOString().replace('Z', '+00:00'),
        platforms: Object.fromEntries(parts.map((part) => [part.platform, { signature: part.signature, url: part.url }])),
    };
    writeFileSync(join(directory, 'latest.json'), json(manifest));
    for (const path of paths) unlinkSync(path);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
    try {
        const [command, target] = process.argv.slice(2);
        if (command === 'validate') validate();
        else if (command === 'combine') combine(resolve(target));
        else if (command && target) prepare(command, resolve(target));
        else throw new Error('Usage: release-artifacts.mjs validate | combine <dir> | <platform> <output dir>');
    } catch (error) {
        console.error(error.message);
        process.exitCode = 1;
    }
}
