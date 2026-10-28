// Shared by prepare-rubberband.mjs and prepare-soundtouch.mjs: verify a pinned
// source archive's SHA-256, then copy its regular files (without the archive's
// top-level folder) into <root>/source. Files whose bytes are unchanged are not
// rewritten, so incremental CMake builds keep their timestamps. Extraction uses
// the system `tar` (GNU tar, bsdtar or Windows' tar.exe), which auto-detects
// gzip/bzip2 compression.
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { join, relative, sep } from 'node:path';

// On Windows use the bundled bsdtar: a Git-for-Windows GNU tar on PATH would
// read "C:" in an archive path as a remote host.
const tarCommand = process.platform === 'win32'
    ? join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'tar.exe') : 'tar';

function tar(args) {
    const result = spawnSync(tarCommand, args, { encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`tar ${args[0]} failed: ${(result.stderr || '').trim()}`);
    return result.stdout;
}

function* regularFiles(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
        const path = join(directory, entry.name);
        if (entry.isDirectory()) yield* regularFiles(path);
        // Symlinks, devices and other non-regular members are skipped, as
        // tarfile's isfile() check did.
        else if (entry.isFile()) yield path;
    }
}

export function writeIfChanged(target, data) {
    if (existsSync(target) && readFileSync(target).equals(data)) return;
    mkdirSync(join(target, '..'), { recursive: true });
    writeFileSync(target, data);
}

/**
 * @param {object} options
 * @param {string} options.root dependency folder holding the archive
 * @param {string} options.archive archive file name inside root
 * @param {string} options.prefix the archive's single top-level folder
 * @param {string} options.sha256 expected archive digest
 * @param {(parts: string[], data: Buffer) => void} [options.onFile] extra handling per file
 */
export function extractPinnedSource({ root, archive, prefix, sha256, onFile }) {
    const archivePath = join(root, archive);
    const digest = createHash('sha256').update(readFileSync(archivePath)).digest('hex');
    if (digest !== sha256) throw new Error(`Checksum mismatch: ${archivePath}`);
    // Every member must live under the expected folder and none may climb out.
    for (const name of tar(['-tf', archivePath]).split(/\r?\n/).filter(Boolean)) {
        const parts = name.replace(/\\/g, '/').split('/').filter((part) => part && part !== '.');
        if (parts[0] !== prefix) throw new Error(`Unexpected archive member outside ${prefix}/: ${name}`);
        if (parts.includes('..')) throw new Error(`Unsafe archive member: ${name}`);
    }
    const scratch = mkdtempSync(join(root, '.extract-'));
    try {
        tar(['-xf', archivePath, '-C', scratch]);
        const top = join(scratch, prefix);
        for (const file of regularFiles(top)) {
            const parts = relative(top, file).split(sep);
            const data = readFileSync(file);
            writeIfChanged(join(root, 'source', ...parts), data);
            onFile?.(parts, data);
        }
    } finally {
        rmSync(scratch, { recursive: true, force: true });
    }
}
