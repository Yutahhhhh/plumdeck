// Official DDJ-1000 selector NOTE 0x20 + index, MIDI channel 5.
export const BEAT_FX = [
  ["lowcutecho", "LOW CUT ECHO"], ["echo", "ECHO"], ["mtdelay", "MT DELAY"],
  ["spiral", "SPIRAL"], ["reverb", "REVERB"], ["tremolo", "TRANS"],
  ["enigmajet", "ENIGMA JET"], ["flanger", "FLANGER"], ["phaser", "PHASER"],
  ["pitchshift", "PITCH"], ["sliproll", "SLIP ROLL"], ["roll", "ROLL"],
  ["mobiussaw", "MOBIUS SAW"], ["mobiustri", "MOBIUS TRI"],
] as const;
export function beatFxMaxBeats(effect: string): number {
  if (["echo","lowcutecho","mtdelay","spiral","enigmajet","sliproll","roll","mobiussaw","mobiustri"].includes(effect)) return 2;
  return effect === "tremolo" ? 4 : 16;
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
