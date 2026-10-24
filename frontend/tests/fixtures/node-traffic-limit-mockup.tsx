// Mockup: machine config 「流量统计」 card with the 自动限速 thresholds drawn on the usage bar. Synthetic data only;
// never imported by the app. Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/node-traffic-limit-mockup.html?scene=limited&option=all
// Query: scene=<SCENES key>, option=a|b|c|d|all, role=system|readonly, edit=none|rules|enable|invalid|popover,
//        view=page|compare, theme=dark|light, w=<frame width>, nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { lazy, Suspense, useLayoutEffect, useMemo, useState, type ReactNode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { NodeAgentStateItem, NodeTrafficItem } from '../../src/api';
import { theme } from '../../src/forge/theme';
import { SegmentedControl } from '../../src/ui/bits';
import { Icon, PanelTitle } from '../../src/ui/icons';
import { machineQuotaTiB } from '../../src/ui/machine-traffic-quota';
import { RegionFlag } from '../../src/ui/region-flag';
import {
  agoText,
  exact,
  layerLabel,
  modeLabel,
  NOW,
  rateText,
  SCENE_KEYS,
  SCENES,
  untilText,
  type Scene,
  type SceneKey,
} from './node-traffic-limit-mockup-data';
import {
  AccountingRows,
  Directions,
  LayerNote,
  LayerSwitch,
  LimitSwitch,
  nextText,
  PinRunway,
  Problem,
  ReadingLine,
  Row,
  RuleList,
  SaveBar,
  Source,
  Staircase,
  StatusChip,
  StatusText,
  TickRunway,
  useCard,
  type Card,
  type Preset,
  type Role,
} from './node-traffic-limit-mockup-ui';
import '../../src/styles.css';
import './node-traffic-limit-mockup.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();

type Option = 'a' | 'b' | 'c' | 'd';
const OPTIONS: { value: Option; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 刻度跑道',
    note: '沿用现在的卡片结构。进度条在每一档阈值处切开，下方旗标写「阈值 速度」；越过阈值后的已用部分加斜纹。限速并入同一张表单，与计量设置共用一个保存。',
  },
  {
    value: 'b',
    name: 'B 限速阶梯',
    note: '跑道上方加一条阶梯线：横轴仍是本期用量，线的高度是允许的速度，越往右越低；圆点是当前位置。表单分「计量」「自动限速」两段，开关放在段标题上。',
  },
  {
    value: 'c',
    name: 'C 通栏双栏',
    note: '卡片放在配置页顶部、占满两栏：左边本期读数与跑道，右边限速状态（当前上限、各层回执、下一档）；下面两栏表单。窄于 720px 时上下排。',
  },
  {
    value: 'd',
    name: 'D 条上编辑',
    note: '规则直接画在进度条上：上方标签是速度，下方刻度是阈值。点标签就地改，右侧 ＋ 添加；表单里只剩开关与执行层。',
  },
];

/* ══ 方案 A：刻度跑道 ══ */

function CardA({ card }: { card: Card }) {
  const enabled = card.form.limit.enabled;
  return (
    <div className="panel config-panel node-traffic-accounting ntl-card ntl-a">
      <CardHeader card={card} />
      <div className="node-traffic-summary">
        <ReadingLine card={card} />
        <TickRunway card={card} />
        <Directions item={card.item} />
      </div>
      <div className="fgrid one node-traffic-settings">
        <AccountingRows card={card} />
        <Row k="自动限速">
          <span className="nd-ctl-line">
            <LimitSwitch card={card} />
            <StatusText card={card} />
          </span>
        </Row>
        {enabled && (
          <>
            <Row k="执行层">
              <LayerSwitch card={card} />
              <LayerNote card={card} />
            </Row>
            <Row k="规则">
              <RuleList card={card} />
            </Row>
          </>
        )}
      </div>
      <Problem card={card} />
      <SaveBar card={card} />
    </div>
  );
}

function CardHeader({ card, chip = true }: { card: Card; chip?: boolean }) {
  return (
    <header>
      <PanelTitle of="usage">流量统计</PanelTitle>
      <span className="sp" />
      {chip && <StatusChip status={card.status} />}
      <Source item={card.item} />
    </header>
  );
}

/* ══ 方案 B：限速阶梯 ══ */

function GroupHead({ title, status, control }: { title: string; status?: ReactNode; control?: ReactNode }) {
  return (
    <div className="ntl-group">
      <span className="ntl-group-t">{title}</span>
      {status}
      <span className="ntl-group-line" aria-hidden="true" />
      {control}
    </div>
  );
}

function CardB({ card }: { card: Card }) {
  const enabled = card.form.limit.enabled;
  return (
    <div className="panel config-panel node-traffic-accounting ntl-card ntl-b">
      <CardHeader card={card} />
      <div className="node-traffic-summary">
        <ReadingLine card={card} />
        <Staircase card={card} />
        <Directions item={card.item} />
      </div>
      <div className="fgrid one ntl-sections">
        <GroupHead title="计量" />
        <AccountingRows card={card} />
        <GroupHead
          title="自动限速"
          status={<StatusText card={card} time={false} />}
          control={<LimitSwitch card={card} />}
        />
        {enabled && (
          <>
            <Row k="执行层">
              <LayerSwitch card={card} />
              <LayerNote card={card} />
            </Row>
            <Row k="规则">
              <RuleList card={card} />
            </Row>
          </>
        )}
      </div>
      <Problem card={card} />
      <SaveBar card={card} />
    </div>
  );
}

/* ══ 方案 C：通栏双栏 ══ */

function CardC({ card }: { card: Card }) {
  const { item, used, end } = card;
  const quota = card.quota ?? null;
  const enabled = card.form.limit.enabled;
  const [figure, unit] = used === null ? ['—', ''] : exact(used).split(' ');
  return (
    <div className="panel config-panel node-traffic-accounting ntl-card ntl-c">
      <CardHeader card={card} chip={false} />
      <div className="ntl-c-top">
        <section className="ntl-c-read" aria-label="本期用量">
          <span className="ntl-c-k">
            本期已用
            <span className="ntl-c-tag">{modeLabel(item.accounting_mode)}</span>
          </span>
          <strong className={`ntl-c-value${used === null ? ' void' : ''}`}>
            {figure}
            {unit && <small>{unit}</small>}
            {quota !== null && <span className="of">/ {machineQuotaTiB(String(quota))} TiB</span>}
            {quota !== null && used !== null && (
              <span className={`pct${used > quota ? ' over' : ''}`}>{Math.round((used / quota) * 100)}%</span>
            )}
          </strong>
          <TickRunway card={card} />
          <div className="ntl-c-foot">
            <Directions item={item} />
            <span className="node-traffic-reset">
              {untilText(end - NOW)}后重置 · {new Date(end * 1000).toISOString().slice(5, 10)} UTC
            </span>
          </div>
        </section>
        <LimitTile card={card} />
      </div>
      <div className="ntl-c-forms">
        <div className="fgrid one">
          <GroupHead title="计量" />
          <AccountingRows card={card} />
        </div>
        <div className="fgrid one">
          <GroupHead title="自动限速" control={<LimitSwitch card={card} />} />
          {enabled && (
            <>
              <Row k="执行层">
                <LayerSwitch card={card} />
                <LayerNote card={card} />
              </Row>
              <Row k="规则">
                <RuleList card={card} />
              </Row>
            </>
          )}
        </div>
      </div>
      <Problem card={card} />
      <SaveBar card={card} />
    </div>
  );
}

/** 右栏：已保存策略的执行情况。大字是机器确认的当前上限；没有回执时写状态，不拿预测冒充确认。 */
function LimitTile({ card }: { card: Card }) {
  const { status, item } = card;
  const policy = item.limit?.policy;
  const active = card.savedSteps.find(step => step.active) ?? null;
  const off = !policy?.enabled && status.rate === null && status.text === '未启用';
  const ready = status.confirmed && status.text === '未达到阈值';
  const big = off ? '未启用' : status.rate !== null ? rateText(status.rate) : ready ? '不限速' : '—';
  const [figure, unit] = status.rate !== null ? big.split(' ') : [big, ''];
  const next = policy?.enabled && status.confirmed ? nextText({ ...card, steps: card.savedSteps }) : null;
  return (
    <section className={`ntl-tile${off ? ' off' : ''}`} aria-label="自动限速状态">
      <span className="ntl-c-k">
        自动限速
        {policy?.enabled && <span className="ntl-c-tag">{layerLabel(policy.layer)}</span>}
      </span>
      <strong className={`ntl-tile-rate${status.tone === 'gold' ? ' on' : ''}${status.rate === null && !ready ? ' dim' : ''}`}>
        {figure}
        {unit && <small>{unit}</small>}
      </strong>
      {!off && (
        <span className={`ntl-tile-sub${status.tone ? ` ${status.tone}` : ''}`}>
          {status.text}
          {status.text === '已限速' && active && ` · ${active.label} 起`}
          {status.reportedAgo !== null && status.confirmed && ` · ${agoText(status.reportedAgo)}`}
        </span>
      )}
      {status.layers.length > 0 && (
        <dl className="ntl-tile-layers">
          {status.layers.map(layer => (
            <div key={layer.layer} className={layer.tone ?? undefined}>
              <dt>{layer.name}</dt>
              <dd>
                {layer.text}
                {layer.rate !== null && ` ${rateText(layer.rate)}`}
                {layer.error && <em>{layer.error}</em>}
              </dd>
            </div>
          ))}
        </dl>
      )}
      {next && (
        <span className="ntl-tile-next">
          <span>下一档</span>
          {next}
        </span>
      )}
    </section>
  );
}

/* ══ 方案 D：条上编辑 ══ */

function CardD({ card, preset }: { card: Card; preset: Preset }) {
  const enabled = card.form.limit.enabled;
  const [open, setOpen] = useState<number | null>(
    preset === 'popover' && card.canEdit && card.form.limit.rules.length > 0 ? 0 : null,
  );
  return (
    <div className="panel config-panel node-traffic-accounting ntl-card ntl-d">
      <CardHeader card={card} />
      <div className="node-traffic-summary">
        <ReadingLine card={card} />
        <PinRunway card={card} open={open} setOpen={setOpen} />
        <Directions item={card.item} />
      </div>
      <div className="fgrid one node-traffic-settings">
        <AccountingRows card={card} />
        <Row k="自动限速">
          <span className="nd-ctl-line">
            <LimitSwitch card={card} />
            {enabled && <LayerSwitch card={card} />}
            <StatusText card={card} />
          </span>
          {enabled && <LayerNote card={card} />}
        </Row>
      </div>
      <Problem card={card} />
      <SaveBar card={card} />
    </div>
  );
}

/* ══ 单张卡：每次换场景、身份或预设都重新挂载，草稿从头开始 ══ */

function OptionCard({ option, scene, role, preset }: { option: Option; scene: Scene; role: Role; preset: Preset }) {
  const card = useCard(scene.item, role, preset);
  switch (option) {
    case 'a':
      return <CardA card={card} />;
    case 'b':
      return <CardB card={card} />;
    case 'c':
      return <CardC card={card} />;
    case 'd':
      return <CardD card={card} preset={preset} />;
  }
}

/* ══ 对照：生产里现在的卡片，同一份数据 ══ */

const LegacyCard = lazy(async () => {
  const module = await import('../../src/panes/nodes');
  return { default: module.TrafficAccountingCard };
});

function Legacy({ scene, role }: { scene: Scene; role: Role }) {
  const client = useMemo(() => {
    const created = new QueryClient({
      defaultOptions: {
        queries: { retry: false, staleTime: Infinity, refetchOnMount: false, refetchOnWindowFocus: false },
      },
    });
    /* 生产卡按真实时钟判断回执是否过期；把回执时间平移到此刻，与 mockup 的固定时钟对齐。 */
    const shift = Math.floor(Date.now() / 1000) - NOW;
    const item: NodeTrafficItem = scene.item.limit
      ? {
          ...scene.item,
          limit: {
            ...scene.item.limit,
            reported_at_unix_secs:
              scene.item.limit.reported_at_unix_secs === null ? null : scene.item.limit.reported_at_unix_secs + shift,
          },
        }
      : scene.item;
    created.setQueryData(['node-traffic'], { nodes: [item] });
    return created;
  }, [scene]);
  return (
    <QueryClientProvider client={client}>
      <Suspense fallback={<div className="panel config-panel">读取现状组件…</div>}>
        <LegacyCard
          node={{ node_id: scene.item.node_id, name: scene.item.name } as NodeAgentStateItem}
          canEdit={role === 'system'}
        />
      </Suspense>
    </QueryClientProvider>
  );
}

/* ══ 页面外壳：机器详情 → 配置页签 ══ */

function NodePage({ children }: { children: ReactNode }) {
  const tabs = [
    { key: 'observed', label: '观测', icon: 'observe' as const },
    { key: 'config', label: '配置', icon: 'config' as const },
    { key: 'chains', label: '规则', icon: 'chains' as const },
  ];
  return (
    <div className="nd-sheet nd-page nd-node-detail">
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head">
          <div className="nd-page-identity">
            <span className="nd-idplate">
              <span className="nd-idplate-clip">
                <RegionFlag code="HK" square />
              </span>
              <i className="node-lamp" aria-label="在线" />
            </span>
            <div className="nd-ident-text">
              <div className="nd-ident-row">
                <h1 className="nd-id">香港示例 01</h1>
              </div>
              <span className="nd-ident-meta">203.0.113.20</span>
            </div>
          </div>
          <div className="nd-tabs" role="tablist" aria-label="机器详情页签">
            <div className="nd-tabs-seg">
              {tabs.map(tab => (
                <button
                  key={tab.key}
                  type="button"
                  role="tab"
                  aria-selected={tab.key === 'config'}
                  tabIndex={tab.key === 'config' ? 0 : -1}
                >
                  <Icon of={tab.icon} size={14} className="nd-tab-ic" />
                  {tab.label}
                </button>
              ))}
            </div>
          </div>
        </header>
        <div className="nd-paper-body">{children}</div>
      </div>
    </div>
  );
}

/** 身份、DNS、WireGuard、证书组只为让改版卡落在真实的版面里，内容是静态的。 */
function IdentityStub({ canEdit }: { canEdit: boolean }) {
  return (
    <div className="panel config-panel">
      <header>
        <PanelTitle of="identity">身份</PanelTitle>
      </header>
      <div className="fgrid one">
        <Row k="名称">
          <input className="f mk-stub-field" defaultValue="香港示例 01" disabled={!canEdit} />
        </Row>
        <Row k="公网 IPv4">
          <input className="f mk-stub-field" defaultValue="203.0.113.20" disabled={!canEdit} />
        </Row>
      </div>
    </div>
  );
}

function DnsStub({ canEdit }: { canEdit: boolean }) {
  return (
    <div className="panel config-panel">
      <header>
        <PanelTitle of="dns">DNS</PanelTitle>
      </header>
      <div className="fgrid one">
        <Row k="服务器">
          <input className="f mk-stub-field" defaultValue="system" disabled={!canEdit} />
        </Row>
        <Row k="域名解析">
          <select className="f mk-stub-field" defaultValue="UseIPv6v4" disabled={!canEdit}>
            <option>UseIPv6v4</option>
          </select>
        </Row>
      </div>
    </div>
  );
}

function WgStub({ canEdit }: { canEdit: boolean }) {
  return (
    <div className="panel config-panel">
      <header>
        <PanelTitle of="tunnels">WIREGUARD</PanelTitle>
      </header>
      <div className="fgrid one">
        <Row k="监听端口">
          <input className="f mono mk-stub-field" defaultValue="51820" disabled={!canEdit} />
        </Row>
      </div>
    </div>
  );
}

function CertStub() {
  return (
    <div className="panel config-panel">
      <header>
        <PanelTitle of="certificate">证书组</PanelTitle>
      </header>
      <div className="fgrid one">
        <Row k="证书组">
          <span>默认</span>
        </Row>
      </div>
    </div>
  );
}

/* ══ 审阅外壳 ══ */

const WIDTHS = [
  { value: 1280, label: '1280' },
  { value: 760, label: '760' },
  { value: 390, label: '390' },
];
const ROLES: { value: Role; label: string }[] = [
  { value: 'system', label: '系统管理员' },
  { value: 'readonly', label: '只读管理员' },
];
const PRESETS: { value: Preset; label: string }[] = [
  { value: 'none', label: '未编辑' },
  { value: 'rules', label: '改第一档并加一档' },
  { value: 'enable', label: '开启' },
  { value: 'invalid', label: '清空额度（百分比无效）' },
  { value: 'popover', label: 'D：打开标签编辑' },
];
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

function OptionNote({ option }: { option: Option }) {
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <p className="mk-option-note">
      <b>方案 {entry.name}</b>
      {entry.note}
    </p>
  );
}

function App() {
  const [sceneKey, setSceneKey] = useState<SceneKey>(pick(params.get('scene'), SCENE_KEYS, 'limited'));
  const [option, setOption] = useState<Option | 'all'>(
    pick(params.get('option'), ['a', 'b', 'c', 'd', 'all'] as const, 'all'),
  );
  const [role, setRole] = useState<Role>(pick(params.get('role'), ['system', 'readonly'] as const, 'system'));
  const [preset, setPreset] = useState<Preset>(
    pick(params.get('edit'), ['none', 'rules', 'enable', 'invalid', 'popover'] as const, 'none'),
  );
  const [view, setView] = useState<'page' | 'compare'>(pick(params.get('view'), ['page', 'compare'] as const, 'page'));
  const [width, setWidth] = useState(Number(params.get('w')) || 1280);
  const [mode, setMode] = useState(theme.snapshot());
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('scene', sceneKey);
    next.set('option', option);
    next.set('role', role);
    next.set('edit', preset);
    next.set('view', view);
    next.set('w', String(width));
    next.set('theme', mode);
    history.replaceState(null, '', `?${next}`);
  }, [sceneKey, option, role, preset, view, width, mode]);
  const scene = SCENES[sceneKey];
  const canEdit = role === 'system';
  const stamp = `${sceneKey}-${role}-${preset}`;
  const cardOf = (value: Option) => (
    <OptionCard key={`${value}-${stamp}`} option={value} scene={scene} role={role} preset={preset} />
  );

  let body: ReactNode;
  if (option === 'all') {
    body = (
      <section className="nd-tab-config nd-tab-panel mk-gallery" role="tabpanel" aria-label="配置">
        {(['a', 'b'] as const).map(value => (
          <div key={value}>
            {nav && <OptionNote option={value} />}
            {cardOf(value)}
          </div>
        ))}
        <div className="ntl-span">
          {nav && <OptionNote option="c" />}
          {cardOf('c')}
        </div>
        <div>
          {nav && <OptionNote option="d" />}
          {cardOf('d')}
        </div>
        <div>
          {nav && (
            <p className="mk-option-note">
              <b>现状</b>生产组件 TrafficAccountingCard，同一份数据
            </p>
          )}
          <Legacy key={stamp} scene={scene} role={role} />
        </div>
      </section>
    );
  } else if (view === 'compare') {
    const wide = option === 'c';
    body = (
      <section className="nd-tab-config nd-tab-panel mk-compare" role="tabpanel" aria-label="配置">
        <div>
          {nav && (
            <p className="mk-option-note">
              <b>现状</b>生产组件 TrafficAccountingCard，同一份数据
            </p>
          )}
          <Legacy key={stamp} scene={scene} role={role} />
        </div>
        <div className={wide ? 'ntl-span' : undefined}>
          {nav && <OptionNote option={option} />}
          {cardOf(option)}
        </div>
      </section>
    );
  } else if (option === 'c') {
    body = (
      <section className="nd-tab-config nd-tab-panel" role="tabpanel" aria-label="配置">
        <div className="ntl-span">{cardOf('c')}</div>
        <div>
          <IdentityStub canEdit={canEdit} />
          <WgStub canEdit={canEdit} />
        </div>
        <div>
          <DnsStub canEdit={canEdit} />
          <CertStub />
        </div>
      </section>
    );
  } else {
    body = (
      <section className="nd-tab-config nd-tab-panel" role="tabpanel" aria-label="配置">
        <div>
          <IdentityStub canEdit={canEdit} />
          {cardOf(option)}
          <WgStub canEdit={canEdit} />
        </div>
        <div>
          <DnsStub canEdit={canEdit} />
          <CertStub />
        </div>
      </section>
    );
  }

  return (
    <>
      {nav && (
        <div className="mk-bar">
          <div className="mk-bar-row">
            <span className="mk-title">
              机器配置 · 流量统计 · 自动限速<small>MOCKUP · 现在 = 10-22 09:00 (UTC+8)</small>
            </span>
            <div className="mk-controls">
              <label className="mk-ctl">
                <span className="mk-ctl-label">场景</span>
                <select value={sceneKey} onChange={event => setSceneKey(event.target.value as SceneKey)}>
                  {SCENE_KEYS.map(key => (
                    <option key={key} value={key}>
                      {SCENES[key].label}
                    </option>
                  ))}
                </select>
              </label>
              <span className="mk-ctl">
                <span className="mk-ctl-label">方案</span>
                <SegmentedControl
                  ariaLabel="设计方案"
                  value={option}
                  options={[
                    { value: 'all' as const, label: '全部' },
                    { value: 'a' as const, label: 'A' },
                    { value: 'b' as const, label: 'B' },
                    { value: 'c' as const, label: 'C' },
                    { value: 'd' as const, label: 'D' },
                  ]}
                  onChange={setOption}
                />
              </span>
              <label className="mk-ctl">
                <span className="mk-ctl-label">身份</span>
                <select value={role} onChange={event => setRole(event.target.value as Role)}>
                  {ROLES.map(entry => (
                    <option key={entry.value} value={entry.value}>
                      {entry.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">编辑</span>
                <select value={preset} onChange={event => setPreset(event.target.value as Preset)}>
                  {PRESETS.map(entry => (
                    <option key={entry.value} value={entry.value}>
                      {entry.label}
                    </option>
                  ))}
                </select>
              </label>
              <span className="mk-ctl">
                <span className="mk-ctl-label">视图</span>
                <SegmentedControl
                  ariaLabel="视图"
                  value={view}
                  options={[
                    { value: 'page' as const, label: '页面' },
                    { value: 'compare' as const, label: '对照现状', disabled: option === 'all' },
                  ]}
                  onChange={setView}
                />
              </span>
              <span className="mk-ctl">
                <span className="mk-ctl-label">宽度</span>
                <SegmentedControl ariaLabel="画框宽度" value={width} options={WIDTHS} onChange={setWidth} />
              </span>
              <span className="mk-ctl">
                <span className="mk-ctl-label">主题</span>
                <SegmentedControl
                  ariaLabel="主题"
                  value={mode}
                  options={[
                    { value: 'dark', label: '暗' },
                    { value: 'light', label: '亮' },
                  ]}
                  onChange={next => {
                    if (theme.snapshot() !== next) theme.toggle();
                    setMode(next);
                  }}
                />
              </span>
            </div>
          </div>
          <p className="mk-scene-note">
            <b>{scene.label}</b>
            {scene.note}
          </p>
          {option !== 'all' && view === 'page' && (
            <p className="mk-scene-note">
              <b>方案 {OPTIONS.find(entry => entry.value === option)!.name}</b>
              {OPTIONS.find(entry => entry.value === option)!.note}
            </p>
          )}
        </div>
      )}
      <div className="mk-stage">
        <div className="mk-frame" style={{ width: `min(100%, ${width}px)` }}>
          <NodePage>{body}</NodePage>
        </div>
      </div>
    </>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
