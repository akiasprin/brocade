import { cleanup, fireEvent, render } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { fetchUsageNodeSeries } from '../src/api';
import type { LoadRange } from '../src/panes/nodes';

let ObserveRangeControl: typeof import('../src/panes/nodes').ObserveRangeControl;
let LOAD_RANGES: typeof import('../src/panes/nodes').LOAD_RANGES;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })),
  );
  ({ ObserveRangeControl, LOAD_RANGES } = await import('../src/panes/nodes'));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('machine telemetry range', () => {
  it('offers every supported range and switches the pressed item', () => {
    const Harness = () => {
      const [selected, setSelected] = useState<LoadRange>(LOAD_RANGES[0]);
      return <ObserveRangeControl value={selected} onChange={setSelected} />;
    };
    const view = render(<Harness />);

    expect(view.getAllByRole('button').map(button => button.textContent)).toEqual(['30m', '1h', '6h', '12h', '24h']);
    expect(view.getByRole('button', { name: '30m' }).getAttribute('aria-pressed')).toBe('true');

    fireEvent.click(view.getByRole('button', { name: '24h' }));
    expect(view.getByRole('button', { name: '24h' }).getAttribute('aria-pressed')).toBe('true');
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
