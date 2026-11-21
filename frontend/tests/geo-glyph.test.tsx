import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { GeoGlyph } from '../src/panes/geo-glyph';

afterEach(cleanup);

it.each([
  ['category-ai-chat-!cn', 'ai', true],
  ['category-entertainment', 'media', true],
  ['category-ads-all', 'ads', true],
  ['category-dev', 'dev', true],
  ['category-games', 'games', true],
  ['category-finance', 'finance', true],
  ['category-communication', 'communication', true],
  ['category-ecommerce', 'ecommerce', true],
  ['category-companies', 'category', true],
  ['geolocation-cn', 'tld', false],
  ['geolocation-!cn', 'tld', false],
  ['tld-cn', 'tld', false],
  ['cn', 'tld', false],
  ['private', 'private', true],
] as const)('maps %s to its semantic collection icon', (code, glyph, theme) => {
  const view = render(<GeoGlyph kind="geosite" code={code} />);
  const icon = view.container.querySelector('.geo-collection-icon');
  expect(icon?.getAttribute('data-geo-glyph')).toBe(glyph);
  expect(icon?.classList.contains('theme')).toBe(theme);
  expect(icon?.getAttribute('aria-hidden')).toBe('true');
});

it('uses direct AI lettering and uppercase 2-3 character initials for unmapped services', () => {
  const ai = render(<GeoGlyph kind="geosite" code="category-ai" />);
  expect(ai.container.querySelector('.geo-ai-text')?.textContent).toBe('AI');
  ai.unmount();
  for (const [code, text] of [
    ['future-service', 'FUT'],
    ['verisign', 'VER'],
    ['12306', '123'],
    ['x-y', 'XY'],
  ] as const) {
    const unknown = render(<GeoGlyph kind="geosite" code={code} />);
    const icon = unknown.container.querySelector('[data-geo-glyph="initials"]');
    expect(icon?.classList.contains('initials')).toBe(true);
    expect(icon?.classList.contains('theme')).toBe(false);
    expect(icon?.getAttribute('data-geo-initials')).toBe(text);
    expect(icon?.textContent).toBe('');
    unknown.unmount();
  }
});

it('retains brand artwork for known services and exact geosite aliases', () => {
  for (const code of [
    'openai',
    'cloudflare',
    'amazon',
    'huggingface',
    'google',
    'alphabet',
    'dmm',
    'abema',
    'xiaomi',
    'xiaomi-ai',
    'xiaomi-iot',
    'bytedance',
    'bytedance-ai-!cn',
    'x',
    'apple-cn',
    'google-cn',
    'bilibili2',
    'bilibili-cdn',
    'cloudflare-cn',
    'aliyun',
    'ookla-speedtest',
  ]) {
    const view = render(<GeoGlyph kind="geosite" code={code} />);
    expect(view.container.querySelector(`[data-site-icon="${code}"] > svg`)).not.toBeNull();
    expect(view.container.querySelector('.geo-collection-icon')).toBeNull();
    view.unmount();
  }
});

it('keeps region flags and gives special GeoIP ranges solid collection icons', () => {
  const region = render(<GeoGlyph kind="geoip" code="!cn" />);
  expect(region.container.querySelector('.geo-flag')).not.toBeNull();
  region.unmount();
  const privateIp = render(<GeoGlyph kind="geoip" code="private" />);
  const privateIcon = privateIp.container.querySelector('[data-geo-glyph="private"]');
  expect(privateIcon).not.toBeNull();
  expect(privateIcon?.classList.contains('theme')).toBe(true);
  privateIp.unmount();
  const other = render(<GeoGlyph kind="geoip" code="future-range" />);
  const otherIcon = other.container.querySelector('[data-geo-glyph="ip"]');
  expect(otherIcon?.classList.contains('theme')).toBe(true);
  expect(other.container.querySelector('.geo-ip-text')?.textContent).toBe('IP');
});
