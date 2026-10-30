// Mockup: 机器用量弹层 → 流入统计 → 点用户打开的「流入明细」重新设计。Synthetic data only; never imported by the app.
// The production machine usage dialog runs on a stubbed Console. In options A–C a click on a user in the 流入统计
// card opens the mockup dialog instead of the production one; 现状 lets the production dialog open.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/inflow-details-mockup.html?option=a
// Query: option=a|b|c|current, scene=typical|mobile|single|degraded|empty|error, user=<user id>,
//        period=day|week|month, theme=dark|light (read by the HTML before modules load), open=0 keeps the details
//        closed, nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useMemo, useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { SiteUsagePeriod } from '../../src/api';
import { NodeTrafficDialog, trafficSources } from '../../src/node-traffic';
import { SegmentedControl } from '../../src/ui/bits';
import {
  chainColor,
  DETAIL_SCENES,
  DETAIL_USERS,
  installBackend,
  NODE_ID,
  NODE_NAME,
  selectDetailScene,
  snapshotOf,
  type DetailScene,
} from './inflow-details-mockup-data';
import { InflowDetailsMockup, type Option } from './inflow-details-mockup-ui';
import '../../src/styles.css';
import './inflow-details-mockup.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

type Choice = Option | 'current';
const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 按接入链分组',
    note: '每条接入链一组：组头是链名、来源数、合计与占比，组内每个来源 IP 一行——地址、网络归属、条形、流量、占比。条形全弹窗共用一把尺子，用这条链在流入统计里的颜色；未记录来源与端口转发用灰色，排在组末。一组超过 6 行时折叠其余。',
  },
  {
    value: 'b',
    name: 'B 来源排行',
    note: '先看组成：按接入链着色的组成条，下面是与流入统计相同的图例，点一条链只看它的来源。下面每个来源 IP 一行，跨链使用的地址只出现一次，条形按链分段；未记录来源、端口转发排在最后。超过 11 行时折叠其余。',
  },
  {
    value: 'c',
    name: 'C 流向图',
    note: '与流入统计的流向图同一画法：左边是来源 IP（前 8 个，其余合为一行，点开展开），右边是接入链，带宽与流量成正比；指向任一侧时其余降到一半不透明度，不再淡到看不清。弹窗窄于 640px 时换成 B 的列表。',
  },
  {
    value: 'current',
    name: '现状',
    note: '生产版本：表格每行重复用户与接入链，手机上流量各列在横向滚动之外；读数与表尾合计重复，底部是说明段落和明细条数。',
  },
];
const COMMON = [
  '弹窗头：头像和用户名作标题，后面是「机器 · 流入明细」；时间范围移进读数栏的标签，不再单占一条。',
  '读数栏与用户页「用量与额度」同一写法：合计大字、上下行组成条与数值，右侧是来源 IP 与接入链数。去掉表尾合计、「N 条明细」和底部说明段落。',
  '来源与用户页「在线设备」同一写法：IPv6 接口标识浅一档，复制钮悬停出现、触屏常显；新增网络归属（本地 GeoIP 的国家／地区与中国运营商）。需要 Console 给 inflow-details 加上 source_countries / source_operators，复用在线设备的同一查询。',
  '一个「合计 / 下行 / 上行」分段控件代替三列排序：排序、条形、数值与占比一起切换。',
  '去掉「用户」列和逐行重复的接入链名。未归属来源改称「未记录来源」，端口转发写「不区分来源」，悬停各有一句解释。',
  '完整性只在出现时写一行：来源记录不完整、采集缺口；与用量账本不一致时点 (!) 展开账本值。',
  '手机宽度不再横向滚动：每个来源排成两行，流量始终可见。',
];
const PERIODS = [
  { value: 'day' as const, label: '当天' },
  { value: 'week' as const, label: '当周' },
  { value: 'month' as const, label: '当月' },
];
const PERIOD_LABELS = new Map<string, SiteUsagePeriod>(PERIODS.map(item => [item.label, item.value]));

/* ══ 合成后端 ══ */

const scene = pick<DetailScene>(
  params.get('scene'),
  DETAIL_SCENES.map(item => item.value),
  'typical',
);
selectDetailScene(scene);
installBackend();
const snapshot = snapshotOf();
const sourceOf = trafficSources(snapshot);
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
client.setQueryData(['snapshot'], snapshot);

const userOf = (id: string | null) => DETAIL_USERS.find(user => user.id === id) ?? null;

function Mockup() {
  const [option, setOption] = useState<Choice>(pick<Choice>(params.get('option'), ['a', 'b', 'c', 'current'], 'a'));
  const [period, setPeriod] = useState<SiteUsagePeriod>(
    pick<SiteUsagePeriod>(params.get('period'), ['day', 'week', 'month'], 'day'),
  );
  const [user, setUser] = useState(userOf(params.get('user'))?.id ?? DETAIL_USERS[0].id);
  const [open, setOpen] = useState(params.get('open') !== '0');
  const [usageOpen, setUsageOpen] = useState(true);
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    next.set('period', period);
    next.set('user', user);
    history.replaceState(history.state, '', `?${next}`);
    document.documentElement.dataset.mkOption = option;
  }, [option, period, user]);
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
  // 方案 A–C：点流入统计里的用户时打开方案的明细，挡住生产弹窗；统计范围以机器弹层头部为准。
  useEffect(() => {
    if (option === 'current') return;
    const intercept = (event: MouseEvent) => {
      const trigger = (event.target as Element | null)?.closest<HTMLElement>('.nti-card [aria-haspopup="dialog"]');
      const name = trigger?.getAttribute('aria-label')?.split(' · ')[0] ?? null;
      if (!trigger || !userOf(name)) return;
      event.stopPropagation();
      event.preventDefault();
      const pressed = document.querySelector('.node-usage-period [aria-pressed="true"]')?.textContent ?? '';
      setPeriod(PERIOD_LABELS.get(pressed) ?? 'day');
      setUser(name!);
      setOpen(true);
    };
    window.addEventListener('click', intercept, true);
    return () => window.removeEventListener('click', intercept, true);
  }, [option]);
  // 现状：等流入统计画出来后点一下所选用户，打开生产弹窗。
  useEffect(() => {
    if (option !== 'current' || !open || !usageOpen) return;
    let done = false;
    const timer = window.setInterval(() => {
      const trigger = document.querySelector<HTMLElement>(
        `.nti-card [aria-haspopup="dialog"][aria-label^="${CSS.escape(user)} · "]`,
      );
      if (!trigger || done) return;
      done = true;
      window.clearInterval(timer);
      trigger.click();
    }, 100);
    return () => window.clearInterval(timer);
  }, [option, open, usageOpen, user, period]);
  const selected = userOf(user)!;
  const colorOf = useMemo(() => chainColor, []);
  return (
    <>
      {nav && (
        <ReviewBar
          option={option}
          onOption={next => {
            setOption(next);
            setOpen(true);
          }}
          period={period}
          onPeriod={setPeriod}
          user={user}
          onUser={next => {
            setUser(next);
            setOpen(true);
          }}
          onReopen={usageOpen ? null : () => setUsageOpen(true)}
        />
      )}
      {usageOpen && (
        <NodeTrafficDialog
          key={`${option === 'current' ? `${user}/` : ''}${period}`}
          nodeId={NODE_ID}
          name={NODE_NAME}
          country="HK"
          initialPeriod={period}
          onClose={() => setUsageOpen(false)}
        />
      )}
      {usageOpen && open && option !== 'current' && (
        <InflowDetailsMockup
          key={`${option}/${user}/${period}`}
          option={option}
          nodeId={NODE_ID}
          nodeName={NODE_NAME}
          tenantId={selected.tenant}
          userId={selected.id}
          period={period}
          sourceOf={sourceOf}
          colorOf={colorOf}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  );
}

/* ══ 审阅外壳 ══ */

function ReviewBar({
  option,
  onOption,
  period,
  onPeriod,
  user,
  onUser,
  onReopen,
}: {
  option: Choice;
  onOption: (next: Choice) => void;
  period: SiteUsagePeriod;
  onPeriod: (next: SiteUsagePeriod) => void;
  user: string;
  onUser: (next: string) => void;
  onReopen: (() => void) | null;
}) {
  const reload = (name: string, value: string) => {
    const next = new URLSearchParams(location.search);
    next.set(name, value);
    location.search = next.toString();
  };
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <div className="mk-bar">
      <div className="mk-bar-row">
        <span className="mk-title">
          流入明细<small>MOCKUP</small>
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
            <span className="mk-ctl-label">场景</span>
            <select value={scene} onChange={event => reload('scene', event.target.value)}>
              {DETAIL_SCENES.map(item => (
                <option key={item.value} value={item.value}>
                  {item.label}
                </option>
              ))}
            </select>
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">用户</span>
            <select value={user} onChange={event => onUser(event.target.value)}>
              {DETAIL_USERS.map(item => (
                <option key={item.id} value={item.id}>
                  {item.id}
                </option>
              ))}
            </select>
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">统计</span>
            <SegmentedControl value={period} options={PERIODS} ariaLabel="统计范围" onChange={onPeriod} />
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
          <summary>三个方案共同的改动</summary>
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
