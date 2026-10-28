// Compile the host DSP against the pinned Mixxx headers, then render buffers
// in memory. No host process, audio device or user music is opened.
import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const host = path.resolve(import.meta.dirname, '..');
const upstream = process.env.PLUMDECK_MIXXX_SOURCE || path.join(host, 'upstream');

test('BEAT FX render dry audio, long/fractional delays and insert PITCH correctly', { timeout: 60000 }, () => {
  assert(existsSync(path.join(upstream, 'src/effects/backends/effectprocessor.h')),
    'Set PLUMDECK_MIXXX_SOURCE to the pinned Mixxx source checkout');
  const temp = mkdtempSync(path.join(tmpdir(), 'plumdeck-beat-dsp-'));
  try {
    const qt = execFileSync('pkg-config', ['--cflags', '--libs', 'Qt6Core', 'Qt6Gui', 'Qt6Widgets', 'Qt6Xml', 'Qt6Sql', 'Qt6Network', 'Qt6Qml', 'Qt6OpenGL'], { encoding: 'utf8' }).trim().split(/\s+/);
    const executable = path.join(temp, 'beat-dsp-offline');
    const compiler = process.env.CXX || 'c++';
    const args = ['-std=c++20', '-O2', '-DNDEBUG',
      '-I' + path.join(upstream, 'src'),
      '-I' + path.join(upstream, 'lib'),
      '-I' + path.join(upstream, 'lib/reverb'),
      '-I' + path.join(upstream, 'lib/rigtorp/SPSCQueue/include'),
      '-I' + path.join(host, 'build-deps/gsl/include'),
      ...(process.env.PLUMDECK_GSL_INCLUDE ? ['-I' + process.env.PLUMDECK_GSL_INCLUDE] : []),
      ...qt, path.join(import.meta.dirname, 'beat-dsp-offline.cpp'), '-o', executable];
    execFileSync(compiler, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    const output = execFileSync(executable, [], { encoding: 'utf8' });
    assert.match(output, /PASS: dry preservation/);
    console.log(output.trim());
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});
