import type { AssistIntent } from "@/services/assist";

/** The floor moves Assist and Play RECOMMEND both offer. */
export const PRESETS: { value: AssistIntent; label: string; detail: string }[] = [
  { value: "keep", label: "キープ", detail: "流れを保つ" },
  { value: "hype", label: "盛り上げる", detail: "熱量を上げる" },
  { value: "dance", label: "踊らせる", detail: "グルーヴへ" },
  { value: "calm", label: "落ち着かせる", detail: "クールダウン" },
  { value: "emotional", label: "エモく", detail: "切なく・深く" },
  { value: "bright", label: "明るく", detail: "開放感へ" },
  { value: "throwback", label: "時代を戻す", detail: "懐かしい曲" },
  { value: "wordplay", label: "ワードプレイ", detail: "言葉でつなぐ" },
];

export const presetLabel = (value: AssistIntent) => PRESETS.find(item => item.value === value)?.label ?? value;
