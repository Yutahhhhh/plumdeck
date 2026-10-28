// Verify release signing and all-platform publication with disposable keys.
// Run: node --test scripts/test-release-artifacts.mjs
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import * as release from './release-artifacts.mjs';

function withEnv(values, action) {
    const previous = Object.fromEntries(Object.keys(values).map((key) => [key, process.env[key]]));
    Object.assign(process.env, values);
    try {
        return action();
    } finally {
        for (const [key, value] of Object.entries(previous)) {
            if (value === undefined) delete process.env[key];
            else process.env[key] = value;
        }
    }
}

test('real signing and incomplete release rejected', () => {
    const root = mkdtempSync(join(tmpdir(), 'plumdeck-release-'));
    try {
        const key = join(root, 'disposable.key');
        const generated = spawnSync(process.execPath, [join(release.ROOT, 'node_modules/@tauri-apps/cli/tauri.js'),
            'signer', 'generate', '--ci', '-p', 'disposable-test', '-w', key], { encoding: 'utf8' });
        assert.equal(generated.status, 0, 'tauri signer generate failed');
        const payload = join(root, 'installer.exe');
        writeFileSync(payload, 'Disposable test payload');
        let signature;
        withEnv({ TAURI_SIGNING_PRIVATE_KEY: readFileSync(key, 'utf8').trim(),
            TAURI_SIGNING_PRIVATE_KEY_PASSWORD: 'disposable-test' }, () => {
            assert.throws(() => release.sign(payload), /does not match/);
            const updater = release.CONFIG.plugins.updater;
            const embedded = updater.pubkey;
            updater.pubkey = readFileSync(`${key}.pub`, 'utf8').trim();
            try {
                signature = release.sign(payload);
            } finally {
                updater.pubkey = embedded;
            }
        });
        const part = (platform) => writeFileSync(join(root, `update-${platform}.json`), JSON.stringify({
            version: release.VERSION, platform, signature, url: 'https://example.com/installer.exe' }));
        part('darwin-aarch64');
        assert.throws(() => release.combine(root), assert.AssertionError);
        assert.equal(existsSync(join(root, 'latest.json')), false);
        part('windows-x86_64');
        release.combine(root);
        const manifest = JSON.parse(readFileSync(join(root, 'latest.json'), 'utf8'));
        assert.equal(Object.keys(manifest.platforms).length, 2);
        assert.equal(existsSync(join(root, 'update-darwin-aarch64.json')), false);
    } finally {
        rmSync(root, { recursive: true, force: true });
    }
});
