import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { EntryPublications } from '../../src/panes/cvm/entry-publications';
import { cvmWorkspace } from '../fixtures/cvm/data';
import { nicCatalog } from '../fixtures/cvm/network-data';
import { entryBoard } from '../fixtures/cvm/entry-data';
let client: QueryClient;
afterEach(() => {
  cleanup();
  client?.clear();
  vi.unstubAllGlobals();
});
it('publishes host IP L3 forwarding and makes cross-machine transport optional', async () => {
  const workspace = cvmWorkspace('board');
  const writes: Record<string, unknown>[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input, init) => {
      if (init?.method && init.method !== 'GET') {
        writes.push(JSON.parse(String(init.body)));
        return Response.json({});
      }
      return Response.json(String(input) === '/cvm/networks' ? nicCatalog() : entryBoard(workspace, 'board'));
    }),
  );
  client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <EntryPublications workspace={workspace} />
    </QueryClientProvider>,
  );
  fireEvent.click(await screen.findByRole('button', { name: '添加入口地址' }));
  fireEvent.change(screen.getByRole('textbox', { name: '名称' }), { target: { value: 'Host L3' } });
  fireEvent.change(screen.getByRole('combobox', { name: '方式' }), { target: { value: 'dedicated' } });
  expect(screen.getByRole('option', { name: '独占地址' })).toBeTruthy();
  expect(screen.getByText(/SSH 等服务端口应填入保留列表/)).toBeTruthy();
  expect(screen.getByText('跨机器传输')).toBeTruthy();
  fireEvent.click(screen.getByRole('checkbox', { name: '允许转发到其他机器' }));
  expect(screen.queryByText('跨机器传输')).toBeNull();
  fireEvent.change(screen.getByRole('textbox', { name: '保留 TCP 端口' }), { target: { value: '8000-7000' } });
  fireEvent.click(screen.getByRole('button', { name: '保存变更' }));
  expect(writes).toHaveLength(0);
  expect((screen.getByRole('textbox', { name: '保留 TCP 端口' }) as HTMLInputElement).disabled).toBe(false);
  fireEvent.change(screen.getByRole('textbox', { name: '保留 TCP 端口' }), { target: { value: '22,2222,8000-8002' } });
  fireEvent.click(screen.getByRole('button', { name: '保存变更' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0]).toMatchObject({
    spec: {
      name: 'Host L3',
      addresses: [
        {
          address: '203.0.113.20',
          kind: 'dedicated',
          reserved_ports: {
            tcp: [
              { start: 22, end: 22 },
              { start: 2222, end: 2222 },
              { start: 8000, end: 8002 },
            ],
            udp: [],
          },
        },
      ],
      transports: [],
    },
  });
});

it.each(['xray', 'phantun'] as const)('publishes %s as the selected cross-machine carrier', async carrier => {
  const workspace = cvmWorkspace('board');
  const writes: Record<string, unknown>[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input, init) => {
      if (init?.method && init.method !== 'GET') {
        writes.push(JSON.parse(String(init.body)));
        return Response.json({});
      }
      return Response.json(String(input) === '/cvm/networks' ? nicCatalog() : entryBoard(workspace, 'board'));
    }),
  );
  client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <EntryPublications workspace={workspace} />
    </QueryClientProvider>,
  );
  fireEvent.click(await screen.findByRole('button', { name: '添加入口地址' }));
  fireEvent.change(screen.getByRole('textbox', { name: '名称' }), { target: { value: 'Transport entry' } });
  if (carrier === 'xray') {
    fireEvent.change(screen.getByRole('combobox', { name: '传输方式' }), { target: { value: 'xray' } });
    expect(screen.queryByRole('combobox', { name: 'WireGuard 传输' })).toBeNull();
  } else {
    fireEvent.change(screen.getByRole('combobox', { name: 'WireGuard 传输' }), { target: { value: 'phantun' } });
  }
  fireEvent.click(screen.getByRole('button', { name: '保存变更' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0]).toMatchObject({
    spec: {
      wireguard_transport: carrier === 'phantun' ? 'phantun' : 'udp',
      transports: [{ kind: carrier === 'xray' ? 'xray' : 'wireguard', mtu: carrier === 'phantun' ? 1380 : 1420 }],
    },
  });
});

it('keeps new carrier options unavailable on an older provider', async () => {
  const workspace = cvmWorkspace('board');
  const catalog = nicCatalog();
  delete catalog.providers[0].capabilities.entry_xray;
  delete catalog.providers[0].capabilities.entry_phantun;
  vi.stubGlobal(
    'fetch',
    vi.fn(async input => Response.json(String(input) === '/cvm/networks' ? catalog : entryBoard(workspace, 'board'))),
  );
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <EntryPublications workspace={workspace} />
    </QueryClientProvider>,
  );
  fireEvent.click(await screen.findByRole('button', { name: '添加入口地址' }));
  expect((screen.getByRole('option', { name: 'Xray' }) as HTMLOptionElement).disabled).toBe(true);
  expect((screen.getByRole('option', { name: 'Phantun / FakeTCP' }) as HTMLOptionElement).disabled).toBe(true);
});
