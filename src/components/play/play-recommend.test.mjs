import test from 'node:test';
import assert from 'node:assert/strict';
import { RECOMMEND_MAX, RECOMMEND_PAGE, cautionsOf, historyBadge, nextRecommendLimit, parseRecommendSettings, recommendExcludeIds, scorePercent, DEFAULT_RECOMMEND_SETTINGS } from './play-recommend.ts';

test('more pages grow the limit up to the engine maximum', () => {
  assert.equal(nextRecommendLimit(RECOMMEND_PAGE), RECOMMEND_PAGE * 2);
  assert.equal(nextRecommendLimit(45), RECOMMEND_MAX);
  assert.equal(nextRecommendLimit(RECOMMEND_MAX), RECOMMEND_MAX);
});

test('played tracks are excluded only when enabled, without the seed', () => {
  const played = new Set([9, 3, 5]);
  assert.deepEqual(recommendExcludeIds(played, true, 5), [3, 9]);
  assert.deepEqual(recommendExcludeIds(played, false, 5), []);
  assert.deepEqual(recommendExcludeIds(undefined, true, null), []);
});

test('scores are whole percents and history prefers the direct follow', () => {
  assert.equal(scorePercent(0.876), 88);
  assert.equal(scorePercent(1.4), 100);
  assert.equal(scorePercent(null), null);
  assert.equal(historyBadge({ followed: 2, nearby: 5, plays: 7, days_since: 3, affinity: 0.9 }), '次に2回');
  assert.equal(historyBadge({ followed: 0, nearby: 4, plays: 7, days_since: null, affinity: 0.4 }), '近くで4回');
  assert.equal(historyBadge({ followed: 0, nearby: 0, plays: 7, days_since: null, affinity: 0 }), null);
  assert.equal(historyBadge(null), null);
  assert.deepEqual(cautionsOf({ reasons: [{ kind: 'key', tone: 'caution', text: 'x' }, { kind: 'bpm', tone: 'good', text: 'y' }] }).map(r => r.text), ['x']);
});

test('remembered settings fall back field by field', () => {
  assert.deepEqual(parseRecommendSettings(null), DEFAULT_RECOMMEND_SETTINGS);
  assert.deepEqual(parseRecommendSettings('not json'), DEFAULT_RECOMMEND_SETTINGS);
  assert.deepEqual(parseRecommendSettings('[]'), DEFAULT_RECOMMEND_SETTINGS);
  assert.deepEqual(parseRecommendSettings('{"intent":"hype","transition":true,"genreScope":"bogus","excludePlayed":false}'),
    { intent: 'hype', transition: true, genreScope: 'any', excludePlayed: false });
});
