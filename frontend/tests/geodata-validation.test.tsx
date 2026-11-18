import { useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { GeoSelector, GeodataBrowser } from '../src/panes/geodata';
import { api, ApiError } from '../src/api/client';
import { geoCatalog, geodataFixture } from './fixtures/geodata-data';
import { rulesProblems } from '../src/rule-validation';
import type { Rule } from '../src/api';
beforeEach(() => {
  vi.stubGlobal('matchMedia', (media: string) => ({
    matches: false,
    media,
    addEventListener() {},
    removeEventListener() {},
  }));
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async (path: string, init?: RequestInit) =>
        geodataFixture(path, init) ?? Response.json({ error: 'missing fixture' }, { status: 404 }),
    ),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});
function mount(children: React.ReactNode) {
  return render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      {children}
    </QueryClientProvider>,
  );
}
it('only accepts existing selections and lets users pick an attribute', async () => {
  const changed = vi.fn();
  function Field() {
    const [values, setValues] = useState<string[]>([]);
    return (
      <GeoSelector
        kind="geosite"
        values={values}
        label="匹配内容"
        onChange={next => {
          setValues(next);
          changed(next);
        }}
      />
    );
  }
  const view = mount(<Field />);
  fireEvent.click(view.getByRole('combobox'));
  const menu = await view.findByRole('dialog', { name: 'geosite 选项' });
  const search = within(menu).getByRole('textbox', { name: '搜索 geosite' });
  fireEvent.change(search, { target: { value: 'netflx' } });
  fireEvent.keyDown(search, { key: 'Enter' });
  expect(changed).not.toHaveBeenCalled();
  fireEvent.change(search, { target: { value: 'google@' } });
  fireEvent.click(await within(menu).findByRole('option', { name: 'google@cn' }));
  expect(changed).toHaveBeenLastCalledWith(['google@cn']);
});
it('keeps unconfirmed old selections when the catalogue cannot be loaded', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ error: 'offline' }, { status: 503 })),
  );
  const view = mount(<GeoSelector kind="geosite" values={['netflix']} label="匹配内容" onChange={vi.fn()} />);
  expect(await view.findByText('未确认')).toBeTruthy();
  expect(view.getByRole('button', { name: '预览 netflix' })).toBeTruthy();
  fireEvent.click(view.getByRole('combobox'));
  expect(await view.findByText('规则集无法加载')).toBeTruthy();
  expect(view.queryAllByRole('option')).toHaveLength(0);
});
it('loads preview entries in bounded pages and checks a target', async () => {
  const view = mount(<GeodataBrowser kind="geosite" initial="google" onClose={vi.fn()} />);
  await waitFor(() => expect(view.getAllByText(/example-\d+\.com/)).toHaveLength(100));
  fireEvent.click(view.getByRole('button', { name: '下一页' }));
  await waitFor(() => expect(view.getAllByText(/example-\d+\.com/)).toHaveLength(2));
  fireEvent.change(view.getByRole('textbox', { name: '检查命中' }), { target: { value: 'example-0.com' } });
  fireEvent.click(view.getByRole('button', { name: '检查' }));
  expect(await view.findByText(/✓ 命中/)).toBeTruthy();
});
it('detects ordered duplicates and permits a deliberate narrower exception first', () => {
  const rule = (value: string): Rule => ({ m: { t: 'domain_suffix', v: [value] }, a: { t: 'block' } });
  expect(rulesProblems([rule('openai.com'), rule('api.openai.com')], {}).has(1)).toBe(true);
  expect(rulesProblems([rule('api.openai.com'), rule('openai.com')], {}).size).toBe(0);
  const initial: Rule[] = [{ m: { t: 'geosite', v: ['netflix'] }, a: { t: 'block' } }];
  expect(rulesProblems(initial, {}, initial).size).toBe(0);
  expect(rulesProblems([{ m: { t: 'geosite', v: ['openai'] }, a: { t: 'block' } }], {}, initial).size).toBe(1);
  expect(
    rulesProblems([{ m: { t: 'geosite', v: ['google@jp'] }, a: { t: 'block' } }], { geosite: geoCatalog('geosite') })
      .size,
  ).toBe(1);
});
it('retains structured 422 locations for the editor to focus', async () => {
  const issues = [{ location: 'video/stream/hk', rule: 1, field: 'm', value: 'bad', message: '不存在' }];
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ error: '不存在', issues }, { status: 422 })),
  );
  await expect(api('/model/apply')).rejects.toMatchObject({ status: 422, issues });
  try {
    await api('/model/apply');
  } catch (error) {
    expect(error).toBeInstanceOf(ApiError);
  }
});
