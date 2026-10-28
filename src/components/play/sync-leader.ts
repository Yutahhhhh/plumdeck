/**
 * Which deck BEAT SYNC follows. Pure module: runs under `node --test`.
 */
type DeckLike = { track: unknown; status: string; syncLeader: string | null; syncEnabled: boolean } | undefined;

/**
 * The MASTER deck first; otherwise a playing deck, then any loaded deck.
 * Never an empty deck: following silence would stall the tempo.
 */
export function chooseSyncLeader<Id extends string>(self: Id, decks: readonly Id[], states: Partial<Record<Id, DeckLike>>): Id | undefined {
  const others = decks.filter((deck) => deck !== self && states[deck]?.track);
  return others.find((deck) => states[deck]?.syncLeader === deck)
    ?? others.find((deck) => states[deck]?.status === "playing")
    ?? others[0];
}
