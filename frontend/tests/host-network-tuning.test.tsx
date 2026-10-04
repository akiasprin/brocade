import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { HostNetworkTuning } from '../src/api';
import { HostNetworkTuningSection } from '../src/panes/settings';

const defaults: HostNetworkTuning = {
  gro_flush_timeout_ns: 20_000,
  napi_defer_hard_irqs: 2,
};

function mounted(data = defaults) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <HostNetworkTuningSection editable data={data} />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('host network tuning settings', () => {
  it('shows the fleet defaults and saves both knobs as one policy', async () => {
    const fetchMock = vi.fn(async (_path: string, init: RequestInit) => {
      const body = JSON.parse(String(init.body)) as HostNetworkTuning;
      return { ok: true, status: 200, json: async () => body };
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = mounted();

    expect((view.getByLabelText('GRO 刷新等待') as HTMLInputElement).value).toBe('20000');
    expect((view.getByLabelText('NAPI 延迟硬中断轮数') as HTMLInputElement).value).toBe('2');
    expect(view.getByLabelText('GRO 刷新等待').closest('.host-network-tuning-grid.settings-field-grid-two')).toBeTruthy();
    const note = view.container.querySelector('.settings-block-note');
    expect(note?.textContent).toContain('gro_flush_timeout');
    expect(note?.textContent).toContain('napi_defer_hard_irqs');
    fireEvent.change(view.getByLabelText('GRO 刷新等待'), { target: { value: '40000' } });
    fireEvent.change(view.getByLabelText('NAPI 延迟硬中断轮数'), { target: { value: '4' } });
    fireEvent.click(view.getByRole('button', { name: '保存网卡调优' }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/host-network-tuning');
    expect(JSON.parse(String(init.body))).toEqual({
      gro_flush_timeout_ns: 40_000,
      napi_defer_hard_irqs: 4,
    });
  });

  it('accepts zero as rollback but disables save outside the bounded range', () => {
    const view = mounted();
    fireEvent.change(view.getByLabelText('GRO 刷新等待'), { target: { value: '0' } });
    fireEvent.change(view.getByLabelText('NAPI 延迟硬中断轮数'), { target: { value: '0' } });
    expect((view.getByRole('button', { name: '保存网卡调优' }) as HTMLButtonElement).disabled).toBe(false);

    fireEvent.change(view.getByLabelText('NAPI 延迟硬中断轮数'), { target: { value: '65' } });
    expect((view.getByRole('button', { name: '保存网卡调优' }) as HTMLButtonElement).disabled).toBe(true);
    expect(view.getByText(/延迟轮数需为 0–64/)).toBeTruthy();
  });
});
