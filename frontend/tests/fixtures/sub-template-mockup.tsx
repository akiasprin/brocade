// Mockup: 订阅模板可编辑——代理组改走指定线路，重点是漏网之鱼的兜底。Synthetic data only; never imported by the app.
// The production subscription dialog runs on a stubbed Console; each option replaces the 「模板」 line with an entry
// that opens its own template editor. 现状 is the untouched production dialog.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/sub-template-mockup.html?option=a
// Query: option=a|b|c|current, scene=tuned|fresh, open=0 keeps the editor closed, pick=<group id> opens that group's
//        menu (final, ai, youtube, …), theme=dark|light (read by the HTML before modules load), nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useLayoutEffect, useState, useSyncExternalStore } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { SubscriptionViewer } from '../../src/panes/subscription';
import { SegmentedControl } from '../../src/ui/bits';
import { GeneratedUserAvatar } from '../../src/ui/user-avatar';
import {
  GROUPS,
  SCENES,
  SELECTED_USER,
  type GroupId,
  type SceneKey,
  type TemplateConfig,
} from './sub-template-mockup-data';
import { TemplateEditor, TemplateLink, type Option } from './sub-template-mockup-ui';
import '../../src/styles.css';
import './sub-template-mockup.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

type Choice = Option | 'current';
const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 读数栏 · 列表',
    note: '最上面是三个关键分组的读数栏（节点选择、自动选择、漏网之鱼），写法同用户页读数栏：小字标签在上，去向在下。下面是服务分流、直连与拦截两张列表，每组一行。点去向弹出菜单，单选一条即完成，多条线路在菜单末尾。',
  },
  {
    value: 'b',
    name: 'B 读数栏 · 两列',
    note: '读数栏同 A；服务分流与直连与拦截并排成两列，同样的行更紧凑，整页不用滚动。',
  },
  {
    value: 'c',
    name: 'C 两栏',
    note: 'macOS 设置式：左栏按同样的顺序列出分组（三个关键分组在最上），右栏是所选分组的设置框——启用、切换方式、可拖动排序的去向、添加，「生成结果」按用户显示这一组的 YAML。',
  },
  { value: 'current', name: '现状', note: '生产版本：模板固定为 SubBoost 标准版，不能编辑。' },
];
const COMMON_NOTE =
  '共同：节点选择、自动选择、漏网之鱼固定在最上面；每个分组一种颜色的图形，透明底——系统色，或服务自己的品牌色，国内服务用国旗。(!) 说明与「计量口径」同一画法。模板全局一份，订阅只用该用户已授权的线路，一条都没有时这一组回到默认去向；未改动的组输出与现在逐字相同。改动用金点标出。';

/* ══ 合成后端 ══ */

const scene = pick<SceneKey>(params.get('scene'), Object.keys(SCENES) as SceneKey[], 'tuned');
const initialPick = (() => {
  const value = params.get('pick');
  return GROUPS.some(group => group.id === value) ? (value as GroupId) : null;
})();

globalThis.fetch = async input => {
  const path = String(input);
  if (path === `/users/preview/${SELECTED_USER}/clash-subscription`) {
    const url = 'https://sub.example/sub/v1/00000000-0000-4000-8000-000000000000/clash.yaml';
    const urls = { both: url, v4: `${url}?family=v4`, v6: `${url}?family=v6` };
    return Response.json({
      url,
      urls,
      template: 'SubBoost 标准版',
      haitun: { template: 'koipy 测速', status: 'active', urls, created_at: null, revoked_at: null },
      remaining_bytes: 490_000_000_000,
      reset_at: '2026-11-01T00:00:00+08:00',
      expires_at: null,
      usage_has_gap: false,
    });
  }
  return Response.json({ error: 'not in mockup' }, { status: 404 });
};

const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });

/* 订阅弹窗的「模板」一行在生产组件里；等它出现后把方案的入口放进去。 */
const slotStore = (() => {
  let slot: HTMLElement | null = null;
  const listeners = new Set<() => void>();
  const find = () => {
    const line = document.querySelector<HTMLElement>('.sub-item[aria-label="Clash 订阅"] .sub-item-sub');
    if (line === slot) return;
    slot = line;
    listeners.forEach(listener => listener());
  };
  new MutationObserver(find).observe(document.body, { childList: true, subtree: true });
  return {
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    get: () => slot,
  };
})();

function Mockup() {
  const [option, setOption] = useState<Choice>(pick<Choice>(params.get('option'), ['a', 'b', 'c', 'current'], 'a'));
  const [saved, setSaved] = useState<TemplateConfig>(SCENES[scene].template);
  const [editing, setEditing] = useState(params.get('open') !== '0' && option !== 'current');
  const [subOpen, setSubOpen] = useState(true);
  const slot = useSyncExternalStore(slotStore.subscribe, slotStore.get);
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    history.replaceState(history.state, '', `?${next}`);
    document.documentElement.dataset.mkOption = option;
  }, [option]);
  // 编辑模板时订阅弹窗让位：两层弹窗叠在一起，边缘会从编辑器两侧露出来。
  useLayoutEffect(() => {
    document.documentElement.dataset.mkEditing = editing && option !== 'current' ? 'true' : 'false';
  }, [editing, option]);
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
  return (
    <>
      {nav && (
        <ReviewBar
          option={option}
          onOption={next => {
            setOption(next);
            setEditing(next !== 'current');
          }}
          onReopen={subOpen ? null : () => setSubOpen(true)}
        />
      )}
      {subOpen && (
        <SubscriptionViewer
          tenant="preview"
          user={SELECTED_USER}
          kind="clash"
          identity={<GeneratedUserAvatar id={SELECTED_USER} variant="mini" />}
          onClose={() => setSubOpen(false)}
        />
      )}
      {slot &&
        option !== 'current' &&
        createPortal(<TemplateLink saved={saved} onOpen={() => setEditing(true)} />, slot)}
      {editing && option !== 'current' && (
        <TemplateEditor
          key={option}
          option={option}
          saved={saved}
          initialPick={initialPick}
          onSave={setSaved}
          onClose={() => setEditing(false)}
        />
      )}
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
    next.set(name, value);
    location.search = next.toString();
  };
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <div className="mk-bar">
      <div className="mk-bar-row">
        <span className="mk-title">
          订阅模板 · 代理组线路<small>MOCKUP</small>
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
              {(Object.keys(SCENES) as SceneKey[]).map(key => (
                <option key={key} value={key}>
                  {SCENES[key].name}
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
            <button type="button" className="btn" onClick={onReopen}>
              打开订阅弹窗
            </button>
          )}
        </div>
      </div>
      <p className="mk-scene-note">
        <b>{entry.name}</b>
        {entry.note}
        {option !== 'current' && <span className="mk-common">{COMMON_NOTE}</span>}
      </p>
    </div>
  );
}

createRoot(document.getElementById('mk-root')!).render(
  <QueryClientProvider client={client}>
    <Mockup />
  </QueryClientProvider>,
);
