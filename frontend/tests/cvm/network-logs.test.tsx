import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { NetworkLogsButton, NetworkLogsDialog } from '../../src/panes/cvm/network-logs';
import { networkLogs } from '../fixtures/cvm/log-data';
let client: QueryClient;
afterEach(() => {
  cleanup();
  client?.clear();
  vi.unstubAllGlobals();
});
function show(child: React.ReactNode) {
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{child}</QueryClientProvider>);
}
it('switches both machines, filters process logs, and exposes other network history explicitly', async () => {
  const calls: string[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async input => {
      calls.push(String(input));
      return Response.json(networkLogs(new URL(String(input), 'http://local').searchParams.get('nodes')!));
    }),
  );
  show(
    <NetworkLogsDialog
      title="入口网络日志"
      onClose={() => {}}
      machines={[
        { node_id: 'remote-ingress', name: '香港入口', role: '入口' },
        { node_id: 'home', name: '家中开发宿主', role: '计算' },
      ]}
      resources={['entry-tunnel-fixture']}
    />,
  );
  expect(await screen.findByText('Phantun server started; listening on port 44000')).toBeTruthy();
  expect(screen.queryByText('另一条出口链路暂不可达，正在重连')).toBeNull();
  fireEvent.change(screen.getByRole('combobox', { name: '日志来源' }), { target: { value: 'phantun' } });
  expect(screen.queryByText(/收到入口配置：版本/)).toBeNull();
  expect(screen.getByText(/authentication=\[已隐藏\]/)).toBeTruthy();
  fireEvent.change(screen.getByRole('combobox', { name: '日志来源' }), { target: { value: 'xray' } });
  fireEvent.click(screen.getByRole('checkbox', { name: '包含本机其他网络记录' }));
  expect(await screen.findByText('另一条出口链路暂不可达，正在重连')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: /计算.*家中开发宿主/ }));
  await waitFor(() => expect(calls.some(url => url.includes('nodes=home'))).toBe(true));
});
it('shows legacy and stale reports as missing or historical, with no healthy assertion', async () => {
  const data = networkLogs();
  data.nodes[0].diagnostics = null;
  data.nodes[0].fresh = false;
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json(data)),
  );
  show(<NetworkLogsDialog title="出口日志" onClose={() => {}} machines={[{ node_id: 'remote-ingress' }]} />);
  expect(await screen.findByText(/尚未回报网络日志/)).toBeTruthy();
  expect(screen.queryByRole('list', { name: '网络执行日志' })).toBeNull();
});
it('labels expired data and bounds the visible history', async () => {
  const data = networkLogs();
  data.nodes[0].fresh = false;
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json(data)),
  );
  show(<NetworkLogsDialog title="裸机网络配置日志" onClose={() => {}} machines={[{ node_id: 'remote-ingress' }]} />);
  expect(await screen.findByText(/回报已过期/)).toBeTruthy();
  expect(screen.getByText(/已有 9 条较早记录被轮换/)).toBeTruthy();
});
it('keeps card navigation from opening behind the log dialog and hides unauthorized controls', async () => {
  const parent = vi.fn();
  const fetch = vi.fn(async () => Response.json(networkLogs()));
  vi.stubGlobal('fetch', fetch);
  const view = show(
    <div onClick={parent}>
      <NetworkLogsButton title="容器网络日志" machines={[{ node_id: 'remote-ingress' }]} allowed={true} />
    </div>,
  );
  fireEvent.click(screen.getByRole('button', { name: '查看容器网络日志' }));
  expect(parent).not.toHaveBeenCalled();
  expect(await screen.findByRole('dialog', { name: '容器网络日志' })).toBeTruthy();
  view.unmount();
  show(<NetworkLogsButton machines={[{ node_id: 'remote-ingress' }]} allowed={false} />);
  expect(screen.queryByRole('button')).toBeNull();
});
it('shows API failures without claiming logs are empty', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ error: '没有查看日志的权限' }, { status: 403 })),
  );
  show(<NetworkLogsDialog title="出口日志" onClose={() => {}} machines={[{ node_id: 'remote-ingress' }]} />);
  expect(await screen.findByText(/当前账号不能查看这项内容/)).toBeTruthy();
  expect(screen.queryByText(/当前筛选下没有日志/)).toBeNull();
});
