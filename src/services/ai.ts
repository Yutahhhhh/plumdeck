import { apiClient } from "./api-client";

export type AiProviderId = "claude" | "openai" | "gemini";

export interface AiProvider {
  id: AiProviderId;
  label: string;
  has_key: boolean;
  model: string;
  default_model: string;
}

export interface AiSettings {
  provider: AiProviderId | null;
  ready: boolean;
  providers: AiProvider[];
}

export interface AiGenreSuggestion {
  track_id: number;
  genre?: string | null;
  subgenre?: string | null;
  confidence?: "high" | "medium" | "low";
  reason?: string | null;
  error?: string | null;
}

/** Optional in-app AI: keys stay in the OS keychain and never come back. */
export const aiService = {
  getSettings: () => apiClient.get<AiSettings>("/ai/settings"),
  updateSettings: (update: { provider?: AiProviderId | null; models?: Partial<Record<AiProviderId, string>> }) =>
    apiClient.put<AiSettings>("/ai/settings", update),
  saveKey: (provider: AiProviderId, apiKey: string) =>
    apiClient.put<{ saved: boolean }>(`/ai/keys/${provider}`, { api_key: apiKey }),
  deleteKey: (provider: AiProviderId) => apiClient.delete(`/ai/keys/${provider}`),
  suggestGenres: (trackIds: number[]) =>
    apiClient.post<{ provider: string; model: string; items: AiGenreSuggestion[] }>(
      "/ai/genre-suggestions",
      { track_ids: trackIds },
      300_000
    ),
};
