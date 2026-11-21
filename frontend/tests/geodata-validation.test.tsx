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
function maskLookupDomain() {
  const original = vi.mocked(fetch).getMockImplementation()!;
  vi.mocked(fetch).mockImplementation(async (input, init) => {
    const response = await original(input, init);
    if (new URL(String(input), 'http://fixture').pathname !== '/routing/geodata/lookup') return response;
    return Response.json({ ...(await response.json()), domain: '***.com' });
  });
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
  const search = within(menu).getByRole('combobox', { name: '搜索 geosite' });
  fireEvent.change(search, { target: { value: 'netflx' } });
  fireEvent.keyDown(search, { key: 'Enter' });
  expect(changed).not.toHaveBeenCalled();
  fireEvent.change(search, { target: { value: 'google@' } });
  fireEvent.click(await within(menu).findByRole('option', { name: 'google@cn' }));
  expect(changed).toHaveBeenLastCalledWith(['google@cn']);
});
function GeoField({
  initial = [],
  blocked = [],
  onChange,
}: {
  initial?: string[];
  blocked?: string[];
  onChange?: (v: string[]) => void;
}) {
  const [values, setValues] = useState<string[]>(initial);
  return (
    <GeoSelector
      kind="geosite"
      values={values}
      label="匹配内容"
      blockedValues={blocked}
      onChange={next => {
        setValues(next);
        onChange?.(next);
      }}
    />
  );
}
async function openMenu(view: ReturnType<typeof mount>) {
  fireEvent.click(view.getByRole('combobox', { name: '匹配内容' }));
  const menu = await view.findByRole('dialog', { name: 'geosite 选项' });
  await within(menu).findByRole('listbox', { name: 'geosite' });
  return within(menu);
}
it('opens on the curated choices and browses topic groups with folded regional variants', async () => {
  const changed = vi.fn();
  const view = mount(<GeoField onChange={changed} />);
  const menu = await openMenu(view);
  expect(view.getByRole('combobox', { name: '匹配内容' }).getAttribute('aria-controls')).toBe(
    menu.getByRole('listbox', { name: 'geosite' }).id,
  );
  expect(menu.getByRole('button', { name: /^常用/, pressed: true })).toBeTruthy();
  expect(menu.getAllByRole('option').map(o => o.getAttribute('aria-label'))).toEqual([
    'category-ads-all',
    'openai',
    'anthropic',
    'google',
    'google@ads',
    'google@cn',
    'netflix',
  ]);
  fireEvent.click(menu.getByRole('button', { name: /^AI/ }));
  expect(menu.getByText('category-ai')).toBeTruthy();
  fireEvent.click(menu.getByRole('option', { name: 'category-ai-cn' }));
  expect(changed).toHaveBeenLastCalledWith(['category-ai-cn']);
  expect(menu.getByRole('button', { name: '移除 category-ai-cn' })).toBeTruthy();
  fireEvent.click(menu.getByRole('option', { name: 'category-ai-cn' }));
  expect(changed).toHaveBeenLastCalledWith([]);
});
it('ranks exact names first, reports totals per group and filters by group', async () => {
  const view = mount(<GeoField />);
  const menu = await openMenu(view);
  fireEvent.change(menu.getByRole('combobox', { name: '搜索 geosite' }), { target: { value: 'cn' } });
  const options = () => menu.getAllByRole('option').map(o => o.getAttribute('aria-label'));
  expect(options()[0]).toBe('cn');
  expect(options()).toContain('geolocation-!cn');
  expect(menu.getByRole('button', { name: /^全部结果/, pressed: true }).textContent).toContain('4');
  fireEvent.click(menu.getByRole('button', { name: /^AI/ }));
  expect(options()).toEqual(['category-ai-!cn', 'category-ai-cn']);
  expect(menu.getByText(/另有 1 个分类带/)).toBeTruthy();
});
it('looks up a domain, marks the narrowest category and adds it', async () => {
  const changed = vi.fn();
  const view = mount(<GeoField onChange={changed} />);
  const menu = await openMenu(view);
  fireEvent.change(menu.getByRole('combobox', { name: '搜索 geosite' }), { target: { value: 'chat.openai.com' } });
  expect(await menu.findByText('包含 chat.openai.com 的分类')).toBeTruthy();
  expect(menu.getByText('最精确')).toBeTruthy();
  expect(menu.getByText('范围很宽')).toBeTruthy();
  expect(menu.getAllByText('命中 domain:openai.com')).toHaveLength(3);
  fireEvent.click(menu.getByRole('option', { name: 'openai' }));
  expect(changed).toHaveBeenLastCalledWith(['openai']);
});
it('disables values the rule or an earlier rule already covers', async () => {
  const changed = vi.fn();
  const view = mount(<GeoField initial={['category-ai-!cn']} blocked={['google']} onChange={changed} />);
  const menu = await openMenu(view);
  const search = menu.getByRole('combobox', { name: '搜索 geosite' });
  fireEvent.change(search, { target: { value: 'openai' } });
  const openai = menu.getByRole('option', { name: 'openai' }) as HTMLButtonElement;
  expect(openai.disabled).toBe(true);
  expect(openai.title).toBe('本条已包含');
  fireEvent.keyDown(search, { key: 'Enter' });
  fireEvent.change(search, { target: { value: 'google@' } });
  expect((menu.getByRole('option', { name: 'google@cn' }) as HTMLButtonElement).disabled).toBe(true);
  expect(changed).not.toHaveBeenCalled();
});
it('uses masked lookup results and clears the previous domain while a new lookup is pending', async () => {
  maskLookupDomain();
  const view = mount(<GeoField />);
  const menu = await openMenu(view);
  const search = menu.getByRole('combobox', { name: '搜索 geosite' });
  fireEvent.change(search, { target: { value: 'chat.openai.com' } });
  expect(await menu.findByText('包含 chat.openai.com 的分类')).toBeTruthy();
  expect(menu.getByRole('option', { name: 'openai' })).toBeTruthy();
  fireEvent.change(search, { target: { value: 'example.com' } });
  expect(menu.queryByRole('option', { name: 'openai' })).toBeNull();
  expect(menu.getByText('正在反查 example.com…')).toBeTruthy();
  expect(await menu.findByText('没有分类包含 example.com')).toBeTruthy();
});
it('moves through options with the arrow keys and adds the highlighted one on Enter', async () => {
  const changed = vi.fn();
  const view = mount(<GeoField onChange={changed} />);
  const menu = await openMenu(view);
  const search = menu.getByRole('combobox', { name: '搜索 geosite' });
  fireEvent.change(search, { target: { value: 'goo' } });
  fireEvent.keyDown(search, { key: 'Enter' });
  expect(changed).not.toHaveBeenCalled();
  fireEvent.keyDown(search, { key: 'ArrowDown' });
  expect(search.getAttribute('aria-activedescendant')).toBe(
    menu.getByRole('option', { name: 'google@ads' }).getAttribute('id'),
  );
  fireEvent.keyDown(search, { key: 'Enter' });
  expect(changed).toHaveBeenLastCalledWith(['google@ads']);
});
it.each(['geosite', 'geoip'] as const)(
  'keeps %s selections without a status line while the catalogue loads',
  async kind => {
    let finish!: (response: Response) => void;
    vi.stubGlobal(
      'fetch',
      vi.fn(
        () =>
          new Promise<Response>(resolve => {
            finish = resolve;
          }),
      ),
    );
    const value = kind === 'geosite' ? 'netflix' : 'cn';
    const view = mount(<GeoSelector kind={kind} values={[value]} label="匹配内容" onChange={vi.fn()} />);
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
    expect(view.getByRole('button', { name: `预览 ${value}` })).toBeTruthy();
    expect(view.container.querySelector('.geo-message')).toBeNull();
    expect(view.container.querySelector('.geo-select')?.getAttribute('aria-busy')).toBe('true');
    finish(Response.json(geoCatalog(kind)));
    await waitFor(() => expect(view.container.querySelector('.geo-select')?.getAttribute('aria-busy')).toBe('false'));
    expect(view.container.querySelector('.geo-message')).toBeNull();
    expect(view.getByRole('button', { name: `预览 ${value}` })).toBeTruthy();
  },
);
it('edits through the original field on narrow screens and expands attributes in place', async () => {
  vi.stubGlobal('matchMedia', (media: string) => ({
    matches: media.includes('max-width'),
    media,
    addEventListener() {},
    removeEventListener() {},
  }));
  const changed = vi.fn();
  const view = mount(<GeoField initial={['netflix', 'cn']} onChange={changed} />);
  const field = view.getByRole('combobox', { name: '匹配内容' });
  field.focus();
  const menu = await openMenu(view);
  expect(menu.queryByRole('combobox')).toBeNull();
  expect(document.activeElement).toBe(field);
  expect(document.body.style.overflow).toBe('');
  fireEvent.change(field, { target: { value: 'google' } });
  expect(menu.queryByRole('option', { name: 'google@ads' })).toBeNull();
  fireEvent.click(menu.getByRole('button', { name: 'google 的 2 个属性' }));
  fireEvent.click(menu.getByRole('option', { name: 'google@ads' }));
  expect(changed).toHaveBeenLastCalledWith(['netflix', 'cn', 'google@ads']);
  expect((field as HTMLInputElement).value).toBe('google');
  fireEvent.change(field, { target: { value: 'goo' } });
  fireEvent.keyDown(field, { key: 'ArrowDown' });
  expect(field.getAttribute('aria-activedescendant')).toBeTruthy();
  fireEvent.click(view.getByRole('button', { name: '完成 geosite 选择' }));
  expect(view.queryByRole('dialog', { name: 'geosite 选项' })).toBeNull();
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
it('previews a category in kind sections and narrows it by attribute and kind', async () => {
  const view = mount(<GeodataBrowser kind="geosite" initial="google" onClose={vi.fn()} />);
  const dialog = within(await view.findByRole('dialog', { name: '规则集预览' }));
  expect(await dialog.findByText('example-0.com')).toBeTruthy();
  expect(dialog.getByRole('heading', { name: 'google' })).toBeTruthy();
  const sections = view.baseElement.querySelectorAll('.geo-browser-section');
  expect([...sections].map(s => s.getAttribute('aria-label'))).toEqual(['后缀', '完整', '正则']);
  expect(view.baseElement.querySelectorAll('.geo-browser-entry')).toHaveLength(102);
  expect(dialog.getByText('共 102 条')).toBeTruthy();
  // 属性和类型都带条目数，并且可以组合筛选。
  fireEvent.click(dialog.getByRole('button', { name: '@ads 12 条' }));
  expect(await dialog.findByRole('heading', { name: 'google@ads' })).toBeTruthy();
  await waitFor(() => expect(view.baseElement.querySelectorAll('.geo-browser-entry')).toHaveLength(12));
  fireEvent.click(dialog.getByRole('button', { name: '@cn 30 条' }));
  fireEvent.click(await dialog.findByRole('button', { name: '后缀 30 条' }));
  await waitFor(() => expect(view.baseElement.querySelectorAll('.geo-browser-entry')).toHaveLength(30));
  expect(dialog.getByText('example-50.com')).toBeTruthy();
  // 包含关系可跳转：openai 包含于 category-ai-!cn，后者列出被它完整包含的分类。
  fireEvent.change(dialog.getByRole('textbox', { name: '搜索分类' }), { target: { value: 'openai' } });
  fireEvent.click(dialog.getByRole('button', { name: 'openai 6 条' }));
  fireEvent.click(await dialog.findByRole('button', { name: 'category-ai-!cn' }));
  fireEvent.click(await dialog.findByRole('button', { name: '3 个分类' }));
  expect(dialog.getByRole('button', { name: /huggingface/ })).toBeTruthy();
  expect(dialog.getByRole('button', { name: '1 处' })).toBeTruthy();
});
it('checks a domain, names the matching entry and offers the categories that contain a miss', async () => {
  const view = mount(<GeodataBrowser kind="geosite" initial="google" onClose={vi.fn()} />);
  const dialog = within(await view.findByRole('dialog', { name: '规则集预览' }));
  const find = await dialog.findByRole('textbox', { name: '筛选条目或检查命中' });
  fireEvent.change(find, { target: { value: 'img.example-3.com' } });
  const verdict = await dialog.findByRole('status');
  await waitFor(() =>
    expect(verdict.textContent).toMatch(/命中\s*img\.example-3\.com\s*由\s*后缀\s*example-3\.com\s*匹配/),
  );
  // 文本筛选不到条目时列出命中的条目本身，并高亮。
  expect(view.baseElement.querySelector('.geo-browser-entry.hit')?.textContent).toBe('example-3.com');
  fireEvent.change(find, { target: { value: 'chat.openai.com' } });
  await waitFor(() => expect(dialog.getByRole('status').textContent).toMatch(/未命中/));
  // 替代分类在结论行里；左栏同名的行会清空输入，这里要点结论行的那一个。
  fireEvent.click(await within(dialog.getByRole('status')).findByRole('button', { name: 'openai 6 条' }));
  expect(await dialog.findByRole('heading', { name: 'openai' })).toBeTruthy();
  expect((dialog.getByRole('textbox', { name: '筛选条目或检查命中' }) as HTMLInputElement).value).toBe(
    'chat.openai.com',
  );
});
it('loads long categories page by page up to a bounded size', async () => {
  const view = mount(<GeodataBrowser kind="geosite" initial="geolocation-!cn" onClose={vi.fn()} />);
  const dialog = within(await view.findByRole('dialog', { name: '规则集预览' }));
  expect(await dialog.findByText('已显示 200 / 450 条')).toBeTruthy();
  fireEvent.click(dialog.getByRole('button', { name: '继续加载' }));
  expect(await dialog.findByText('已显示 400 / 450 条')).toBeTruthy();
  fireEvent.click(dialog.getByRole('button', { name: '继续加载' }));
  expect(await dialog.findByText('共 450 条')).toBeTruthy();
  const pages = vi.mocked(fetch).mock.calls.map(([path]) => new URL(String(path), 'http://fixture'));
  expect(
    pages.filter(url => url.pathname.endsWith('/geolocation-!cn')).map(url => url.searchParams.get('limit')),
  ).toEqual(['200', '200', '200']);
});
it('offers alternative categories when the lookup response domain is masked', async () => {
  maskLookupDomain();
  const view = mount(<GeodataBrowser kind="geosite" initial="google" onClose={vi.fn()} />);
  const dialog = within(await view.findByRole('dialog', { name: '规则集预览' }));
  fireEvent.change(await dialog.findByRole('textbox', { name: '筛选条目或检查命中' }), {
    target: { value: 'chat.openai.com' },
  });
  const verdict = within(await dialog.findByRole('status'));
  fireEvent.click(await verdict.findByRole('button', { name: 'openai 6 条' }));
  expect(await dialog.findByRole('heading', { name: 'openai' })).toBeTruthy();
});
it('adds, re-scopes and removes the previewed category in its source row', async () => {
  const changed = vi.fn();
  function Field({ initial }: { initial: string[] }) {
    const [values, setValues] = useState(initial);
    return (
      <GeoSelector
        kind="geosite"
        values={values}
        label="匹配内容"
        scope="D1"
        onChange={next => {
          setValues(next);
          changed(next);
        }}
      />
    );
  }
  const view = mount(<Field initial={['google', 'category-ai-!cn']} />);
  fireEvent.click(await view.findByRole('button', { name: '预览 google' }));
  const dialog = within(await view.findByRole('dialog', { name: '规则集预览' }));
  expect(await dialog.findByText('已在 D1')).toBeTruthy();
  fireEvent.click(dialog.getByRole('button', { name: '@cn 30 条' }));
  fireEvent.click(dialog.getByRole('button', { name: '把 google 改为 google@cn' }));
  expect(changed).toHaveBeenLastCalledWith(['google@cn', 'category-ai-!cn']);
  expect(await dialog.findByText('已在 D1')).toBeTruthy();
  // 被同一行更宽的分类包含时说明原因，不能加入。
  fireEvent.change(dialog.getByRole('textbox', { name: '搜索分类' }), { target: { value: 'openai' } });
  fireEvent.click(dialog.getByRole('button', { name: 'openai 6 条' }));
  expect(await dialog.findByText('D1 已包含')).toBeTruthy();
  expect((dialog.getByRole('button', { name: '加入 D1' }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(dialog.getByRole('textbox', { name: '搜索分类' }), { target: { value: 'netflix' } });
  fireEvent.click(dialog.getByRole('button', { name: 'netflix 26 条' }));
  fireEvent.click(await dialog.findByRole('button', { name: '加入 D1' }));
  expect(changed).toHaveBeenLastCalledWith(['google@cn', 'category-ai-!cn', 'netflix']);
  fireEvent.click(await dialog.findByRole('button', { name: '移除' }));
  expect(changed).toHaveBeenLastCalledWith(['google@cn', 'category-ai-!cn']);
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
