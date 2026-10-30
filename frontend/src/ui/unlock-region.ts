import data from './unlock-region-data.json';

const ISO_REGIONS: Readonly<Partial<Record<string, string>>> = data.iso3;
const FASTLY_REGIONS: Readonly<Partial<Record<string, string>>> = data.fastly;

/** Preserve the service's raw code in text; convert only the flag's ISO alpha-2 key. */
export function unlockFlagRegion(service: string, code: string): string | undefined {
  if (/^[A-Z]{2}$/.test(code)) return code;
  return service === 'reddit' ? FASTLY_REGIONS[code] : ISO_REGIONS[code];
}
