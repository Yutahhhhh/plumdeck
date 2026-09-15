/**
 * 手を離したバックスピンを、指の代わりに惰性で回し続ける。
 *
 * エンジンのスクラッチは指の変位を追うだけで惰性を持たないので、減速する
 * 変位を move として送り続け、十分に減速した時点で end を送る。end を
 * 受けたエンジンは即座に通常再生（停止中なら停止）へ着地する。ゆっくり
 * 立ち上げると同じ着地を使うスクラッチまで遅く感じるため、ここでは行わない。
 *
 * 減速は dv/dt = −v/τ − μ·sign(v)。τ は勢いの抜け方（慣性）、μ は常に
 * 掛かるブレーキ（抵抗感）で、どちらも JOG の重さで決まる。
 */
export const BACKSPIN_MOTION = {
  /** LIGHT → HEAVY の時定数（秒）。 */
  tauLight: 0.7, tauHeavy: 0.12,
  /** LIGHT → HEAVY の摩擦（通常速度比 / 秒）。 */
  frictionLight: 0.5, frictionHeavy: 6,
  /** この速度まで落ちたら着地して通常再生へ戻す。 */
  landSpeed: 0.5,
  tickMs: 10,
  /** タイマーが間引かれて時間が飛んだら（ウィンドウ非表示など）、その場で着地する。 */
  maxGapMs: 200,
  /** プロトコルが受け付けるジェスチャー内の変位。 */
  limitMs: 60_000,
} as const;

export function backspinBrake(weight: number): { tau: number; friction: number } {
  const w = Number.isFinite(weight) ? Math.max(0, Math.min(1, weight)) : 0.5;
  return {
    tau: BACKSPIN_MOTION.tauLight + (BACKSPIN_MOTION.tauHeavy - BACKSPIN_MOTION.tauLight) * w,
    friction: BACKSPIN_MOTION.frictionLight + (BACKSPIN_MOTION.frictionHeavy - BACKSPIN_MOTION.frictionLight) * w,
  };
}

/** 速度 speed（絶対値）から dt 秒後の速度。解析解なので刻み幅に依らない。 */
function brake(speed: number, dt: number, tau: number, friction: number): number {
  const floor = friction * tau;
  return Math.max(0, (speed + floor) * Math.exp(-dt / tau) - floor);
}

type Timers = {
  now: () => number;
  every: (callback: () => void, ms: number) => unknown;
  cancel: (handle: unknown) => void;
};
const browserTimers: Timers = {
  now: () => performance.now(),
  every: (callback, ms) => setInterval(callback, ms),
  cancel: (handle) => clearInterval(handle as ReturnType<typeof setInterval>),
};

export type BackspinOptions = {
  /** 離した瞬間の速度（通常速度比、負 = 逆回転）。 */
  velocity: number;
  /** 0 = LIGHT（長く回る）、1 = HEAVY（すぐ止まる）。 */
  weight: number;
  /** 離した時点のジェスチャー内の変位（ms）。 */
  positionMs: number;
  move: (positionMs: number, at: number) => void;
  land: (positionMs: number) => Promise<unknown> | void;
  timers?: Timers;
};

export class Backspin {
  private velocity: number;
  private position: number;
  private previous: number;
  private handle: unknown;
  private running = true;
  private readonly tau: number;
  private readonly friction: number;
  private readonly timers: Timers;
  private readonly options: BackspinOptions;

  constructor(options: BackspinOptions) {
    this.options = options;
    this.timers = options.timers ?? browserTimers;
    const { tau, friction } = backspinBrake(options.weight);
    this.tau = tau;
    this.friction = friction;
    this.velocity = options.velocity;
    this.position = options.positionMs;
    this.previous = this.timers.now();
    this.handle = this.timers.every(() => this.tick(), BACKSPIN_MOTION.tickMs);
    if (Math.abs(this.velocity) <= BACKSPIN_MOTION.landSpeed) this.stop();
  }

  get active(): boolean { return this.running; }
  get positionMs(): number { return this.position; }

  /** 回転を止めて同じジェスチャーを手で掴み直す。end は送らない。 */
  grab(): number {
    this.halt();
    return this.position;
  }

  /** その場で着地させる。着地コマンドの完了を返す。 */
  stop(): Promise<unknown> | void {
    if (!this.running) return;
    this.halt();
    return this.options.land(this.position);
  }

  private halt(): void {
    if (!this.running) return;
    this.running = false;
    this.timers.cancel(this.handle);
  }

  private tick(): void {
    if (!this.running) return;
    const now = this.timers.now();
    const gap = now - this.previous;
    this.previous = now;
    if (gap > BACKSPIN_MOTION.maxGapMs) { this.stop(); return; }
    if (gap <= 0) return;
    const speed = brake(Math.abs(this.velocity), gap / 1000, this.tau, this.friction);
    const next = Math.sign(this.velocity) * speed;
    // 台形則。速度は解析解なので、位置の誤差は 1 刻みあたりごく小さい。
    const position = this.position + (this.velocity + next) / 2 * gap;
    this.velocity = next;
    this.position = Math.max(-BACKSPIN_MOTION.limitMs, Math.min(BACKSPIN_MOTION.limitMs, position));
    if (speed <= BACKSPIN_MOTION.landSpeed || Math.abs(this.position) >= BACKSPIN_MOTION.limitMs) { this.stop(); return; }
    this.options.move(this.position, now);
  }
}
