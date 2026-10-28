#!/usr/bin/env node
// Select the FFmpeg used for recording conversion (WAV -> FLAC/MP3) and stage
// it as a Tauri sidecar (`bundle.externalBin: ["bin/ffmpeg"]`).
//
// Usage: node scripts/packaging-ffmpeg.mjs [--target <rust-triple>] [--check]
//   --target  x86_64-pc-windows-msvc | aarch64-apple-darwin | x86_64-apple-darwin
//             (default: derived from the running Node's platform/arch)
//   --check   only select and probe; print the chosen path, stage nothing
//
// Selection (first candidate that exists, matches the build architecture and
// lists the pcm_s16le, flac and libmp3lame encoders wins):
//   1. PLUMDECK_FFMPEG
//   2. Windows: Chocolatey's real executable under lib/ffmpeg*/tools/**
//      (bin/ffmpeg.exe is a shim launcher and is never bundled), then the PATH
//      ffmpeg (a Scoop shim is resolved through its .shim file)
//      macOS/Linux: PATH ffmpeg, /opt/homebrew/bin/ffmpeg, /usr/local/bin/ffmpeg
//
// Output (src-tauri/bin/ is git-ignored):
//   src-tauri/bin/ffmpeg-<triple>[.exe]  sidecar; Tauri renames it to ffmpeg[.exe]
//                                        next to the app executable
//   src-tauri/bin/ffmpeg-libs/           macOS only: non-system dylib closure,
//                                        bundled as a resource (tauri.mixxx.conf.json)
//
// macOS bundle layout, which the rewritten load commands assume:
//   plumdeck.app/Contents/MacOS/ffmpeg                   (externalBin)
//   plumdeck.app/Contents/Resources/ffmpeg-libs/*.dylib  (resource)
// ffmpeg loads @executable_path/../Resources/ffmpeg-libs/<lib>; each dylib loads
// its siblings via @loader_path/<lib>. No DYLD_* variables are needed at
// runtime. Rewritten files are ad-hoc signed here so they run unbundled;
// native/mixxx-engine-host/scripts/sign-tauri-bundle-macos.sh re-signs them
// inside the final bundle with the release identity.
import { spawnSync } from 'node:child_process';
import {
    chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync,
    realpathSync, rmSync, statSync, writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, delimiter, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const binDir = join(root, 'src-tauri/bin');
const libsDirName = 'ffmpeg-libs';
const requiredEncoders = ['pcm_s16le', 'flac', 'libmp3lame'];
// Install-name prefix used by the bundled ffmpeg (Contents/MacOS -> Contents/Resources).
const exeLibPrefix = `@executable_path/../Resources/${libsDirName}/`;

const triples = {
    'x86_64-pc-windows-msvc': { os: 'win32', arch: 'x86_64' },
    'aarch64-apple-darwin': { os: 'darwin', arch: 'arm64' },
    'x86_64-apple-darwin': { os: 'darwin', arch: 'x86_64' },
    // Not shipped; accepted so the selection logic can be exercised on Linux.
    'x86_64-unknown-linux-gnu': { os: 'linux', arch: 'x86_64' },
    'aarch64-unknown-linux-gnu': { os: 'linux', arch: 'aarch64' },
};

function fail(message) {
    throw new Error(`packaging-ffmpeg: ${message}`);
}

function parseArgs(argv) {
    const options = { target: undefined, check: false };
    for (let i = 0; i < argv.length; i += 1) {
        const arg = argv[i];
        if (arg === '--target') options.target = argv[++i];
        else if (arg.startsWith('--target=')) options.target = arg.slice('--target='.length);
        else if (arg === '--check') options.check = true;
        else fail(`unknown argument ${arg}`);
    }
    return options;
}

function hostTriple() {
    const arch = { x64: 'x86_64', arm64: 'aarch64' }[process.arch];
    const suffix = { win32: 'pc-windows-msvc', darwin: 'apple-darwin', linux: 'unknown-linux-gnu' }[process.platform];
    if (!arch || !suffix) fail(`unsupported host ${process.platform}/${process.arch}; pass --target`);
    return `${arch}-${suffix}`;
}

function run(command, args, { allowFailure = false, timeout = 60_000 } = {}) {
    const result = spawnSync(command, args, { encoding: 'utf8', timeout, maxBuffer: 64 * 1024 * 1024 });
    if (!allowFailure && (result.error || result.status !== 0)) {
        fail(`${command} ${args.join(' ')} failed: ${result.error?.message || result.stderr || `exit ${result.status}`}`);
    }
    return result;
}

function isFile(path) {
    try { return statSync(path).isFile(); } catch { return false; }
}

function which(name, os) {
    const exts = os === 'win32' ? (process.env.PATHEXT || '.EXE;.CMD;.BAT;.COM').split(';').filter(Boolean) : [''];
    for (const dir of (process.env.PATH || '').split(delimiter)) {
        if (!dir) continue;
        for (const ext of exts) {
            const candidate = join(dir, name + ext);
            if (isFile(candidate)) return candidate;
        }
    }
    return undefined;
}

// Equivalent of Python's sorted(glob("<lib>/ffmpeg*/tools/**/ffmpeg.exe")).
function chocolateyExecutables(chocolatey) {
    const lib = join(chocolatey, 'lib');
    const found = [];
    let packages = [];
    try { packages = readdirSync(lib, { withFileTypes: true }); } catch { return found; }
    for (const entry of packages) {
        if (!entry.isDirectory() || !entry.name.toLowerCase().startsWith('ffmpeg')) continue;
        const tools = join(lib, entry.name, 'tools');
        let files = [];
        try { files = readdirSync(tools, { recursive: true }); } catch { continue; }
        for (const file of files) {
            if (basename(String(file)).toLowerCase() === 'ffmpeg.exe') found.push(join(tools, String(file)));
        }
    }
    return found.sort();
}

function sameDir(a, b) {
    try { return realpathSync(a).toLowerCase() === realpathSync(b).toLowerCase(); } catch { return false; }
}

function buildCandidates(os) {
    const candidates = [];
    if (process.env.PLUMDECK_FFMPEG) candidates.push(process.env.PLUMDECK_FFMPEG);
    const selected = which('ffmpeg', os);
    if (os === 'win32') {
        const chocolatey = process.env.ChocolateyInstall || 'C:\\ProgramData\\chocolatey';
        candidates.push(...chocolateyExecutables(chocolatey));
        if (selected) {
            const scoopShim = selected.replace(/\.[^.\\/]*$/, '') + '.shim';
            if (isFile(scoopShim)) {
                for (const line of readFileSync(scoopShim, 'utf8').replace(/^\uFEFF/, '').split(/\r?\n/)) {
                    if (line.startsWith('path = ')) candidates.push(line.slice(7).trim().replace(/^"|"$/g, ''));
                }
            } else if (!sameDir(dirname(selected), join(chocolatey, 'bin'))) {
                candidates.push(selected);
            }
        }
    } else {
        candidates.push(selected, '/opt/homebrew/bin/ffmpeg', '/usr/local/bin/ffmpeg');
    }
    return [...new Set(candidates.filter(Boolean))];
}

function matchesArch(path, target) {
    if (target.os !== 'darwin') return true;
    return run('/usr/bin/lipo', [path, '-verify_arch', target.arch], { allowFailure: true }).status === 0;
}

function hasEncoders(path) {
    const probe = run(path, ['-hide_banner', '-encoders'], { allowFailure: true, timeout: 10_000 });
    if (probe.error || probe.status !== 0) return false;
    const words = new Set(probe.stdout.split(/\s+/));
    return requiredEncoders.every((encoder) => words.has(encoder));
}

function findFfmpeg(triple, target) {
    const rejected = [];
    for (const candidate of buildCandidates(target.os)) {
        if (!isFile(candidate)) { rejected.push(`${candidate}: not a file`); continue; }
        if (!matchesArch(candidate, target)) { rejected.push(`${candidate}: not ${target.arch}`); continue; }
        if (!hasEncoders(candidate)) { rejected.push(`${candidate}: missing one of ${requiredEncoders.join(', ')}`); continue; }
        return realpathSync(candidate);
    }
    fail(`FFmpeg with WAV/FLAC/MP3 encoders for ${triple} is required to package recordings.\n  `
        + (rejected.length ? rejected.join('\n  ') : 'no candidates (set PLUMDECK_FFMPEG or install ffmpeg)'));
}

// ---------------------------------------------------------------- macOS dylibs

// Slice inspected with otool when a library is universal.
let machoArch = 'arm64';
const isSystemLib = (path) => path.startsWith('/usr/lib/') || path.startsWith('/System/');

function machoInfo(path) {
    const id = run('/usr/bin/otool', ['-arch', machoArch, '-D', path]).stdout.split('\n').slice(1).map((s) => s.trim()).find(Boolean);
    const deps = run('/usr/bin/otool', ['-arch', machoArch, '-L', path]).stdout.split('\n').slice(1)
        .map((line) => line.trim().replace(/\s+\(compatibility version.*$/, '')).filter(Boolean)
        .filter((dep) => dep !== id);
    const rpaths = [];
    const lines = run('/usr/bin/otool', ['-arch', machoArch, '-l', path]).stdout.split('\n');
    for (let i = 0; i < lines.length; i += 1) {
        if (lines[i].trim() !== 'cmd LC_RPATH') continue;
        for (let j = i + 1; j < Math.min(i + 4, lines.length); j += 1) {
            const match = lines[j].trim().match(/^path (.+?) \(offset \d+\)$/);
            if (match) { rpaths.push(match[1]); break; }
        }
    }
    return { id, deps, rpaths };
}

function resolveInstallName(name, loaderPath, executablePath, rpaths) {
    const expand = (p) => p.replace(/^@loader_path/, dirname(loaderPath)).replace(/^@executable_path/, dirname(executablePath));
    if (name.startsWith('@rpath/')) {
        for (const rpath of rpaths) {
            const candidate = join(expand(rpath), name.slice('@rpath/'.length));
            if (isFile(candidate)) return realpathSync(candidate);
        }
        fail(`cannot resolve ${name} (loaded by ${loaderPath}; rpaths: ${rpaths.join(', ') || 'none'})`);
    }
    const candidate = name.startsWith('@') ? expand(name) : name;
    if (!isFile(candidate)) fail(`dependency ${name} of ${loaderPath} does not exist`);
    return realpathSync(candidate);
}

// Returns Map<bundledBasename, sourceRealPath> of every non-system dylib reachable from ffmpeg.
function dylibClosure(ffmpeg) {
    const libs = new Map();
    const exeRpaths = machoInfo(ffmpeg).rpaths;
    const queue = [ffmpeg];
    const seen = new Set();
    while (queue.length) {
        const file = queue.shift();
        if (seen.has(file)) continue;
        seen.add(file);
        const info = machoInfo(file);
        // @rpath lookups use the loader's LC_RPATHs, then the executable's.
        const rpaths = [...info.rpaths, ...(file === ffmpeg ? [] : exeRpaths)];
        for (const dep of info.deps) {
            if (isSystemLib(dep)) continue;
            const source = resolveInstallName(dep, file, ffmpeg, rpaths);
            const name = basename(dep);
            const known = libs.get(name);
            if (known && known !== source) fail(`two different libraries are both named ${name}: ${known}, ${source}`);
            libs.set(name, source);
            queue.push(source);
        }
    }
    return libs;
}

// Point every non-system load command of `file` at its bundled copy and drop
// rpaths that would let dyld fall back to the build machine's Homebrew.
function rewriteLoadCommands(file, prefix, ownId) {
    const info = machoInfo(file);
    const args = [];
    if (ownId) args.push('-id', ownId);
    for (const dep of info.deps) {
        if (!isSystemLib(dep)) args.push('-change', dep, prefix + basename(dep));
    }
    for (const rpath of new Set(info.rpaths)) {
        if (!rpath.startsWith('@')) args.push('-delete_rpath', rpath);
    }
    if (args.length) run('/usr/bin/install_name_tool', [...args, file]);
    run('/usr/bin/codesign', ['--force', '--sign', '-', file]);
}

function assertSelfContained(file) {
    for (const dep of machoInfo(file).deps) {
        if (!isSystemLib(dep) && !dep.startsWith('@executable_path/') && !dep.startsWith('@loader_path/')) {
            fail(`${file} still references ${dep}`);
        }
    }
}

function stageMacLibs(ffmpegSource, stagedFfmpeg, target) {
    const libsDir = join(binDir, libsDirName);
    machoArch = target.arch;
    const libs = dylibClosure(ffmpegSource);
    for (const [name, source] of libs) {
        if (!matchesArch(source, target)) fail(`${source} does not contain ${target.arch}`);
        const destination = join(libsDir, name);
        copyFileSync(source, destination);
        chmodSync(destination, 0o755); // Homebrew installs libraries read-only.
        rewriteLoadCommands(destination, '@loader_path/', exeLibPrefix + name);
    }
    rewriteLoadCommands(stagedFfmpeg, exeLibPrefix);
    for (const file of [stagedFfmpeg, ...[...libs.keys()].map((name) => join(libsDir, name))]) assertSelfContained(file);

    // Prove the rewritten binary runs from the bundle layout, without Homebrew.
    const scratch = mkdtempSync(join(tmpdir(), 'plumdeck-ffmpeg-'));
    try {
        const macos = join(scratch, 'Contents/MacOS');
        mkdirSync(macos, { recursive: true });
        copyFileSync(stagedFfmpeg, join(macos, 'ffmpeg'));
        cpSync(libsDir, join(scratch, 'Contents/Resources', libsDirName), { recursive: true });
        const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('DYLD_')));
        const probe = spawnSync(join(macos, 'ffmpeg'), ['-hide_banner', '-encoders'],
            { encoding: 'utf8', timeout: 10_000, env: { ...env, DYLD_PRINT_LIBRARIES: '1' } });
        if (probe.status !== 0) fail(`relocated ffmpeg failed to start: ${probe.stderr}`);
        const leaked = probe.stderr.split('\n').filter((line) => /\/(opt\/homebrew|usr\/local)\//.test(line));
        if (leaked.length) fail(`relocated ffmpeg still loads build-machine libraries:\n${leaked.join('\n')}`);
        if (!requiredEncoders.every((encoder) => probe.stdout.split(/\s+/).includes(encoder))) {
            fail('relocated ffmpeg lost required encoders');
        }
    } finally {
        rmSync(scratch, { recursive: true, force: true });
    }
    return libs.size;
}

// ---------------------------------------------------------------- main

function stage(source, triple, target) {
    mkdirSync(binDir, { recursive: true });
    const exe = target.os === 'win32' ? '.exe' : '';
    if (target.os === 'win32' && readdirSync(dirname(source)).some((f) => /^av(codec|format|util)-\d+\.dll$/i.test(f))) {
        // externalBin copies a single executable; a shared build would miss its DLLs.
        fail(`${source} is a shared FFmpeg build; use a static build (Chocolatey/Scoop ffmpeg are static)`);
    }
    const destination = join(binDir, `ffmpeg-${triple}${exe}`);
    rmSync(destination, { force: true });
    copyFileSync(source, destination);
    chmodSync(destination, 0o755);

    const libsDir = join(binDir, libsDirName);
    rmSync(libsDir, { recursive: true, force: true });
    let libCount = 0;
    if (target.os === 'darwin') {
        mkdirSync(libsDir, { recursive: true });
        // Keeps the resource directory non-empty even for a fully static ffmpeg.
        writeFileSync(join(libsDir, 'README.txt'),
            `Libraries for Contents/MacOS/ffmpeg, staged by scripts/packaging-ffmpeg.mjs from ${source}\n`);
        libCount = stageMacLibs(source, destination, target);
    }
    return { destination, libCount };
}

try {
    const options = parseArgs(process.argv.slice(2));
    const triple = options.target || hostTriple();
    const target = triples[triple];
    if (!target) fail(`unsupported target ${triple}; expected one of ${Object.keys(triples).join(', ')}`);
    if (target.os !== process.platform) fail(`target ${triple} must be staged on ${target.os}, not ${process.platform}`);
    const source = findFfmpeg(triple, target);
    if (options.check) {
        console.log(source);
    } else {
        const { destination, libCount } = stage(source, triple, target);
        console.log(`Staged ${source} -> ${resolve(destination)}${target.os === 'darwin' ? ` (+${libCount} dylibs in ${join(binDir, libsDirName)})` : ''}`);
    }
} catch (error) {
    console.error(error.message);
    process.exitCode = 1;
}
