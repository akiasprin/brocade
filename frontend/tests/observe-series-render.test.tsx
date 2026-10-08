import { expect, it } from 'vitest';
import { init, use } from 'echarts/core';
import { LineChart } from 'echarts/charts';
import { GridComponent } from 'echarts/components';
import { SVGRenderer } from 'echarts/renderers';
import { observeSeriesData } from '../src/ui/observe-series';

use([LineChart, GridComponent, SVGRenderer]);

it.each([undefined, 'components'])('renders separate line and area segments, including stack %s', stack => {
  const chart = init(null, null, { renderer: 'svg', ssr: true, width: 600, height: 200 });
  try {
    chart.setOption({
      animation: false,
      xAxis: { type: 'value', min: 0, max: 1_200_000, show: false },
      yAxis: { type: 'value', min: 0, max: 60, show: false },
      series: [0, 1].map(index => ({
        type: 'line',
        stack,
        showSymbol: false,
        connectNulls: false,
        lineStyle: { color: '#123456' },
        areaStyle: { color: '#abcdef' },
        data: observeSeriesData(
          [100, 110, 1_000, 1_010],
          [12, 13, 14, 15].map(value => value + index),
        ),
      })),
    });
    const paths = [...chart.renderToSVGString().matchAll(/<path d="([^"]+)"([^>]+)>/g)];
    const strokes = paths.filter(path => path[2].includes('stroke="#123456"'));
    const fills = paths.filter(path => path[2].includes('fill="#abcdef"'));
    expect(strokes).toHaveLength(2);
    expect(fills).toHaveLength(2);
    for (const path of strokes) expect(path[1].match(/M/g)).toHaveLength(2);
    for (const path of fills) expect(path[1].match(/Z/g)).toHaveLength(2);
  } finally {
    chart.dispose();
  }
});
