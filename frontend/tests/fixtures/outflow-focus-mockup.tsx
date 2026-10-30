// Mockup: 机器用量弹层里，点流入统计中一位用户的流向，把流出统计聚焦到这位用户；点空白处恢复整机。
// Synthetic data only; never imported by the app. The two cards are forks of production
// (outflow-focus-node-traffic.tsx, outflow-focus-site-usage.tsx) on the stubbed Console of inflow-details-mockup-data.ts.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/outflow-focus-mockup.html?option=a
// Query: option=a|b|current, focus=<user id> starts focused, period=day|week|month, theme=dark|light (read by the HTML
//        before modules load), scroll=inflow|outflow scrolls the dialog to a card, nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useLayoutEffect, useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { SiteUsagePeriod } from '../../src/api';
import { NodeTrafficDialog as CurrentDialog } from '../../src/node-traffic';
import { SegmentedControl } from '../../src/ui/bits';
import { DETAIL_USERS, installBackend, NODE_ID, NODE_NAME, snapshotOf } from './inflow-details-mockup-data';
import { NodeTrafficDialog } from './outflow-focus-node-traffic';
import type { FocusOption } from './outflow-focus-site-usage';
import '../../src/styles.css';
import './outflow-focus-mockup.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

type Choice = FocusOption | 'current';
const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 只看该用户',
    note: '流出统计换成这位用户在本机的数据：读数、柱图、排行和两类明细都只算这位用户，占比以这位用户的合计为分母。卡头用户名旁的 × 或点空白处恢复整机。',
  },
  {
    value: 'b',
    name: 'B 在整机中标出',
    note: '柱高仍是整机：这位用户的流量按站点着色，本机其他用户合为最上面一层灰。排行列出这位用户的各类站点，最后一行是其他用户，占比都以整机为分母；读数写明占本机多少。',
  },
  {
    value: 'current',
    name: '现状',
    note: '生产版本：点流向只在指针停留时高亮；点用户名打开流入明细；流出统计始终是整机，点站点类别可看本机主要用户。',
  },
];
const COMMON = [
  '流入统计：点一位用户的流向或左侧节点，选中这位用户；流向保持高亮，其余流向、用户与没用到的接入链降到一半，与柱状图 mockup 方案 A 相同。指针移到别处仍可预览，离开后回到选中的用户。',
  '再点同一位用户、点流出统计卡头的 ×，或点弹层里任意空白处，恢复整机。点到按钮、图表、链接或正在选中文字时不算空白。',
  '点用户名仍打开流入明细；窄卡（排行条形）里点整行选中用户，点名字打开流入明细。',
  '切换当天／当周／当月或自选区间时保留所选用户；流出统计的站点颜色仍取整机当月排行，聚焦前后同一类站点不换色。',
  '只看一位用户时，站点行不再展开「本机主要用户」；其他站点与嗅探失败仍跳到明细，明细也只列这位用户。',
  '第一次读取这位用户时整机读数留在原处并变浅，读到后替换，卡片高度不跳。',
  '需要 Console 给 /nodes/{id}/site-usage、/other-domains、/sniffing-failed-targets 加上可选的 tenant_id、user_id，按租户范围校验；数据来自现有的用户 × 站点小时表，不改 Agent 与 Xray。',
];

/* ══ 合成后端 ══ */

installBackend();
const snapshot = snapshotOf();
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
client.setQueryData(['snapshot'], snapshot);

const userOf = (id: string | null) => DETAIL_USERS.find(user => user.id === id) ?? null;

function Mockup() {
  const [option, setOption] = useState<Choice>(pick<Choice>(params.get('option'), ['a', 'b', 'current'], 'a'));
  const period = pick<SiteUsagePeriod>(params.get('period'), ['day', 'week', 'month'], 'day');
  const preset = userOf(params.get('focus'));
  const [open, setOpen] = useState(true);
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    history.replaceState(history.state, '', `?${next}`);
    document.documentElement.dataset.mkOption = option;
  }, [option]);
  // 弹窗让出审阅条：量出它的高度交给 CSS。
  useLayoutEffect(() => {
    const root = document.documentElement;
    root.dataset.mkNav = nav ? 'true' : 'false';
    const bar = document.querySelector<HTMLElement>('.mk-bar');
    if (!bar) return;
    const measure = () => root.style.setProperty('--mk-bar-h', `${bar.offsetHeight}px`);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(bar);
    return () => observer.disconnect();
  }, [nav]);
  // 截图用：把弹层滚到流入或流出统计。
  useLayoutEffect(() => {
    const target = params.get('scroll');
    if (!target || !open) return;
    const timer = window.setInterval(() => {
      const card = document.querySelector<HTMLElement>(target === 'outflow' ? '.site-usage' : '.nti-card');
      const body = document.querySelector<HTMLElement>('.node-usage-body');
      if (!card || !body || !document.querySelector('.ntf-box, .nti-bars')) return;
      window.clearInterval(timer);
      body.scrollTop += card.getBoundingClientRect().top - body.getBoundingClientRect().top - 12;
    }, 100);
    return () => window.clearInterval(timer);
  }, [option, open]);
  const focus = preset && { id: `${preset.tenant}/${preset.id}`, tenantId: preset.tenant, userId: preset.id };
  return (
    <>
      {nav && (
        <ReviewBar
          option={option}
          onOption={next => {
            setOption(next);
            setOpen(true);
          }}
          onReopen={open ? null : () => setOpen(true)}
        />
      )}
      {open &&
        (option === 'current' ? (
          <CurrentDialog
            key="current"
            nodeId={NODE_ID}
            name={NODE_NAME}
            country="HK"
            initialPeriod={period}
            onClose={() => setOpen(false)}
          />
        ) : (
          <NodeTrafficDialog
            key={option}
            nodeId={NODE_ID}
            name={NODE_NAME}
            country="HK"
            initialPeriod={period}
            option={option}
            initialFocus={focus}
            onClose={() => setOpen(false)}
          />
        ))}
    </>
  );
}

/* ══ 审阅外壳 ══ */

function ReviewBar({
  option,
  onOption,
  onReopen,
}: {
  option: Choice;
  onOption: (next: Choice) => void;
  onReopen: (() => void) | null;
}) {
  const reload = (name: string, value: string) => {
    const next = new URLSearchParams(location.search);
    if (value) next.set(name, value);
    else next.delete(name);
    location.search = next.toString();
  };
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <div className="mk-bar">
      <div className="mk-bar-row">
        <span className="mk-title">
          流出聚焦<small>MOCKUP</small>
        </span>
        <div className="mk-controls">
          <label className="mk-ctl">
            <span className="mk-ctl-label">方案</span>
            <SegmentedControl
              value={option}
              options={OPTIONS.map(item => ({ value: item.value, label: item.name }))}
              ariaLabel="方案"
              onChange={onOption}
            />
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">打开时聚焦</span>
            <select value={params.get('focus') ?? ''} onChange={event => reload('focus', event.target.value)}>
              <option value="">不聚焦</option>
              {DETAIL_USERS.map(item => (
                <option key={item.id} value={item.id}>
                  {item.id}
                </option>
              ))}
            </select>
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">主题</span>
            <select
              value={document.documentElement.dataset.theme}
              onChange={event => reload('theme', event.target.value)}
            >
              <option value="dark">深</option>
              <option value="light">浅</option>
            </select>
          </label>
          {onReopen && (
            <button type="button" className="btn sm" onClick={onReopen}>
              打开机器弹层
            </button>
          )}
        </div>
      </div>
      <p className="mk-scene-note">
        <b>{entry.name}</b>
        {entry.note}
      </p>
      {option !== 'current' && (
        <details className="mk-common">
          <summary>两个方案共同的改动</summary>
          <ol>
            {COMMON.map(item => (
              <li key={item}>{item}</li>
            ))}
          </ol>
        </details>
      )}
    </div>
  );
}

createRoot(document.getElementById('mk-root')!).render(
  <QueryClientProvider client={client}>
    <Mockup />
  </QueryClientProvider>,
);
