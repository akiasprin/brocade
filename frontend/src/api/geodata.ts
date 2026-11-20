import { api } from './client';
import type { ModelOp } from '../draft';
export type GeodataKind = 'geosite' | 'geoip';
export type GeoCategory = {
  code: string;
  count: number;
  attrs: string[];
  /** Entries per attribute (geosite) or address family (geoip). */
  attr_counts?: Record<string, number>;
  /** Other geosite categories holding all of this one's entries, narrowest first. */
  within?: string[];
};
export type GeoCatalog = {
  source: string;
  loaded_at: number;
  entries: GeoCategory[];
  /** Curated common geosite choices present in this file, in product order. */
  featured?: string[];
  references?: { selector: string; location: string; rule: number }[];
};
export type GeoLookupHit = { code: string; count: number; kind: string; value: string };
/** Categories of the configured geosite source that contain a domain, narrowest first. */
export type GeoLookup = { domain: string; hits: GeoLookupHit[]; unknown: string[] };
export type GeoEntry = { kind: string; value: string; attrs: string[] };
export type GeoPage = {
  code: string;
  total: number;
  counts: Record<string, number>;
  entries: GeoEntry[];
  offset: number;
  next_offset: number | null;
  hit: boolean | null;
  /** The first entries matching the target (at most 8); empty when an inverted geoip turns them into a miss. */
  hits: GeoEntry[];
};
export type RuleIssue = { location: string; rule: number | null; field: string; value: string; message: string };
export const fetchGeoCatalog = (kind: GeodataKind, signal?: AbortSignal, source?: string) =>
  api<GeoCatalog>(`/routing/geodata/catalog?${new URLSearchParams({ kind, ...(source ? { source } : {}) })}`, '', {
    signal,
  });
export const fetchGeoPage = (kind: GeodataKind, code: string, query: Record<string, string>, signal?: AbortSignal) =>
  api<GeoPage>(`/routing/geodata/${kind}/${encodeURIComponent(code)}?${new URLSearchParams(query)}`, '', { signal });
export const lookupGeosite = (domain: string, signal?: AbortSignal) =>
  api<GeoLookup>(`/routing/geodata/lookup?${new URLSearchParams({ domain })}`, '', { signal });
export const validateDraft = (ops: ModelOp[]) =>
  api<{ valid: boolean }>('/model/validate', '', { method: 'POST', body: JSON.stringify({ ops }) });
export type GeoRelations = { covers: { wider: string; narrower: string }[]; errors: Record<string, string> };
export const fetchGeoRelations = (kind: GeodataKind, values: string[], signal?: AbortSignal) =>
  api<GeoRelations>('/routing/geodata/relations', '', {
    method: 'POST',
    signal,
    body: JSON.stringify({ kind, values }),
  });

export function geoValueIssue(catalog: GeoCatalog | undefined, value: string): string | null {
  if (!catalog) return null;
  const [rawCode, ...attrs] = value.toLowerCase().split('@');
  const code = rawCode.replace(/^!+/, '');
  const entry = catalog.entries.find(e => e.code.toLowerCase() === code);
  if (!entry) return `${code} 不存在`;
  const missing = attrs.find(attr => !entry.attrs.includes(attr));
  return missing ? `${code} 无 @${missing}` : null;
}

export function geoSuggestions(values: string[], query: string): string[] {
  const target = query.toLowerCase();
  if (target.length > 128) return [];
  const distance = (value: string) => {
    let row = Array.from({ length: target.length + 1 }, (_, i) => i);
    for (let i = 0; i < value.length; i++) {
      const next = [i + 1];
      for (let j = 0; j < target.length; j++)
        next.push(Math.min(next[j] + 1, row[j + 1] + 1, row[j] + Number(value[i] !== target[j])));
      row = next;
    }
    return row[target.length];
  };
  return values
    .map(value => ({ value, score: distance(value.toLowerCase()) }))
    .filter(v => v.score <= Math.max(2, Math.floor(target.length / 3)))
    .sort((a, b) => a.score - b.score || a.value.localeCompare(b.value))
    .slice(0, 3)
    .map(v => v.value);
}
