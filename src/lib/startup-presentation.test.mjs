import { test } from 'node:test';
import assert from 'node:assert/strict';
import { advanceStartupBar, formatElapsed, startupMessage, startupRatio } from './startup-presentation.ts';

test('known stages use friendly copy instead of service names', () => {
  assert.equal(startupMessage({ stage: 'services', label: 'ライブラリ・解析・再生の機能を読み込んでいます' }), 'ライブラリを準備しています…');
  assert.equal(startupMessage({ stage: 'launch', label: '解析サービスへの接続を確認しています' }), 'plumdeck を起動しています…');
});

test('unknown stages fall back to the reported label', () => {
  assert.equal(startupMessage({ stage: 'migrate', label: 'データを移行しています。' }), 'データを移行しています…');
  assert.equal(startupMessage({ stage: 'constructor', label: '' }), '準備しています…');
});

test('ratio is clamped and safe for a zero total', () => {
  assert.equal(startupRatio(3, 5), 0.6);
  assert.equal(startupRatio(9, 5), 1);
  assert.equal(startupRatio(1, 0), 0);
});

test('bar never moves backwards and snaps up to milestones', () => {
  assert.equal(advanceStartupBar(0.5, 1, 5, false), 0.5);
  assert.equal(advanceStartupBar(0.1, 3, 5, false), 0.6);
});

test('trickle eases toward the next milestone without reaching it', () => {
  let shown = 0.2;
  for (let i = 0; i < 500; i++) shown = advanceStartupBar(shown, 1, 5, true);
  assert.ok(shown > 0.3 && shown < 0.4);
  assert.equal(advanceStartupBar(1, 5, 5, true), 1);
});

test('elapsed time reads naturally', () => {
  assert.equal(formatElapsed(9), '9秒');
  assert.equal(formatElapsed(65), '1分05秒');
});
