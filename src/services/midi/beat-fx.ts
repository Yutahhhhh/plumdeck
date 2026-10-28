// Official DDJ-1000 selector NOTE 0x20 + index, MIDI channel 5.
export const BEAT_FX = [
  ["lowcutecho", "LOW CUT ECHO"], ["echo", "ECHO"], ["mtdelay", "MT DELAY"],
  ["spiral", "SPIRAL"], ["reverb", "REVERB"], ["tremolo", "TRANS"],
  ["enigmajet", "ENIGMA JET"], ["flanger", "FLANGER"], ["phaser", "PHASER"],
  ["pitchshift", "PITCH"], ["sliproll", "SLIP ROLL"], ["roll", "ROLL"],
  ["mobiussaw", "MOBIUS SAW"], ["mobiustri", "MOBIUS TRI"],
] as const;
export const BEAT_FX_RANGE_CAPABILITY = "mixer.beatfx.range.v2";
const EXTENDED_BEAT_FX = ["echo", "lowcutecho", "mtdelay", "spiral", "enigmajet", "sliproll", "roll", "mobiussaw", "mobiustri"];
/** Preserve older hosts' range until the processor expansion is advertised. */
export function beatFxMaxBeats(effect: string, capabilities: readonly string[] = []): number {
  const extended = capabilities.includes(BEAT_FX_RANGE_CAPABILITY);
  if (EXTENDED_BEAT_FX.includes(effect)) return extended ? 16 : 2;
  if (effect === "tremolo" || extended && effect === "phaser") return 4;
  return 16;
}
export function beatFxMinBeats(effect: string, capabilities: readonly string[] = []): number {
  if (!capabilities.includes(BEAT_FX_RANGE_CAPABILITY)) return 1 / 8;
  if (EXTENDED_BEAT_FX.includes(effect)) return 1 / 16;
  return effect === "flanger" || effect === "phaser" ? 1 / 4 : 1 / 8;
}
/** REVERB and PITCH have no beat parameter, so their beat selector is hidden. */
export function beatFxHasBeats(effect: string): boolean {
  return effect !== "reverb" && effect !== "pitchshift";
}
/**
 * Effects offered on screen. MOBIUS is hidden: it adds an oscillator that
 * sounds even with a stopped deck. Indices of BEAT_FX stay the controller's.
 */
export const BEAT_FX_MENU = BEAT_FX.filter(([effect]) => !effect.startsWith("mobius"));
