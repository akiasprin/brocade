import { api } from './client';

export type GeositeChecks = Record<string, { hit: boolean | null; reason?: string }>;

export async function matchGeosite(domain: string, sets: string[], signal?: AbortSignal): Promise<GeositeChecks> {
  const matches: GeositeChecks = {};
  for (let at = 0; at < sets.length; at += 128) {
    const query = new URLSearchParams({ domain, sets: JSON.stringify(sets.slice(at, at + 128)) });
    const result = await api<{ matches: GeositeChecks }>(`/routing/geosite?${query}`, '', { signal });
    Object.assign(matches, result.matches);
  }
  return matches;
}
