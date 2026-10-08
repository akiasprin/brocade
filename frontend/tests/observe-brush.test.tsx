import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ReactNode } from 'react';

/* Every chart instance keeps its own last option, so convertToPixel can map that chart's axes onto
   one fixed plot area (40–640 × 10–190 px inside the container). */
const chartMock = vi.hoisted(() => ({
  plot: { left: 40, right: 640, top: 10, bottom: 190 },
  instances: [] as Array<{ option: Record<string, unknown> | null; dispatchAction: ReturnType<typeof vi.fn> }>,
}));

vi.mock('echarts/core', () => ({
  use: vi.fn(),
  connect: vi.fn(),
  init: vi.fn(() => {
    const instance = {
      option: null as Record<string, unknown> | null,
      group: '',
      setOption(option: Record<string, unknown>) {
        instance.option = option;
      },
      showLoading: vi.fn(),
      hideLoading: vi.fn(),
      resize: vi.fn(),
      dispose: vi.fn(),
      dispatchAction: vi.fn(),
      convertToPixel(_finder: unknown, [x, y]: number[]) {
        const option = instance.option as { xAxis: { min: number; max: number }[]; yAxis: { max: number } };
        const { left, right, top, bottom } = chartMock.plot;
        const xAxis = option.xAxis[0];
        return [
          left + ((x - xAxis.min) / (xAxis.max - xAxis.min)) * (right - left),
          bottom - (y / option.yAxis.max) * (bottom - top),
        ];
      },
    };
    chartMock.instances.push(instance);
    return instance;
  }),
}));
vi.mock('echarts/charts', () => ({ CustomChart: {}, LineChart: {} }));
vi.mock('echarts/components', () => ({ GridComponent: {}, MarkLineComponent: {}, TooltipComponent: {} }));
vi.mock('echarts/renderers', () => ({ CanvasRenderer: {} }));

let ThroughputChart: typeof import('../src/panes/telemetry').ThroughputChart;
let ObserveBrushProvider: typeof import('../src/ui/observe-brush').ObserveBrushProvider;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })),
  );
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    },
  );
  ({ ThroughputChart } = await import('../src/panes/telemetry'));
  ({ ObserveBrushProvider } = await import('../src/ui/observe-brush'));
});

beforeEach(() => {
  chartMock.instances.length = 0;
});

afterEach(() => {
  cleanup();
  document.body.classList.remove('observe-brushing');
});

// One hour over 600 px: 6 s per pixel, so the drag snaps to 10 s.
const START = 1_700_000_000;
const END = START + 3_600;
const times = Array.from({ length: 120 }, (_, index) => START + 30 * (index + 1));
const values = times.map(() => 100e6);

const chart = (name: string) => (
  <ThroughputChart
    timesUnixSecs={times}
    rangeStartUnixSecs={START}
    rangeEndUnixSecs={END}
    rx={values}
    tx={values}
    rxName={name}
    txName={`${name}-2`}
  />
);

function mount({
  onSelect = vi.fn((_start: number, _end: number) => true),
  onZoomOut = vi.fn(),
  pending = false,
}: {
  onSelect?: (start: number, end: number) => boolean;
  onZoomOut?: () => void;
  pending?: boolean;
} = {}) {
  const tree = (pendingNow: boolean): ReactNode => (
    <ObserveBrushProvider rangeKey={3_600} pending={pendingNow} onSelect={onSelect} onZoomOut={onZoomOut}>
      {chart('接收')}
      {chart('用户')}
    </ObserveBrushProvider>
  );
  const view = render(tree(pending));
  const plots = [...view.container.querySelectorAll<HTMLElement>('.observe-plot')];
  const bands = [...view.container.querySelectorAll<HTMLElement>('.observe-brush-band')];
  const tags = [...view.container.querySelectorAll<HTMLElement>('.observe-brush-tag')];
  return { view, plots, bands, tags, onSelect, onZoomOut, rerender: (next: boolean) => view.rerender(tree(next)) };
}

const press = (plot: HTMLElement, clientX: number, pointerType = 'mouse') =>
  fireEvent.pointerDown(plot, { button: 0, pointerId: 1, pointerType, clientX, clientY: 100 });
const moveTo = (clientX: number) => fireEvent.pointerMove(window, { pointerId: 1, clientX, clientY: 100 });
const release = (clientX: number) => fireEvent.pointerUp(window, { pointerId: 1, clientX, clientY: 100 });

describe('observation chart brush', () => {
  it('paints one selection on every chart and hands the snapped range to the page', () => {
    const { plots, bands, tags, onSelect, rerender } = mount();

    press(plots[0], 140);
    moveTo(240);

    expect(document.body.classList.contains('observe-brushing')).toBe(true);
    expect(bands.map(band => band.hidden)).toEqual([false, false]);
    expect(bands[0].hasAttribute('data-linked')).toBe(false);
    expect(bands[1].hasAttribute('data-linked')).toBe(true);
    expect([bands[0].style.left, bands[0].style.width]).toEqual(['140px', '100px']);
    expect([bands[0].style.top, bands[0].style.height]).toEqual(['10px', '180px']);
    expect(tags[0].hidden).toBe(false);
    expect(tags[0].textContent).toContain('→');
    expect(tags[0].textContent).toContain('10 分钟');
    expect(tags[1].hidden).toBe(true);
    for (const instance of chartMock.instances) {
      expect(instance.dispatchAction).toHaveBeenCalledWith({ type: 'hideTip' });
      expect(instance.dispatchAction).toHaveBeenCalledWith({ type: 'updateAxisPointer', currTrigger: 'leave' });
    }

    release(240);
    expect(onSelect).toHaveBeenCalledWith(START + 600, START + 1_200);
    expect(document.body.classList.contains('observe-brushing')).toBe(false);
    // The selection stays on screen while the page reads the new range, then clears with it.
    expect(bands.every(band => !band.hidden && band.hasAttribute('data-pending'))).toBe(true);
    rerender(true);
    expect(bands[0].hidden).toBe(false);
    rerender(false);
    expect(bands.map(band => band.hidden)).toEqual([true, true]);
  });

  it('widens a selection shorter than a minute and says so', () => {
    const { plots, tags, onSelect } = mount();
    press(plots[0], 400);
    moveTo(405);
    expect(tags[0].textContent).toContain('最短 1 分钟');
    release(405);
    // 2160 s → 2190 s is 30 s; centred on 2175 s and snapped to 10 s it becomes 2150 s → 2210 s.
    expect(onSelect).toHaveBeenCalledWith(START + 2_150, START + 2_210);
  });

  it('treats Escape, short presses and touch drags as no selection', () => {
    const { plots, bands, onSelect } = mount();

    press(plots[0], 140);
    moveTo(300);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(bands.map(band => band.hidden)).toEqual([true, true]);
    expect(document.body.classList.contains('observe-brushing')).toBe(false);
    release(300);

    press(plots[0], 140);
    moveTo(142);
    release(142);

    press(plots[0], 140, 'touch');
    moveTo(300);
    release(300);

    expect(bands.map(band => band.hidden)).toEqual([true, true]);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('ignores presses outside the plot area and clears a selection the page did not apply', () => {
    const onSelect = vi.fn((_start: number, _end: number) => false);
    const { plots, bands } = mount({ onSelect });

    press(plots[0], 20);
    moveTo(300);
    release(300);
    expect(onSelect).not.toHaveBeenCalled();

    press(plots[0], 140);
    moveTo(300);
    release(300);
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(bands.map(band => band.hidden)).toEqual([true, true]);
  });

  it('zooms out on a double click inside the plot area only', () => {
    const { plots, onZoomOut } = mount();
    fireEvent.doubleClick(plots[1], { clientX: 20, clientY: 100 });
    expect(onZoomOut).not.toHaveBeenCalled();
    fireEvent.doubleClick(plots[1], { clientX: 300, clientY: 100 });
    expect(onZoomOut).toHaveBeenCalledTimes(1);
  });

  it('marks only the plot area with the selection cursor', () => {
    const { plots } = mount();
    fireEvent.pointerMove(plots[0], { pointerType: 'mouse', clientX: 300, clientY: 100 });
    expect(plots[0].hasAttribute('data-brush-hot')).toBe(true);
    fireEvent.pointerMove(plots[0], { pointerType: 'mouse', clientX: 300, clientY: 205 });
    expect(plots[0].hasAttribute('data-brush-hot')).toBe(false);
  });

  it('renders the bare chart container without a page that handles ranges', () => {
    const view = render(chart('接收'));
    const plot = view.container.querySelector<HTMLElement>('.observe-plot');
    expect(plot?.classList.contains('ndtp-ec')).toBe(true);
    expect(plot?.querySelector('.observe-plot-canvas')).toBeTruthy();
    expect(view.container.querySelector('.observe-brush-layer')).toBeNull();
    act(() => {
      fireEvent.pointerDown(plot!, { button: 0, pointerId: 1, pointerType: 'mouse', clientX: 140, clientY: 100 });
    });
    moveTo(300);
    expect(document.body.classList.contains('observe-brushing')).toBe(false);
    release(300);
  });
});
