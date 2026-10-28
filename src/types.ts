export interface Track {
  id: number;
  filepath: string;
  title: string;
  artist: string;
  album: string;
  bpm: number | null;
  key: string;
  genre: string;
  subgenre?: string;
  year?: number;
  duration: number;
  is_analyzed?: boolean | null;
  analysis_level?: "light" | "full" | null;
  energy: number;
  danceability: number;
  brightness: number;
  contrast: number;
  noisiness: number;
  loudness: number;
  is_genre_verified?: boolean;
  has_lyrics?: boolean;
}
