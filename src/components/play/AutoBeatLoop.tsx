import { cn } from "@/lib/utils";
import { formatBeats } from "./loop-math";

export type LoopMode = "auto" | "manual";

type Props = {
  deckId: string;
  /** Size shown on AUTO: the active loop's length, otherwise the chosen size. */
  beats: number;
  mode: LoopMode;
  loopActive: boolean;
  hasLoop: boolean;
  /** Manual IN waiting for OUT. */
  pendingIn: boolean;
  disabledReason?: string;
  canHalve: boolean;
  canDouble: boolean;
  onMode: (mode: LoopMode) => void;
  /** ‹ / ›: halve or double; while looping the IN point is kept. */
  onStep: (direction: -1 | 1) => void;
  /** AUTO: start a new loop of `beats` at the playhead, or exit the active loop. */
  onAuto: () => void;
  onLoopIn: () => void;
  onLoopOut: () => void;
  /** Exit the active loop, or go back to its IN and loop again. */
  onReloop: () => void;
};

/**
 * AUTO BEAT LOOP block: size (AUTO) / ‹ › / AU・MA. MA shows IN / OUT /
 * RELOOP-EXIT like a player's manual loop section.
 */
export function AutoBeatLoop(props: Props) {
  const { disabledReason: reason } = props;
  const off = Boolean(reason);
  const auto = props.mode === "auto";
  const reloopReason = reason ?? (props.hasLoop ? undefined : "ループがありません");
  const outReason = reason ?? (props.pendingIn ? undefined : "先に IN を押してループの開始点を決めてください");

  return <div className="dj-abl" role="group" aria-label={`Deck ${props.deckId} オートビートループ`}>
    {auto
      ? <button type="button" className={cn("dj-abl-value", "dj-num", props.loopActive && "is-on")} disabled={off}
          title={reason ?? (props.loopActive ? "ループを抜けます" : `${formatBeats(props.beats)} 拍のループを現在位置から張ります（Q ON なら拍に合わせます）`)}
          aria-pressed={props.loopActive} onClick={props.onAuto}>
          {formatBeats(props.beats)}
        </button>
      : <div className="dj-abl-manual">
          <button type="button" className={cn(props.pendingIn && "is-armed")} disabled={off} title={reason ?? "ループイン点を現在位置に設定（Q ON なら拍に合わせます）"} onClick={props.onLoopIn}>IN</button>
          <button type="button" disabled={Boolean(outReason)} title={outReason ?? "ループアウト点を現在位置に設定してループ開始"} onClick={props.onLoopOut}>OUT</button>
          <button type="button" className={cn("dj-abl-reloop", props.loopActive && "is-on")} disabled={Boolean(reloopReason)}
            title={reloopReason ?? (props.loopActive ? "EXIT：ループを抜けます" : "RELOOP：ループの開始点へ戻ってループします")} onClick={props.onReloop}>{props.loopActive ? "EXIT" : "RELOOP"}</button>
        </div>}

    <div className="dj-abl-step">
      <button type="button" disabled={off || !props.canHalve} aria-label="ループを半分の長さにする" title={reason ?? (props.loopActive ? "1/2X：開始点を保ったまま半分の長さにします" : "ループの拍数を半分にします")}
        onClick={() => props.onStep(-1)}>‹</button>
      <button type="button" disabled={off || !props.canDouble} aria-label="ループを倍の長さにする" title={reason ?? (props.loopActive ? "2X：開始点を保ったまま倍の長さにします" : "ループの拍数を倍にします")}
        onClick={() => props.onStep(1)}>›</button>
    </div>

    <div className="dj-abl-mode" role="group" aria-label="ループモード">
      <button type="button" className={cn(auto && "is-on")} aria-pressed={auto} disabled={off}
        title={reason ?? "オートビートループとマニュアルループを切り換えます"} onClick={() => props.onMode("auto")}>AU</button>
      <button type="button" className={cn(!auto && "is-on")} aria-pressed={!auto} disabled={off}
        title={reason ?? "オートビートループとマニュアルループを切り換えます"} onClick={() => props.onMode("manual")}>MA</button>
    </div>
  </div>;
}
