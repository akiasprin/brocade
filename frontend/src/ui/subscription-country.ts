import { FLAG_SHEET } from './flags';

export const SUBSCRIPTION_COUNTRY_CODES = FLAG_SHEET.flatMap(line =>
  Array.from({ length: line.length / 2 }, (_, index) => line.slice(index * 2, index * 2 + 2).toUpperCase()),
);
export const SUBSCRIPTION_COUNTRY_CODE_SET = new Set(SUBSCRIPTION_COUNTRY_CODES);
const COUNTRY_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region' });

/** The compact regional-indicator prefix used in generated subscription node names. */
export function subscriptionFlag(code: string): string {
  return SUBSCRIPTION_COUNTRY_CODE_SET.has(code)
    ? Array.from(code, letter => String.fromCodePoint(127462 + letter.charCodeAt(0) - 65)).join('')
    : '';
}

export function subscriptionCountryLabel(code: string): string {
  const name = COUNTRY_NAMES.of(code);
  const flag = subscriptionFlag(code);
  return `${flag ? `${flag} ` : ''}${name && name !== code ? `${code} · ${name}` : code}`;
}
