/** How long one audition plays before stopping by itself. */
export const AUDITION_SECONDS = 30;
const START_FRACTION = 0.3;

/** Where an audition starts: about 30% in, pulled earlier so a full audition still fits. */
export function auditionStart(duration: number | null | undefined): number {
  if (!duration || !Number.isFinite(duration) || duration <= 0) return 0;
  return Math.max(0, Math.min(duration * START_FRACTION, duration - AUDITION_SECONDS));
}

export interface AuditionTrack { id: number; filepath: string; duration: number }

/** What a card needs to show and drive the one audition that may play at a time. */
export interface AuditionControl {
  playingId: number | null;
  failedId: number | null;
  toggle: (track: AuditionTrack) => void;
}
