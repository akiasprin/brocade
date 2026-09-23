import { useCallback, useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
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

function ProjectionHarness({ onReport }: { onReport: (handle: ProjectionHandle) => void }) {
  const [client] = useState(
    () =>
      new QueryClient({
        defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
      }),
  );
  const { onV4Handle } = useProjectionHandles();
  const reportHandle = useCallback(
    (handle: ProjectionHandle) => {
      onReport(handle);
      onV4Handle(handle);
    },
    [onReport, onV4Handle],
  );

  return (
    <QueryClientProvider client={client}>
      <dl>
        <IngressProjectionRow
          appId="app-1"
          ingress={ingress}
          protocol="vless"
          family="v4"
          node={{ public_ipv4: '192.0.2.1', public_ipv6: null }}
          editable
          onHandle={reportHandle}
        />
      </dl>
    </QueryClientProvider>
  );
}

afterEach(cleanup);

describe('projection handle registration', () => {
  it('keeps the default public endpoint implicit', async () => {
    const reports = vi.fn<(handle: ProjectionHandle) => void>();
    const view = render(<ProjectionHarness onReport={reports} />);

    await waitFor(() => expect(reports).toHaveBeenCalledTimes(1));
    expect(view.queryByText(/使用机器公网地址/)).toBeNull();
    expect(view.queryByText(/192\.0\.2\.1/)).toBeNull();
  });

  it('reports once per semantic form change instead of looping after the parent rerenders', async () => {
    const reports = vi.fn<(handle: ProjectionHandle) => void>();
    const view = render(<ProjectionHarness onReport={reports} />);

    await waitFor(() => expect(reports).toHaveBeenCalledTimes(1));
    expect(reports.mock.calls[0]?.[0]).toMatchObject({ dirty: false, blocked: false });

    fireEvent.click(view.getByRole('button', { name: '转换' }));
    await waitFor(() => expect(reports).toHaveBeenCalledTimes(2));
    expect(reports.mock.calls[1]?.[0]).toMatchObject({ dirty: true, blocked: true });

    fireEvent.change(view.getByPlaceholderText('地址或域名'), { target: { value: 'edge.example.com' } });
    await waitFor(() => expect(reports).toHaveBeenCalledTimes(3));
    expect(reports.mock.calls[2]?.[0]).toMatchObject({ dirty: true, blocked: false });
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
});
