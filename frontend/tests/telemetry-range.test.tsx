import { cleanup, fireEvent, render } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { fetchNodePingProbeRange, fetchUsageNodeSeries, fetchUsageNodeSeriesRange } from '../src/api';
import type { LoadRange } from '../src/panes/nodes';

let ObserveRangeControl: typeof import('../src/panes/nodes').ObserveRangeControl;
let ObserveLinkControl: typeof import('../src/panes/nodes').ObserveLinkControl;
let LOAD_RANGES: typeof import('../src/panes/nodes').LOAD_RANGES;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })),
  );
  ({ ObserveRangeControl, ObserveLinkControl, LOAD_RANGES } = await import('../src/panes/nodes'));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('machine telemetry range', () => {
  it('keeps chart linking off by default and exposes it as a labelled slider', () => {
    const Harness = () => {
      const [linked, setLinked] = useState(false);
      return <ObserveLinkControl value={linked} onChange={setLinked} />;
    };
    const view = render(<Harness />);
    const control = view.getByRole('switch', { name: '同组图表联动' });

    expect(control.getAttribute('aria-checked')).toBe('false');
    expect(view.getByText('同组图表联动')).toBeTruthy();
    fireEvent.click(control);
    expect(control.getAttribute('aria-checked')).toBe('true');
  });

  it('uses the clock dropdown, offers every full range label and switches the selected item', () => {
    const Harness = () => {
      const [selected, setSelected] = useState<LoadRange>(LOAD_RANGES[0]);
      return <ObserveRangeControl value={selected} onChange={setSelected} />;
    };
    const view = render(<Harness />);

    const trigger = view.getByRole('button', { name: '观测时间范围：近 30 分钟' });
    expect(trigger.getAttribute('aria-expanded')).toBe('false');

    fireEvent.click(trigger);
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
    expect(view.getAllByRole('option').map(option => option.textContent)).toEqual([
      '近 30 分钟✓',
      '近 1 小时✓',
      '近 6 小时✓',
      '近 12 小时✓',
      '近 24 小时✓',
    ]);
    expect(view.getByRole('option', { name: '近 30 分钟' }).getAttribute('aria-selected')).toBe('true');

    fireEvent.click(view.getByRole('option', { name: '近 24 小时' }));
    expect(view.getByRole('button', { name: '观测时间范围：近 24 小时' }).getAttribute('aria-expanded')).toBe('false');
    expect(view.queryByRole('listbox')).toBeNull();
  });

  it('closes the range menu on outside interaction and Escape', () => {
    const Harness = () => {
      const [selected, setSelected] = useState<LoadRange>(LOAD_RANGES[0]);
      return <ObserveRangeControl value={selected} onChange={setSelected} />;
    };
    const view = render(<Harness />);
    const trigger = view.getByRole('button', { name: '观测时间范围：近 30 分钟' });

    fireEvent.click(trigger);
    fireEvent.pointerDown(document.body);
    expect(trigger.getAttribute('aria-expanded')).toBe('false');

    fireEvent.click(trigger);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
    expect(document.activeElement).toBe(trigger);
  });

  it('applies one fixed date range instead of turning its duration into a recent window', () => {
    const Harness = () => {
      const [selected, setSelected] = useState<LoadRange>(LOAD_RANGES[0]);
      return (
        <>
          <ObserveRangeControl value={selected} onChange={setSelected} />
          <output>{`${selected.startUnixSecs ?? ''}|${selected.endUnixSecs ?? ''}`}</output>
        </>
      );
    };
    const view = render(<Harness />);
    fireEvent.click(view.getByRole('button', { name: '观测时间范围：近 30 分钟' }));
    fireEvent.change(view.getByLabelText('观测开始时间'), { target: { value: '2026-09-06T10:00' } });
    fireEvent.change(view.getByLabelText('观测结束时间'), { target: { value: '2026-09-06T12:30' } });
    fireEvent.click(view.getByRole('button', { name: '应用时间范围' }));

    const start = Math.floor(new Date('2026-09-06T10:00').getTime() / 1000);
    const end = Math.floor(new Date('2026-09-06T12:30').getTime() / 1000);
    expect(view.getByText(`${start}|${end}`)).toBeTruthy();
  });

  it('narrows a long Xray series request to the current machine', async () => {
    const fetchMock = vi.fn(
      async (_input: RequestInfo | URL) => new Response(JSON.stringify({ since: '', month_start: '', nodes: [] })),
    );
    vi.stubGlobal('fetch', fetchMock);

    await fetchUsageNodeSeries(86_400, 'akile-ogvtw-hinet');

    expect(String(fetchMock.mock.calls[0][0])).toBe('/usage/node-series?window_secs=86400&node_id=akile-ogvtw-hinet');
  });

  it('sends identical absolute boundaries to Xray and PING history endpoints', async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL) =>
      new Response(JSON.stringify({ nodes: [], targets: [] })),
    );
    vi.stubGlobal('fetch', fetchMock);

    await fetchUsageNodeSeriesRange(1_700_000_000, 1_700_003_600, 'n1');
    await fetchNodePingProbeRange('n1', 1_700_000_000, 1_700_003_600);

    expect(String(fetchMock.mock.calls[0][0])).toBe(
      '/usage/node-series?start_unix_secs=1700000000&end_unix_secs=1700003600&node_id=n1',
    );
    expect(String(fetchMock.mock.calls[1][0])).toBe(
      '/ping-probe/nodes/n1?start_unix_secs=1700000000&end_unix_secs=1700003600',
    );
  });
});
