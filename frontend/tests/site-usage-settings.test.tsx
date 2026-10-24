import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import {
  fetchSiteUsagePolicy,
  saveSiteUsagePolicy,
  fetchOtherDomainPolicy,
  saveOtherDomainPolicy,
  fetchSniffingFailedPolicy,
  saveSniffingFailedPolicy,
} from '../src/api';
import { SiteUsageSettingsCard } from '../src/site-usage-settings';

vi.mock('../src/api', async original => ({
  ...(await original<typeof import('../src/api')>()),
  fetchSiteUsagePolicy: vi.fn(),
  saveSiteUsagePolicy: vi.fn(),
  fetchOtherDomainPolicy: vi.fn(),
  saveOtherDomainPolicy: vi.fn(),
  fetchSniffingFailedPolicy: vi.fn(),
  saveSniffingFailedPolicy: vi.fn(),
}));
const fetchPolicy = vi.mocked(fetchSiteUsagePolicy);
const savePolicy = vi.mocked(saveSiteUsagePolicy);
const domainPolicy = {
  enabled: false,
  top_n: 128,
  max_domains_per_user: 1024,
  node_capacity: 4096,
  report_interval_secs: 30,
};
const targetPolicy = {
  enabled: false,
  top_n: 128,
  max_targets_per_user: 1024,
  node_capacity: 4096,
  report_interval_secs: 30,
};
const classification = () => within(screen.getByRole('region', { name: '站点分类' }));
const card = (editable = true) =>
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <SiteUsageSettingsCard editable={editable} />
    </QueryClientProvider>,
  );
beforeEach(() => {
  fetchPolicy.mockReset().mockResolvedValue({ enabled: false });
  savePolicy.mockReset().mockResolvedValue({ enabled: true });
  vi.mocked(fetchOtherDomainPolicy).mockReset().mockResolvedValue(domainPolicy);
  vi.mocked(saveOtherDomainPolicy)
    .mockReset()
    .mockImplementation(async value => value);
  vi.mocked(fetchSniffingFailedPolicy).mockReset().mockResolvedValue(targetPolicy);
  vi.mocked(saveSniffingFailedPolicy)
    .mockReset()
    .mockImplementation(async value => value);
});
afterEach(cleanup);

it('starts off and saves a single fleet-wide policy', async () => {
  card();
  const on = classification().getByRole('button', { name: '开启' });
  await waitFor(() => expect(on.hasAttribute('disabled')).toBe(false));
  expect(classification().getByRole('button', { name: '关闭' }).getAttribute('aria-pressed')).toBe('true');
  fireEvent.click(on);
  await classification().findByText('已保存，待节点同步。');
  expect(savePolicy).toHaveBeenCalledExactlyOnceWith(true, expect.anything());
  expect(on.getAttribute('aria-pressed')).toBe('true');
  expect(saveOtherDomainPolicy).not.toHaveBeenCalled();
  expect(saveSniffingFailedPolicy).not.toHaveBeenCalled();
});

it('disables controls for read-only admins', async () => {
  card(false);
  await screen.findByText(/仅系统管理员/);
  expect(screen.getAllByRole('button', { name: '开启' }).every(button => button.hasAttribute('disabled'))).toBe(true);
  fireEvent.click(classification().getByRole('button', { name: '开启' }));
  expect(savePolicy).not.toHaveBeenCalled();
});

it('prevents repeat submits and does not pretend a failed save succeeded', async () => {
  let reject!: (error: Error) => void;
  savePolicy.mockImplementation(
    () =>
      new Promise((_, fail) => {
        reject = fail;
      }),
  );
  card();
  const on = classification().getByRole('button', { name: '开启' });
  await waitFor(() => expect(on.hasAttribute('disabled')).toBe(false));
  fireEvent.click(on);
  await screen.findByText('正在保存…');
  expect(on.hasAttribute('disabled')).toBe(true);
  reject(new Error('policy unavailable'));
  await screen.findByText('policy unavailable');
  expect(on.getAttribute('aria-pressed')).toBe('false');
});

it('does not permit changes when the policy could not be read', async () => {
  fetchPolicy.mockRejectedValue(new Error('cannot read policy'));
  card();
  await screen.findByText('cannot read policy');
  expect(classification().getByRole('button', { name: '开启' }).hasAttribute('disabled')).toBe(true);
});

it('groups three independent policies in one settings panel', async () => {
  const view = card();
  await screen.findByLabelText('每用户目标容量');
  const panel = within(screen.getByRole('region', { name: '全局出流量统计' }));
  expect(panel.getByRole('heading', { name: '出流量统计' })).toBeTruthy();
  expect(view.container.querySelectorAll('.panel.config-panel')).toHaveLength(1);
  expect(panel.getByText(/容量满时优先淘汰已可靠保存的冷条目/)).toBeTruthy();
  expect(panel.getByText(/默认保留 24 小时，可在“数据保留”中分别调整/)).toBeTruthy();
  for (const name of ['站点分类', '未分类域名', '嗅探失败目标']) {
    const section = within(screen.getByRole('region', { name }));
    expect(section.getByRole('heading', { name })).toBeTruthy();
    expect(section.getByRole('button', { name: '关闭' }).getAttribute('aria-pressed')).toBe('true');
  }
});

it('marks only changed fields and removes the save bar when changes are reverted', async () => {
  card();
  const domains = within(screen.getByRole('region', { name: '未分类域名' }));
  const targets = within(screen.getByRole('region', { name: '嗅探失败目标' }));
  const top = await domains.findByLabelText('每用户 Top N');
  const targetTop = await targets.findByLabelText('每用户 Top N');
  expect(top.id).not.toBe(targetTop.id);
  expect(domains.queryByRole('button', { name: '保存未分类域名设置' })).toBeNull();

  fireEvent.change(top, { target: { value: '64' } });
  expect(top.classList.contains('chg')).toBe(true);
  expect(domains.getByLabelText('每用户域名容量').classList.contains('chg')).toBe(false);
  expect(targetTop.classList.contains('chg')).toBe(false);
  expect(domains.getByText('有未保存的改动')).toBeTruthy();
  fireEvent.change(top, { target: { value: '128' } });
  expect(top.classList.contains('chg')).toBe(false);
  expect(domains.queryByRole('button', { name: '保存未分类域名设置' })).toBeNull();

  fireEvent.click(domains.getByRole('button', { name: '开启' }));
  expect(domains.getByRole('group', { name: '未分类域名统计' }).classList.contains('chg')).toBe(true);
  fireEvent.click(domains.getByRole('button', { name: '关闭' }));
  expect(domains.getByRole('group', { name: '未分类域名统计' }).classList.contains('chg')).toBe(false);
  expect(domains.queryByRole('button', { name: '保存未分类域名设置' })).toBeNull();
  expect(saveOtherDomainPolicy).not.toHaveBeenCalled();
});

it('saving one detail policy preserves the other draft and supports retry after failure', async () => {
  card();
  const domains = within(screen.getByRole('region', { name: '未分类域名' }));
  const targets = within(screen.getByRole('region', { name: '嗅探失败目标' }));
  await domains.findByLabelText('每用户域名容量');
  await targets.findByLabelText('每用户目标容量');
  fireEvent.click(domains.getByRole('button', { name: '开启' }));
  fireEvent.change(domains.getByLabelText('每用户 Top N'), { target: { value: '64' } });
  fireEvent.click(targets.getByRole('button', { name: '开启' }));
  fireEvent.change(targets.getByLabelText('每用户 Top N'), { target: { value: '32' } });
  fireEvent.click(domains.getByRole('button', { name: '保存未分类域名设置' }));
  await domains.findByText('已保存，待节点同步。');
  expect(saveOtherDomainPolicy).toHaveBeenCalledExactlyOnceWith({ ...domainPolicy, enabled: true, top_n: 64 });
  expect(saveSniffingFailedPolicy).not.toHaveBeenCalled();
  expect(savePolicy).not.toHaveBeenCalled();
  expect((targets.getByLabelText('每用户 Top N') as HTMLInputElement).value).toBe('32');

  vi.mocked(saveSniffingFailedPolicy).mockRejectedValueOnce(new Error('目标策略保存失败'));
  fireEvent.click(targets.getByRole('button', { name: '保存嗅探失败目标设置' }));
  await targets.findByText('目标策略保存失败');
  expect(targets.queryByText('已保存，待节点同步。')).toBeNull();
  expect((targets.getByLabelText('每用户 Top N') as HTMLInputElement).value).toBe('32');
  fireEvent.click(targets.getByRole('button', { name: '保存嗅探失败目标设置' }));
  await targets.findByText('已保存，待节点同步。');
  expect(saveSniffingFailedPolicy).toHaveBeenLastCalledWith({ ...targetPolicy, enabled: true, top_n: 32 });
});

it('isolates a failed policy read from the other controls', async () => {
  vi.mocked(fetchOtherDomainPolicy).mockRejectedValue(new Error('域名策略读取失败'));
  card();
  await screen.findByText('域名策略读取失败');
  expect(
    within(screen.getByRole('region', { name: '未分类域名' }))
      .getByRole('button', { name: '开启' })
      .hasAttribute('disabled'),
  ).toBe(true);
  await waitFor(() =>
    expect(classification().getByRole('button', { name: '开启' }).hasAttribute('disabled')).toBe(false),
  );
  const targets = within(screen.getByRole('region', { name: '嗅探失败目标' }));
  await targets.findByLabelText('每用户目标容量');
  expect(targets.getByRole('button', { name: '开启' }).hasAttribute('disabled')).toBe(false);
});
