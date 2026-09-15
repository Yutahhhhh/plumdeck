import { useSyncExternalStore } from 'react';
import { shareMusicsStore } from '@/services/junction/share-musics/coordinator';
export function useShareMusics() { return useSyncExternalStore(shareMusicsStore.subscribe, shareMusicsStore.get); }
