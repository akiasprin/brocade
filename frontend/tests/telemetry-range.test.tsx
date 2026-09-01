import { cleanup, fireEvent, render } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { fetchUsageNodeSeries } from '../src/api';
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

  it('narrows a long Xray series request to the current machine', async () => {
    const fetchMock = vi.fn(
      async (_input: RequestInfo | URL) => new Response(JSON.stringify({ since: '', month_start: '', nodes: [] })),
    );
    vi.stubGlobal('fetch', fetchMock);

    await fetchUsageNodeSeries(86_400, 'akile-ogvtw-hinet');

    expect(String(fetchMock.mock.calls[0][0])).toBe('/usage/node-series?window_secs=86400&node_id=akile-ogvtw-hinet');
  });
});
