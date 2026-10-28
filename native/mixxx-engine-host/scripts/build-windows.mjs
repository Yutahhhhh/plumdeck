// Build the pinned real Mixxx host from an MSVC developer command prompt.
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import {
    copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync,
    utimesSync, writeFileSync,
} from 'node:fs';
import { basename, delimiter, dirname, join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const DEPS = join(ROOT, 'build-deps');
const MIXXX = '3ebac449e7e5fe2a0186596657696e87ce8b0e56';
const VCPKG = '3d33b7f7a8121da6e4fa80b35618fc8e960be686';
const LDC = '20bf658a60881854984f8ffb1586a4722bf590ec';

const posix = (path) => path.replace(/\\/g, '/');

function run(command, ...args) {
    const result = spawnSync(String(command), args.map(String), { stdio: 'inherit' });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`${command} exited (${result.signal || result.status})`);
}

function output(command, ...args) {
    const result = spawnSync(String(command), args.map(String), { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`${command} exited (${result.signal || result.status})`);
    return result.stdout;
}

function succeeds(command, ...args) {
    return spawnSync(String(command), args.map(String), { stdio: 'ignore' }).status === 0;
}

/** Recursively yield files (not directories) below a directory. */
function* walkFiles(directory) {
    let entries;
    try {
        entries = readdirSync(directory, { withFileTypes: true });
    } catch {
        return;
    }
    for (const entry of entries) {
        const path = join(directory, entry.name);
        if (entry.isDirectory()) yield* walkFiles(path);
        else if (entry.isFile()) yield path;
    }
}

const isDirectory = (path) => {
    try { return statSync(path).isDirectory(); } catch { return false; }
};

function firstNamed(directory, name) {
    const wanted = name.toLowerCase();
    for (const file of walkFiles(directory)) if (basename(file).toLowerCase() === wanted) return file;
    throw new Error(`${name} was not found under ${directory}`);
}

/** shutil.copy2: copy contents and keep the modification time. */
function copy2(source, destination) {
    mkdirSync(dirname(destination), { recursive: true });
    copyFileSync(source, destination);
    const { atime, mtime } = statSync(source);
    utimesSync(destination, atime, mtime);
}

function which(name) {
    const extensions = (process.env.PATHEXT || '.EXE;.CMD;.BAT;.COM').split(';');
    for (const directory of (process.env.PATH || '').split(delimiter).filter(Boolean)) {
        for (const extension of ['', ...extensions]) {
            const candidate = join(directory, name + extension);
            try { if (statSync(candidate).isFile()) return candidate; } catch { /* keep looking */ }
        }
    }
    return null;
}

function fetch(url, revision, destination) {
    if (!existsSync(join(destination, '.git'))) {
        run('git', 'init', destination);
        run('git', '-C', destination, 'remote', 'add', 'origin', url);
        run('git', '-C', destination, 'fetch', '--depth', '1', 'origin', revision);
        run('git', '-C', destination, 'checkout', '--detach', 'FETCH_HEAD');
    }
    const actual = output('git', '-C', destination, 'rev-parse', 'HEAD').trim();
    if (actual !== revision) throw new Error(`Unexpected dependency revision: ${destination}`);
}

function sha256(path) {
    return createHash('sha256').update(readFileSync(path)).digest('hex');
}

function download(url, destination, digest) {
    mkdirSync(dirname(destination), { recursive: true });
    if (!existsSync(destination)) {
        const temporary = `${destination}.download`;
        run('curl.exe', '--fail', '--location', '--retry', '3', '--connect-timeout', '30', '--max-time', '1800',
            '--speed-time', '120', '--speed-limit', '1024', '--output', temporary, url);
        renameSync(temporary, destination);
    }
    if (digest && sha256(destination) !== digest) throw new Error(`Checksum mismatch: ${destination}`);
}

function configure(source, build, ...options) {
    run('cmake', '-S', source, '-B', build, '-G', 'Ninja', '-DCMAKE_BUILD_TYPE=Release',
        '-DCMAKE_POLICY_VERSION_MINIMUM=3.5', '-DCMAKE_CXX_FLAGS=/utf-8 /DNOMINMAX /EHsc', ...options);
}

function build(directory, ...targets) {
    run('cmake', '--build', directory, '--parallel', process.env.PLUMDECK_BUILD_JOBS || '4',
        ...(targets.length ? ['--target', ...targets] : []));
}

/** Normal and delay-loaded PE imports reported by the MSVC toolchain. */
function importedDlls(binary) {
    const text = output('dumpbin', '/DEPENDENTS', binary);
    const found = new Set();
    for (const match of text.matchAll(/^\s+(\S+\.dll)\s*$/gim)) found.add(match[1]);
    return [...found].sort();
}

/** Copy the executable's recursive DLL closure plus explicitly loaded Qt plugins. */
function stageRuntimeClosure(executable, stage, searchRoots, pluginSeeds) {
    const candidates = new Map();
    for (const root of searchRoots) {
        if (!root || !isDirectory(root)) continue;
        for (const file of walkFiles(root)) {
            const key = basename(file).toLowerCase();
            if (key.endsWith('.dll') && !candidates.has(key)) candidates.set(key, file);
        }
    }
    const queue = [];
    const staged = new Map();
    const copy = (source, destination) => {
        copy2(source, destination);
        staged.set(basename(source).toLowerCase(), destination);
        queue.push(destination);
    };
    copy(executable, join(stage, basename(executable)));
    for (const [relativePath, source] of pluginSeeds) copy(source, join(stage, relativePath));

    const unresolved = new Set();
    while (queue.length) {
        const binary = queue.pop();
        for (const dependency of importedDlls(binary)) {
            const key = dependency.toLowerCase();
            if (staged.has(key)) continue;
            const source = candidates.get(key);
            if (!source) {
                unresolved.add(dependency);
                continue;
            }
            copy(source, join(stage, basename(source)));
        }
    }
    // Windows system DLLs are intentionally unresolved. The relocated smoke test
    // runs with only System32 and this directory available, so any missing
    // redistributable dependency still fails the build.
    if (unresolved.size) {
        const names = [...unresolved].sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()));
        console.log('Runtime DLLs supplied by Windows:', names.join(', '));
    }
}

/** Retain redistributable notices without shipping CMake files, docs or locales. */
function copyNotices(source, destination) {
    if (!isDirectory(source)) return;
    const names = ['license', 'copying', 'copyright', 'notice'];
    for (const notice of walkFiles(source)) {
        const lower = basename(notice).toLowerCase();
        if (!(names.some((name) => lower.startsWith(name)) || lower.endsWith('.spdx.json'))) continue;
        copy2(notice, join(destination, relative(source, notice)));
    }
}

function main() {
    if (process.platform !== 'win32') throw new Error('Run from an x64 MSVC developer command prompt on Windows');
    mkdirSync(DEPS, { recursive: true });
    const upstream = join(ROOT, 'upstream');
    fetch('https://github.com/mixxxdj/mixxx.git', MIXXX, upstream);
    // Use the hash-verified dependencies from this exact Mixxx revision.
    const depsName = 'mixxx-deps-2.5-x64-windows-release-40c29ff';
    const archive = join(DEPS, `${depsName}.zip`);
    const prefixRoot = join(DEPS, depsName);
    if (!existsSync(prefixRoot)) {
        download(`https://downloads.mixxx.org/dependencies/2.5-rel/Windows/${depsName}.zip`, archive,
            'a9d809ae9c52d8a553af1bb8a58565649ced7b1f938d1d37c1c7d83ad53aacf3');
        // Windows' bundled bsdtar handles Qt's deeply nested object paths in
        // this verified archive and keeps extraction usable from an ordinary
        // developer checkout under the user profile.
        run('tar.exe', '-xf', archive, '-C', DEPS);
    }
    const prefix = join(prefixRoot, 'installed', 'x64-windows-release');
    if (!isDirectory(prefix)) throw new Error('Mixxx dependency archive has an unexpected layout');
    const vcpkg = join(DEPS, 'vcpkg-junction');
    fetch('https://github.com/microsoft/vcpkg.git', VCPKG, vcpkg);
    if (!existsSync(join(vcpkg, 'vcpkg.exe'))) run('cmd', '/c', join(vcpkg, 'bootstrap-vcpkg.bat'), '-disableMetrics');
    const triplets = join(DEPS, 'triplets');
    mkdirSync(triplets, { recursive: true });
    writeFileSync(join(triplets, 'x64-windows-release.cmake'),
        'set(VCPKG_TARGET_ARCHITECTURE x64)\nset(VCPKG_CRT_LINKAGE dynamic)\n'
        + 'set(VCPKG_LIBRARY_LINKAGE dynamic)\nset(VCPKG_BUILD_TYPE release)\n');
    run(join(vcpkg, 'vcpkg.exe'), 'install', 'libnice:x64-windows-release', 'opus:x64-windows-release',
        '--host-triplet=x64-windows-release', `--overlay-triplets=${triplets}`, '--clean-after-build');
    const extra = join(vcpkg, 'installed', 'x64-windows-release');
    const prefixes = `${posix(prefix)};${posix(extra)}`;
    const ldc = join(DEPS, 'libdatachannel');
    fetch('https://github.com/paullouisageneau/libdatachannel.git', LDC, ldc);
    run('git', '-C', ldc, 'submodule', 'update', '--init', '--recursive', '--depth', '1');
    const pkgconfs = [...walkFiles(join(vcpkg, 'downloads', 'tools'))]
        .filter((file) => basename(file).toLowerCase() === 'pkgconf.exe');
    const parts = (file) => file.split(sep);
    pkgconfs.sort((a, b) => (Number(!parts(a).includes('mingw64')) - Number(!parts(b).includes('mingw64')))
        || (parts(a).length - parts(b).length));
    const pkg = pkgconfs[0] || which('pkg-config');
    if (!pkg) throw new Error("vcpkg's pkgconf executable was not found");
    process.env.PKG_CONFIG_PATH = join(extra, 'lib', 'pkgconfig');
    configure(ldc, join(ldc, 'build-windows'), `-DCMAKE_PREFIX_PATH=${prefixes}`, `-DPKG_CONFIG_EXECUTABLE=${pkg}`,
        `-DCMAKE_INSTALL_PREFIX=${posix(join(ldc, 'install'))}`, '-DBUILD_SHARED_LIBS=ON', '-DUSE_NICE=ON',
        '-DNO_MEDIA=OFF', '-DNO_WEBSOCKET=OFF', '-DNO_EXAMPLES=ON', '-DNO_TESTS=ON');
    build(join(ldc, 'build-windows'));
    run('cmake', '--install', join(ldc, 'build-windows'));
    for (const [name, url, filename] of [
        ['soundtouch', 'https://codeberg.org/soundtouch/soundtouch/archive/2.4.1.tar.gz', 'source.tar.gz'],
        ['rubberband', 'https://breakfastquay.com/files/releases/rubberband-4.0.0.tar.bz2', 'source.tar.bz2'],
    ]) download(url, join(DEPS, name, filename));
    // These preparation scripts check the source archive hashes before extraction.
    const st = join(DEPS, 'soundtouch');
    run(process.execPath, join(ROOT, 'scripts/prepare-soundtouch.mjs'), st);
    configure(join(st, 'source'), join(st, 'build'), '-DBUILD_SHARED_LIBS=OFF', '-DSOUNDSTRETCH=OFF',
        '-DSOUNDTOUCH_DLL=OFF', '-DOPENMP=OFF', '-DNEON=OFF');
    build(join(st, 'build'));
    const rb = join(DEPS, 'rubberband');
    run(process.execPath, join(ROOT, 'scripts/prepare-rubberband.mjs'), rb);
    mkdirSync(join(rb, 'build-config'), { recursive: true });
    copy2(join(ROOT, 'cmake/rubberband-source.cmake'), join(rb, 'build-config/CMakeLists.txt'));
    configure(join(rb, 'build-config'), join(rb, 'build'), `-DCMAKE_PREFIX_PATH=${prefixes}`,
        `-DRB_SOURCE=${posix(join(rb, 'source'))}`);
    build(join(rb, 'build'));
    const patch = join(ROOT, 'patches/recording-frame-clock.patch');
    if (!succeeds('git', '-C', upstream, 'apply', '--reverse', '--check', patch)) {
        run('git', '-C', upstream, 'apply', '--check', patch);
        run('git', '-C', upstream, 'apply', patch);
    }
    const allPrefixes = `${prefixes};${posix(join(ldc, 'install'))}`;
    const options = [`-DCMAKE_PREFIX_PATH=${allPrefixes}`, `-DMIXXX_VCPKG_ROOT=${posix(prefixRoot)}`,
        '-DVCPKG_TARGET_TRIPLET=x64-windows-release', '-DQML=OFF', '-DBUILD_TESTING=OFF', '-DBUILD_BENCH=OFF',
        '-DENGINEPRIME=OFF', '-DKEYFINDER=OFF', '-DPORTMIDI=OFF', '-DBROADCAST=OFF', '-DQTKEYCHAIN=OFF',
        '-DHID=OFF', '-DBULK=OFF', '-DVINYLCONTROL=OFF', '-DFFMPEG=OFF', '-DMAD=ON', '-DBATTERY=OFF', '-DLILV=OFF',
        '-DOPTIMIZE=portable', '-DWARNINGS_FATAL=OFF',
        `-DCMAKE_PROJECT_mixxx_INCLUDE=${posix(join(ROOT, 'cmake/inject-host.cmake'))}`];
    configure(upstream, join(ROOT, 'build-upstream'), ...options);
    build(join(ROOT, 'build-upstream'), 'plumdeck-mixxx-engine-host');
    configure(ROOT, join(ROOT, 'build-seam'), `-DCMAKE_PREFIX_PATH=${allPrefixes}`);
    build(join(ROOT, 'build-seam'));
    // Stage a complete relocatable directory. The dependency archives contain
    // every Mixxx/Qt development DLL and more than 100 MiB of GUI resources;
    // this headless host must ship only its recursive PE closure and the Qt
    // plugins it loads by name.
    const stage = join(ROOT, 'stage', 'PlumdeckMixxxHost');
    rmSync(stage, { recursive: true, force: true });
    mkdirSync(stage, { recursive: true });
    const executable = join(ROOT, 'build-upstream/plumdeck-mixxx-engine-host.exe');
    const plugins = [
        [join('platforms', 'qoffscreen.dll'), firstNamed(prefix, 'qoffscreen.dll')],
        [join('sqldrivers', 'qsqlite.dll'), firstNamed(prefix, 'qsqlite.dll')],
        [join('tls', 'qschannelbackend.dll'), firstNamed(prefix, 'qschannelbackend.dll')],
    ];
    const searchRoots = [join(prefix, 'bin'), join(extra, 'bin'), join(ldc, 'install/bin')];
    // windeployqt previously copied vc_redist.x64.exe into the application but
    // nothing executed it. Copy the imported VC runtime DLLs app-locally instead.
    const redist = process.env.VCToolsRedistDir;
    if (redist) searchRoots.push(join(redist, 'x64'));
    stageRuntimeClosure(executable, stage, searchRoots, plugins);

    // The host creates no window, skin, library or controller subsystem. Effects
    // and the settings schema are the only upstream runtime resources it uses.
    cpSync(join(upstream, 'res/effects'), join(stage, 'res/effects'), { recursive: true, preserveTimestamps: true });
    copy2(join(upstream, 'res/schema.xml'), join(stage, 'res/schema.xml'));

    copyNotices(join(extra, 'share'), join(stage, 'licenses/junction'));
    copyNotices(join(prefix, 'share'), join(stage, 'licenses/mixxx-dependencies'));
    copy2(join(upstream, 'LICENSE'), join(stage, 'LICENSE-Mixxx'));
    // These libraries are built outside the Mixxx/vcpkg prefixes. Retain their
    // notices as well, including libdatachannel's bundled dependencies.
    for (const [name, source] of [['SoundTouch', join(st, 'source')], ['RubberBand', join(rb, 'source')],
        ['libdatachannel', ldc]]) copyNotices(source, join(stage, 'licenses', name));
    copy2(join(ROOT, 'dependency-versions.json'), join(stage, 'dependency-versions.json'));
    const stagedFiles = [...walkFiles(stage)];
    const stagedBytes = stagedFiles.reduce((total, file) => total + statSync(file).size, 0);
    console.log(`Staged Windows native engine: ${stagedFiles.length} files, ${(stagedBytes / 1024 / 1024).toFixed(1)} MiB`);
    if (stagedFiles.length > 500 || stagedBytes > 160 * 1024 * 1024) {
        throw new Error('Windows native stage unexpectedly contains development or GUI payloads');
    }
    process.env.PATH = stage + delimiter + process.env.PATH;
    run('ctest', '--test-dir', join(ROOT, 'build-seam'), '--output-on-failure');
    run(process.execPath, join(ROOT, 'scripts/smoke-bundle.mjs'), join(stage, 'plumdeck-mixxx-engine-host.exe'));
}

try {
    main();
} catch (error) {
    console.error(error.message);
    process.exitCode = 1;
}
