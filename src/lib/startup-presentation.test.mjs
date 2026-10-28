import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  STARTUP_SLOW_HINT, advanceStartupBar, formatElapsed, startupDetailsText, startupMessage, startupPhrases, startupRatio,
} from './startup-presentation.ts';

test('known stages use friendly copy instead of service names', () => {
  assert.equal(startupMessage({ stage: 'services', label: 'ライブラリ・解析・再生の機能を読み込んでいます' }), '解析・再生エンジンを準備しています…');
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

test('copy does not suggest the music library is being scanned or is the cause of a slow start', () => {
  assert.doesNotMatch(startupMessage({ stage: 'services', label: '' }), /ライブラリ/);
  assert.doesNotMatch(STARTUP_SLOW_HINT, /ライブラリ|曲/);
});

test('phrases break after Japanese punctuation and before brackets, keeping spaces', () => {
  assert.deepEqual(startupPhrases('準備の途中で問題が発生しました。'), ['準備の途中で問題が発生しました。']);
  assert.deepEqual(startupPhrases('初回起動やアップデートの直後は、準備に少し時間がかかります。'),
    ['初回起動やアップデートの直後は、', '準備に少し時間がかかります。']);
  assert.deepEqual(startupPhrases('解析サービスが終了しました（終了コード: Some(1)）'),
    ['解析サービスが終了しました', '（終了コード:', ' ', 'Some(1)）']);
  assert.deepEqual(startupPhrases('接続を再試行中: Failed to fetch'),
    ['接続を再試行中:', ' ', 'Failed', ' ', 'to', ' ', 'fetch']);
  assert.deepEqual(startupPhrases('ライブラリ・解析・再生の機能を読み込んでいます'),
    ['ライブラリ・', '解析・', '再生の機能を読み込んでいます']);
});

test('details text lists the error first and includes the version', () => {
  const text = startupDetailsText({
    version: '1.2.3',
    progress: { stage: 'services', label: '機能を読み込んでいます', completed: 2, total: 5, error: ' 終了しました ' },
    elapsedSeconds: 65,
    connectionError: '接続を再試行中: Failed to fetch',
  });
  assert.equal(text, [
    'plumdeck v1.2.3',
    'エラー: 終了しました',
    '処理: 機能を読み込んでいます',
    '状態: services (2/5)',
    '経過時間: 1分05秒',
    '接続: 接続を再試行中: Failed to fetch',
  ].join('\n'));
  assert.doesNotMatch(startupDetailsText({ version: '1', progress: { stage: '', label: '', completed: 0, total: 5 }, elapsedSeconds: 0 }), /エラー|接続/);
});
