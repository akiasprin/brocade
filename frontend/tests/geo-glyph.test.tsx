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
  ['geolocation-!cn', 'location', false],
  ['tld-cn', 'tld', false],
  ['cn', 'tld', false],
] as const)('maps %s to its semantic collection icon', (code, glyph, theme) => {
  const view = render(<GeoGlyph kind="geosite" code={code} />);
  const icon = view.container.querySelector('.geo-collection-icon');
  expect(icon?.getAttribute('data-geo-glyph')).toBe(glyph);
  expect(icon?.classList.contains('theme')).toBe(theme);
  expect(icon?.getAttribute('aria-hidden')).toBe('true');
});

it('uses direct AI lettering and a tall document for unmapped services', () => {
  const ai = render(<GeoGlyph kind="geosite" code="category-ai" />);
  expect(ai.container.querySelector('.geo-ai-text')?.textContent).toBe('AI');
  ai.unmount();
  const unknown = render(<GeoGlyph kind="geosite" code="future-service" />);
  expect(unknown.container.querySelector('[data-geo-glyph="document"] path')?.getAttribute('d')).toContain('v11.5H5');
});

it('retains brand artwork for known services', () => {
  for (const code of ['openai', 'cloudflare', 'amazon', 'huggingface']) {
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
