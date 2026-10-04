import { useCallback, useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { SnapshotIngress } from '../src/api';
import {
  effectiveProjectionEndpoint,
  IngressProjectionRow,
  useProjectionHandles,
  withProjectionEndpoint,
  type ProjectionHandle,
} from '../src/panes/chains';

const ingress: SnapshotIngress = {
  id: 'ingress-1',
  chain: 'chain-1',
  node: 'node-1',
  bind: '0.0.0.0',
  port: 443,
  projection: {},
  guard: {
    no_private: true,
    no_bittorrent: true,
    no_mail: true,
    no_udp_amplification: true,
    tcp_and_quic_only: false,
  },
  identity: { public_key: 'public-key', short_ids: ['0123456789abcdef'] },
  wires: { vless: { kind: 'vless-reality' } },
};

function ProjectionHarness({
  onReport,
  source = ingress,
  family = 'v4',
  saving = false,
}: {
  onReport: (handle: ProjectionHandle) => void;
  source?: SnapshotIngress;
  family?: 'v4' | 'v6';
  saving?: boolean;
}) {
  const [client] = useState(
    () =>
      new QueryClient({
        defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
      }),
  );
  const { registrars } = useProjectionHandles();
  const register = registrars[`vless:${family}`];
  const reportHandle = useCallback(
    (handle: ProjectionHandle) => {
      onReport(handle);
      register(handle);
    },
    [onReport, register],
  );

  return (
    <QueryClientProvider client={client}>
      <dl>
        <IngressProjectionRow
          appId="app-1"
          ingress={source}
          protocol="vless"
          family={family}
          node={{ public_ipv4: '192.0.2.1', public_ipv6: null }}
          editable
          onHandle={reportHandle}
          saving={saving}
        />
      </dl>
    </QueryClientProvider>
  );
}

afterEach(cleanup);

describe('projection handle registration', () => {
  it('shows the effective public endpoint below the default/conversion switch', async () => {
    const reports = vi.fn<(handle: ProjectionHandle) => void>();
    const view = render(<ProjectionHarness onReport={reports} />);

    await waitFor(() => expect(reports).toHaveBeenCalledTimes(1));
    expect(view.queryByText(/使用机器公网地址/)).toBeNull();
    expect(view.getByText('当前结果')).toBeTruthy();
    expect(view.getByText('192.0.2.1:443')).toBeTruthy();
  });

  it('does not present a NAT address as a generated client endpoint', () => {
    const reports = vi.fn<(handle: ProjectionHandle) => void>();
    const [client] = [new QueryClient({ defaultOptions: { queries: { retry: false } } })];
    const onHandle = (handle: ProjectionHandle) => reports(handle);
    const view = render(
      <QueryClientProvider client={client}>
        <dl>
          <IngressProjectionRow
            appId="app-1"
            ingress={ingress}
            protocol="vless"
            family="v4"
            node={{ public_ipv4: '192.0.2.1', public_ipv6: null, public_ipv4_nat: true }}
            editable
            onHandle={onHandle}
          />
        </dl>
      </QueryClientProvider>,
    );

    expect(view.queryByText('192.0.2.1:443')).toBeNull();
    expect(view.getByText('机器公网 IPv4 不可直连，不生成此条订阅')).toBeTruthy();
  });

  it('reports once per semantic form change instead of looping after the parent rerenders', async () => {
    const reports = vi.fn<(handle: ProjectionHandle) => void>();
    const view = render(<ProjectionHarness onReport={reports} />);

    await waitFor(() => expect(reports).toHaveBeenCalledTimes(1));
    expect(reports.mock.calls[0]?.[0]).toMatchObject({ dirty: false, blocked: false });

    fireEvent.click(view.getByRole('button', { name: '转换' }));
    await waitFor(() => expect(reports).toHaveBeenCalledTimes(2));
    expect(reports.mock.calls[1]?.[0]).toMatchObject({ dirty: true, blocked: true });
    expect(view.getByText('填写有效地址和端口后显示')).toBeTruthy();

    fireEvent.change(view.getByPlaceholderText('地址或域名'), { target: { value: 'edge.example.com' } });
    await waitFor(() => expect(reports).toHaveBeenCalledTimes(3));
    expect(reports.mock.calls[2]?.[0]).toMatchObject({ dirty: true, blocked: false });
    expect(view.getByText('edge.example.com:443')).toBeTruthy();
  });

  it('expands a legacy shared address into an independent protocol mapping', () => {
    const legacy: SnapshotIngress = {
      ...ingress,
      projection: { v4: { host: 'legacy.edge.example', port: 10443 } },
      wires: {
        ...ingress.wires,
        anytls: {
          port: 2443,
          security: 'tls',
          padding_scheme: [],
          masquerade: { kind: 'not-found' },
        },
      },
    };

    expect(effectiveProjectionEndpoint(legacy, 'vless', 'v4')).toEqual({
      host: 'legacy.edge.example',
      port: 10443,
    });
    expect(effectiveProjectionEndpoint(legacy, 'anytls', 'v4')).toEqual({
      host: 'legacy.edge.example',
      port: 2443,
    });
    expect(withProjectionEndpoint(legacy.projection, 'anytls', 'v4', null)).toEqual({
      v4: { host: 'legacy.edge.example', port: 10443 },
      anytls: { v4: null },
    });
  });

  it('opens a saved editor without marking it dirty, tracks real changes, and restores the summary', async () => {
    const reports = vi.fn<(handle: ProjectionHandle) => void>();
    const source = { ...ingress, projection: { v4: { host: 'edge.example.com', port: 10443 } } };
    const view = render(<ProjectionHarness source={source} onReport={reports} />);
    const latest = () => reports.mock.lastCall![0];

    expect(view.getByText('edge.example.com:10443')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '编辑' }));
    await waitFor(() => expect(latest()).toMatchObject({ dirty: false, editing: true, blocked: false }));
    expect(latest().apply(source.projection)).toEqual(source.projection);
    const port = view.getByRole('textbox', { name: 'VLESS · TLS / REALITY IPv4 端口' });
    fireEvent.change(port, { target: { value: '2443' } });
    await waitFor(() => expect(latest()).toMatchObject({ dirty: true, editing: true, blocked: false }));
    expect(latest().apply(source.projection).v4?.port).toBe(2443);
    fireEvent.change(port, { target: { value: '10443' } });
    await waitFor(() => expect(latest()).toMatchObject({ dirty: false, editing: true, blocked: false }));
    act(() => latest().reset());
    expect(view.queryByRole('textbox')).toBeNull();
    expect(view.getByText('edge.example.com:10443')).toBeTruthy();
    expect(latest()).toMatchObject({ dirty: false, editing: false, blocked: false });
  });

  it('discards a new conversion when returning to default and blocks invalid ports', async () => {
    const reports = vi.fn<(handle: ProjectionHandle) => void>();
    const view = render(<ProjectionHarness onReport={reports} />);
    fireEvent.click(view.getByRole('button', { name: '转换' }));
    fireEvent.change(view.getByRole('textbox', { name: 'VLESS · TLS / REALITY IPv4 地址或域名' }), {
      target: { value: 'edge.example.com' },
    });
    fireEvent.change(view.getByRole('textbox', { name: 'VLESS · TLS / REALITY IPv4 端口' }), {
      target: { value: '65536' },
    });
    await waitFor(() => expect(reports.mock.lastCall![0]).toMatchObject({ dirty: true, blocked: true }));
    fireEvent.click(view.getByRole('button', { name: '默认' }));
    await waitFor(() =>
      expect(reports.mock.lastCall![0]).toMatchObject({ dirty: false, editing: false, blocked: false }),
    );
    expect(view.queryByRole('textbox')).toBeNull();
  });

  it('formats a saved IPv6 endpoint unambiguously and freezes controls while saving', () => {
    const reports = vi.fn<(handle: ProjectionHandle) => void>();
    const source = { ...ingress, projection: { v6: { host: '2001:db8::20', port: 10443 } } };
    const view = render(<ProjectionHarness source={source} family="v6" onReport={reports} />);
    expect(view.getByText('[2001:db8::20]:10443')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '编辑' }));
    view.rerender(<ProjectionHarness source={source} family="v6" onReport={reports} saving />);
    for (const input of view.getAllByRole('textbox')) expect((input as HTMLInputElement).disabled).toBe(true);
    expect((view.getByRole('button', { name: '默认' }) as HTMLButtonElement).disabled).toBe(true);
    expect((view.getByRole('button', { name: '转换' }) as HTMLButtonElement).disabled).toBe(true);
  });
});
