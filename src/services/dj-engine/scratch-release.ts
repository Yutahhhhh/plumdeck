/**
 * 波形スクラッチで手を離した瞬間に、バックスピンかスクラッチかを決める。
 *
 * 判定はポインター履歴だけで同期的に行い、離してから待つことはしない。
 * スクラッチ側は判定のために一瞬でも再生復帰を遅らせてはならないため。
 * 時間は performance.now() と同じ軸（ms）、位置はジェスチャー開始からの
 * 変位（ms、負 = 逆回転）。速度は通常再生に対する倍率で扱う。
 */
export type MotionSample = { time: number; positionMs: number };
export type ReleaseDecision = { kind: "scratch" } | { kind: "backspin"; velocity: number };

export const BACKSPIN_DETECTION = {
  /** 通常速度の何倍以上の逆回転で離したらバックスピン候補にするか。 */
  minReverseSpeed: 2,
  /** 最後の動きから離すまでがこれより長ければ、手は止まっていた。 */
  maxIdleMs: 25,
  /** 同じ向きに動かし続けた時間。スクラッチの短い引きを除く。 */
  minStrokeMs: 50,
  /** 離す瞬間の速度がその引きの最高速度にこれだけ近いこと。折り返し直前の減速を除く。 */
  minPeakRatio: 0.7,
  /** 速度を測る区間。単発のイベント間隔の揺れを均す。 */
  velocityWindowMs: 30,
  /** エンジンのスクラッチ権限（±16倍）を超える速度は出せない。 */
  maxSpeed: 16,
} as const;

const HISTORY_MS = 500;
const HISTORY_LIMIT = 128;

/** ジェスチャー中のポインター位置を、判定に要る直近分だけ持つ。 */
export class MotionHistory {
  readonly samples: MotionSample[] = [];

  push(time: number, positionMs: number): void {
    const last = this.samples[this.samples.length - 1];
    if (last && time < last.time) time = last.time;
    this.samples.push({ time, positionMs });
    const horizon = time - HISTORY_MS;
    let drop = 0;
    while (drop < this.samples.length - 2 && (this.samples[drop].time < horizon || this.samples.length - drop > HISTORY_LIMIT)) drop++;
    if (drop) this.samples.splice(0, drop);
  }
}

/** index の時点までの直近区間の速度。区間が取れなければ null。 */
function velocityAt(samples: MotionSample[], index: number, windowMs: number): number | null {
  const end = samples[index];
  let start = index;
  while (start > 0 && end.time - samples[start].time < windowMs) start--;
  const dt = end.time - samples[start].time;
  return dt > 0 ? (end.positionMs - samples[start].positionMs) / dt : null;
}

export function classifyRelease(samples: readonly MotionSample[], releasedAt: number, playing: boolean): ReleaseDecision {
  const scratch: ReleaseDecision = { kind: "scratch" };
  // 停止中は離した位置で止める。頭出しの素早いドラッグを滑らせない。
  if (!playing || samples.length < 2) return scratch;
  const list = samples as MotionSample[];
  const lastIndex = list.length - 1;
  const last = list[lastIndex];
  if (releasedAt - last.time > BACKSPIN_DETECTION.maxIdleMs) return scratch;
  const velocity = velocityAt(list, lastIndex, BACKSPIN_DETECTION.velocityWindowMs);
  if (velocity === null || velocity > -BACKSPIN_DETECTION.minReverseSpeed) return scratch;

  let strokeStart = lastIndex;
  while (strokeStart > 0 && list[strokeStart].positionMs - list[strokeStart - 1].positionMs < 0) strokeStart--;
  if (last.time - list[strokeStart].time < BACKSPIN_DETECTION.minStrokeMs) return scratch;

  let peak = 0;
  for (let index = strokeStart + 1; index <= lastIndex; index++) {
    const speed = velocityAt(list, index, BACKSPIN_DETECTION.velocityWindowMs);
    if (speed !== null) peak = Math.min(peak, speed);
  }
  if (velocity > peak * BACKSPIN_DETECTION.minPeakRatio) return scratch;
  return { kind: "backspin", velocity: Math.max(-BACKSPIN_DETECTION.maxSpeed, velocity) };
}
