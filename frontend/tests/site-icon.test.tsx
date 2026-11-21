import { readFileSync } from 'node:fs';
import { URL as FileURL } from 'node:url';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { hasSiteBrand, SiteIcon } from '../src/ui/site-icon';

const catalog = JSON.parse(
  readFileSync(new FileURL('../../components/xray-core/common/siteusage/catalog.json', import.meta.url), 'utf8'),
) as { sites: { id: string }[] };
const generic = new Set(['other', 'sniffing-failed', 'unidentified']);
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it('bundles a local, decorative brand outline for every known site', () => {
  const fetch = vi.spyOn(globalThis, 'fetch');
  const brands = catalog.sites.filter(site => !generic.has(site.id));
  const view = render(
    <>
      {brands.map(site => (
        <SiteIcon key={site.id} siteId={site.id} />
      ))}
    </>,
  );
  for (const { id } of brands) {
    const icon = view.container.querySelector(`[data-site-icon="${id}"]`);
    expect(icon?.getAttribute('aria-hidden')).toBe('true');
    expect(icon?.querySelector(':scope > svg > path')?.getAttribute('d')?.length).toBeGreaterThan(0);
    expect(icon?.querySelector('svg')?.getAttribute('fill')).toBe('currentColor');
    expect(icon?.querySelector('svg')?.getAttribute('focusable')).toBe('false');
  }
  expect(view.container.querySelectorAll('img, image, use, script')).toHaveLength(0);
  expect(fetch).not.toHaveBeenCalled();
});

it('uses local generic icons for unclassified traffic and unknown categories', () => {
  for (const id of [...generic, 'future-category', 'constructor', '__proto__']) {
    const view = render(<SiteIcon siteId={id} />);
    expect(view.container.querySelector('.site-brand-icon .ui-icon svg')).not.toBeNull();
    view.unmount();
  }
});

it('renders transparent pirate Hugging Face artwork with thickened outlines', () => {
  const view = render(<SiteIcon siteId="huggingface" />);
  const icon = view.container.querySelector('[data-site-icon="huggingface"]');
  expect(icon?.querySelectorAll('svg > path')).toHaveLength(9);
  const hands = icon?.querySelectorAll('svg > path[fill="none"][stroke="currentColor"]');
  expect(hands).toHaveLength(2);
  for (const hand of hands ?? []) {
    expect(hand.getAttribute('stroke-width')).toBe('42');
  }
  const fills = icon?.querySelectorAll('svg > path:not([fill="none"])');
  expect(fills).toHaveLength(7);
  for (const fill of fills ?? []) {
    expect(fill.getAttribute('stroke')).toBe('currentColor');
    expect(fill.getAttribute('stroke-width')).toBe('8');
  }
  expect(icon?.querySelector('svg > path')?.getAttribute('d')).toContain('A208.00 206.75');
  expect(icon?.querySelector('[fill="white"], [fill="black"], [fill="#FF323D"]')).toBeNull();
  expect(icon?.querySelector('text, img, image, use, script')).toBeNull();
});

it('renders monochrome official Google G, expanded geosite brands, and exact brand aliases only', () => {
  const google = render(<SiteIcon siteId="google" />);
  const googleSvg = google.container.querySelector('[data-site-icon="google"] > svg');
  expect(googleSvg?.getAttribute('fill')).toBe('currentColor');
  expect(googleSvg?.getAttribute('preserveAspectRatio')).toBe('xMidYMid meet');
  expect(googleSvg?.querySelectorAll('path')).toHaveLength(1);
  expect(googleSvg?.querySelector('path')?.getAttribute('d')).toContain('M22.56 12.25');
  google.unmount();

  const dmm = render(<SiteIcon siteId="dmm" />);
  expect(dmm.container.querySelector('[data-site-icon="dmm"] path')?.getAttribute('fill-rule')).toBe('evenodd');
  dmm.unmount();

  for (const id of ['abema', 'alphabet', 'xiaomi', 'bytedance', 'amap', 'iqiyi', 'lark', 'wps', 'youku']) {
    expect(hasSiteBrand(id)).toBe(true);
    const view = render(<SiteIcon siteId={id} />);
    const icon = view.container.querySelector(`[data-site-icon="${id}"]`);
    expect(icon?.querySelectorAll(':scope > svg > path').length).toBeGreaterThan(0);
    expect(icon?.querySelector('.ui-icon')).toBeNull();
    view.unmount();
  }

  for (const alias of [
    'xiaomi-ai',
    'xiaomi-iot',
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
    expect(hasSiteBrand(alias)).toBe(true);
    const view = render(<SiteIcon siteId={alias} />);
    expect(view.container.querySelector(`[data-site-icon="${alias}"] > svg > path`)).not.toBeNull();
    view.unmount();
  }

  expect(hasSiteBrand('google-unknown')).toBe(false);
  expect(hasSiteBrand('xiaomi-unknown')).toBe(false);
});

it.each([
  ['dazn', 1],
  ['hbomax', 6],
] as const)('renders the complete %s vector without font or network assets', (id, shapes) => {
  const fetch = vi.spyOn(globalThis, 'fetch');
  const view = render(<SiteIcon siteId={id} />);
  const icon = view.container.querySelector(`[data-site-icon="${id}"]`);
  expect(icon?.querySelectorAll('svg > path')).toHaveLength(shapes);
  expect(icon?.querySelectorAll('text, img, image, use, script, .ui-icon')).toHaveLength(0);
  expect(icon?.querySelector('svg')?.getAttribute('fill')).toBe('currentColor');
  expect(fetch).not.toHaveBeenCalled();
});
