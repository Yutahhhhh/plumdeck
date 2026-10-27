type DeckIdentity = { slot: number; loaded: boolean; title: string | null; artist: string | null };

type ResolvedDeckCollection = { decks: Array<{ slot: number }> };

/** Tempo, pitch, key display and native signatures can change without loading a song. */
export function observedDeckIdentity(decks: readonly DeckIdentity[]): string {
  return JSON.stringify([...decks].sort((a, b) => a.slot - b.slot).map(deck => [
    deck.slot, deck.loaded,
    deck.loaded ? deck.title?.normalize("NFC").trim() ?? "" : "",
    deck.loaded ? deck.artist?.normalize("NFC").trim() ?? "" : "",
  ]));
}

/** Keeps a successful match visible during same-song verification. */
export class DeckResolutionCache<T> {
  private identity: string | null = null;
  private identities = new Map<number, string>();
  value: T | null = null;

  observe(decks: readonly DeckIdentity[]): string {
    const identity = observedDeckIdentity(decks);
    if (identity !== this.identity && this.value !== null) {
      // A deck change should not make an unchanged source deck look unresolved.
      // Keep the last confirmed resolution for stable slots while the changed
      // slot is being checked again.
      const nextIdentities = new Map(decks.map(deck => [deck.slot, deckIdentity(deck)]));
      const collection = this.value as T & ResolvedDeckCollection;
      const retained = collection.decks.filter(deck => nextIdentities.get(deck.slot) === this.identities.get(deck.slot));
      this.value = retained.length ? { ...collection, decks: retained } as T : null;
      this.identities = nextIdentities;
    } else if (identity !== this.identity) {
      this.identities = new Map(decks.map(deck => [deck.slot, deckIdentity(deck)]));
    }
    this.identity = identity;
    return identity;
  }

  publish(value: T): void { this.value = value; }
  clear(): void { this.identity = null; this.identities.clear(); this.value = null; }
}

function deckIdentity(deck: DeckIdentity): string {
  return JSON.stringify([
    deck.loaded,
    deck.loaded ? deck.title?.normalize("NFC").trim() ?? "" : "",
    deck.loaded ? deck.artist?.normalize("NFC").trim() ?? "" : "",
  ]);
}
