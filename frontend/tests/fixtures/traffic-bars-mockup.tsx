// Mockup: 机器用量弹层里的柱状图（流出统计、流入统计、历史流量）——聚焦时其余类别淡化的程度，以及顺带发现的细节问题。
// Synthetic data only; never imported by the app. The page runs the production NodeTrafficDetails (实时流量 →
// 流入统计 → 流出统计) on a stubbed Console. traffic-bars-mockup-echarts.ts rewrites the two ECharts bar charts
// per option and traffic-bars-mockup.css restyles the HTML bars, so 现状 and every option share the same cards.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/traffic-bars-mockup.html?option=a&period=day&scene=typical&width=wide
// Query: option=current|a|b|c, period=day|week|month, scene=typical|many|solo|gaps|history|quiet|sites-off,
//        history=normal|duplex|units|single, width=wide|card|phone, theme=dark|light, nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useLayoutEffect, useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { SiteUsagePeriod } from '../../src/api';
import { theme } from '../../src/forge/theme';
import { NodeRealtimeProvider } from '../../src/node-realtime';
import { NodeTrafficDetails } from '../../src/node-traffic';
import { SegmentedControl } from '../../src/ui/bits';
import { SCENE_OPTIONS, type SceneKey } from './node-traffic-mockup-data';
import {
  HISTORY_CASES,
  installBackend,
  NODE_ID,
  NODE_NAME,
  selectHistoryCase,
  selectScene,
  snapshotOf,
  type HistoryCase,
} from './traffic-bars-mockup-data';
import { setBarOption, type BarOption } from './traffic-bars-mockup-echarts';
import '../../src/styles.css';
import './traffic-bars-mockup.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
installBackend();

type Width = 'wide' | 'card' | 'phone';
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

const OPTIONS: { value: BarOption; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 轻淡化',
    note: '其余类别保留 50% 不透明度：颜色仍能分辨，每根柱的总高度与组成都在。与现状最接近，只改淡化程度。',
  },
  {
    value: 'b',
    name: 'B 其余转灰',
    note: '指向右侧某一行时，其余类别换成同一种中性灰，不降透明度，柱形完整；只有聚焦的类别保留颜色。聚焦灰色类别（其余站点、未归类）时，它提亮为浅灰。悬停图中色块按 A 轻淡化。',
  },
  {
    value: 'c',
    name: 'C 聚焦置底',
    note: '指向右侧某一行时，该类别移到柱底、贴着横轴，各时段的高低可以直接比较；其余类别按 A 淡化。流入统计窄卡的聚焦本来就是这种方式。悬停图中色块按 A 轻淡化、不移动。',
  },
  {
    value: 'current',
    name: '现状',
    note: '生产页面，未改动：聚焦时其余类别降到 10% 不透明度，「现在」线随第一类一起变淡；直接悬停图中色块也是如此。',
  },
];
const COMMON = [
  '「现在」线和未观测底纹不再随聚焦变淡。',
  '悬停图中色块即按 A 轻淡化其余类别，右侧对应行同步高亮；方案自己的聚焦方式只由右侧列表触发。',
  '提示框与右侧列表同序，首行是该时段合计，聚焦类别加粗；悬停时整列加一道浅底，代替穿过柱子的虚线。',
  '纵轴单位只在轴顶写一次，刻度只写数字，与其他观测图一致。',
  '色块之间留 1 px 分隔（当周的小时柱太窄，不加）；半透明的灰色类别改为不透明，叠在一起也分得清。',
  '第 5 类站点改用观测色第 3 档：原第 7 档在黛蓝、玄墨里与端口转发（第 2 档）几乎同色。流入统计的接入链同一规则，端口转发两张卡同色。',
  '去掉图下说明「每小时一柱 · 固定本月主要 5 类站点着色……」。',
  '历史流量：下行、上行叠成一根柱，量程按同一时段合计计算，标题、刻度与提示框单位一致。柱宽跟随实际采样窗口和容器宽度，缺口保留空白。',
  '流入统计：流向图与窄卡的聚焦淡化按所选方案处理（原为 8% 和 16%）。',
];
const PERIODS = [
  { value: 'day' as const, label: '当天' },
  { value: 'week' as const, label: '当周' },
  { value: 'month' as const, label: '当月' },
];
const WIDTHS = [
  { value: 'wide' as const, label: '桌面' },
  { value: 'card' as const, label: '窄卡' },
  { value: 'phone' as const, label: '手机' },
];

function apply(option: BarOption) {
  setBarOption(option);
  document.documentElement.dataset.tbOption = option;
}
apply(pick<BarOption>(params.get('option'), ['a', 'b', 'c', 'current'], 'a'));

function Stage({ scene, period, history }: { scene: SceneKey; period: SiteUsagePeriod; history: HistoryCase }) {
  const [client] = useState(() => {
    selectScene(scene);
    selectHistoryCase(history);
    return new QueryClient({ defaultOptions: { queries: { retry: false } } });
  });
  const [snapshot] = useState(snapshotOf);
  return (
    <section className="dialog-surface tb-surface" aria-label={`${NODE_NAME} · 用量`}>
      <div className="node-usage-body tb-body">
        <QueryClientProvider client={client}>
          <NodeRealtimeProvider nodeId={NODE_ID}>
            <NodeTrafficDetails nodeId={NODE_ID} nodeName={NODE_NAME} snapshot={snapshot} period={period} />
          </NodeRealtimeProvider>
        </QueryClientProvider>
      </div>
    </section>
  );
}

function App() {
  const [option, setOption] = useState<BarOption>(
    pick<BarOption>(params.get('option'), ['a', 'b', 'c', 'current'], 'a'),
  );
  const [period, setPeriod] = useState<SiteUsagePeriod>(
    pick<SiteUsagePeriod>(params.get('period'), ['day', 'week', 'month'], 'day'),
  );
  const [scene, setScene] = useState<SceneKey>(
    pick(
      params.get('scene'),
      SCENE_OPTIONS.map(item => item.value),
      'typical',
    ),
  );
  const [width, setWidth] = useState<Width>(pick<Width>(params.get('width'), ['wide', 'card', 'phone'], 'wide'));
  const [historyCase, setHistoryCase] = useState<HistoryCase>(
    pick(
      params.get('history'),
      HISTORY_CASES.map(item => item.value),
      'normal',
    ),
  );
  const [mode, setMode] = useState(theme.snapshot());
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    next.set('period', period);
    next.set('scene', scene);
    next.set('width', width);
    next.set('theme', mode);
    next.set('history', historyCase);
    history.replaceState(null, '', `?${next}`);
  }, [option, period, scene, width, mode, historyCase]);
  const info = OPTIONS.find(item => item.value === option)!;
  return (
    <>
      {nav && (
        <div className="mk-bar">
          <div className="mk-bar-row">
            <span className="mk-title">
              柱状图<small>MOCKUP</small>
            </span>
            <div className="mk-controls">
              <label className="mk-ctl">
                <span className="mk-ctl-label">方案</span>
                <SegmentedControl
                  value={option}
                  options={OPTIONS.map(item => ({ value: item.value, label: item.name }))}
                  ariaLabel="方案"
                  onChange={next => {
                    apply(next);
                    setOption(next);
                  }}
                />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">统计</span>
                <SegmentedControl value={period} options={PERIODS} ariaLabel="统计范围" onChange={setPeriod} />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">宽度</span>
                <SegmentedControl value={width} options={WIDTHS} ariaLabel="宽度" onChange={setWidth} />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">场景</span>
                <select value={scene} onChange={event => setScene(event.target.value as SceneKey)}>
                  {SCENE_OPTIONS.map(item => (
                    <option key={item.value} value={item.value}>
                      {item.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">历史</span>
                <select
                  aria-label="历史流量场景"
                  value={historyCase}
                  onChange={event => setHistoryCase(event.target.value as HistoryCase)}
                >
                  {HISTORY_CASES.map(item => (
                    <option key={item.value} value={item.value}>
                      {item.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">主题</span>
                <SegmentedControl
                  value={mode}
                  options={[
                    { value: 'dark' as const, label: '深' },
                    { value: 'light' as const, label: '浅' },
                  ]}
                  ariaLabel="主题"
                  onChange={next => {
                    if (next !== theme.snapshot()) theme.toggle();
                    setMode(theme.snapshot());
                  }}
                />
              </label>
            </div>
          </div>
          <p className="mk-scene-note">
            <b>{info.name}</b>
            {info.note}
          </p>
          {option !== 'current' && (
            <details className="mk-common" open>
              <summary>三个方案共同的改动</summary>
              <ol>
                {COMMON.map(item => (
                  <li key={item}>{item}</li>
                ))}
              </ol>
            </details>
          )}
        </div>
      )}
      <main className="tb-stage" data-width={width}>
        {/* Each review dataset starts the real cards and their query cache afresh. */}
        <Stage key={`${option}-${scene}-${historyCase}`} scene={scene} period={period} history={historyCase} />
      </main>
    </>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
