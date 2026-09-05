import { useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { fetchCompileView, fetchRevisions, type AdminRole, type BrandingSettings, type Whoami } from '../api';
import { wm } from '../wm/store';
import { artifactPanel } from './artifact-panel';
import { useNarrow } from './viewport';
import { useWm } from './windows';
import { BrandIcon } from './branding';

export interface Tab {
  key: string;
  label: string;
  /* 默认对所有角色可见。按角色隐藏入口属于体验优化而非安全边界，写操作由服务端单独鉴权。 */
  roles?: AdminRole[];
}

export const TABS: Tab[] = [
  { key: 'nodes', label: '节点' },
  { key: 'chains', label: '链路' },
  { key: 'users', label: '用户与授权' },
  { key: 'tenants', label: '租户' },
  { key: 'deploy', label: '发布' },
  { key: 'usage', label: '用量' },
  { key: 'settings', label: '设置' },
];

// 底部 tab bar 只有四格——移动端从第五格开始标签会被压缩到不可读。
// 未进入的项（租户、设置）收入抽屉，不是移除。
const BOTTOM_KEYS = ['nodes', 'users', 'deploy', 'usage'];
/* 底部四格使用短标签：完整名称在 25% 屏宽下无法容纳 */
const SHORT_LABEL: Record<string, string> = { users: '用户' };

export function visibleTabs(who: Whoami): Tab[] {
  return TABS.filter(t => !t.roles || t.roles.includes(who.role));
}

/* 功能窗属于管理台面：在拓扑台面点击菜单时先切回管理台面，再执行置顶 */
export function openTab(tab: Tab) {
  wm.setFloor('desk');
  wm.open(`tab:${tab.key}`, tab.label);
}

export function openTabByKey(key: string) {
  const tab = TABS.find(t => t.key === key);
  if (tab) openTab(tab);
}

// 修订角标是全局状态的入口：显示当前修订和编译摘要，点击打开诊断窗。
// 窄屏无法容纳三段摘要，压缩为「R12 · 2 错」，警告数和可发布状态在诊断窗中查看。
function RevisionBadge() {
  const narrow = useNarrow();
  const revisions = useQuery({
    queryKey: ['revisions'],
    queryFn: () => fetchRevisions(),
    refetchInterval: 15_000,
  });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });

  const open = () => {
    wm.setFloor('desk');
    wm.open('diag', '诊断', { w: 560, h: 320 });
  };

  if (current === undefined) {
    return (
      <button id="revbadge" onClick={open}>
        <span className="dim">修订 …</span>
      </button>
    );
  }
  const summary = compile.data?.summary;
  if (narrow) {
    return (
      <button id="revbadge" onClick={open} title="点开诊断窗">
        <span>R{current}</span>
        {summary && <span className={summary.errors ? 'err' : 'ok'}>{summary.errors} 错</span>}
      </button>
    );
  }
  return (
    <button id="revbadge" onClick={open} title="点开诊断窗">
      <span>修订 {current}</span>
      {summary && (
        <>
          <span className={summary.errors ? 'err' : 'ok'}>{summary.errors} 错</span>
          <span className={summary.warnings ? 'warn' : 'ok'}>{summary.warnings} 警</span>
          <span className={summary.can_publish ? 'ok' : 'err'}>{summary.can_publish ? '可发布' : '禁发布'}</span>
        </>
      )}
    </button>
  );
}

/* 修改密码不进入 TABS：它不是功能页面，而是身份区的操作，且对所有角色开放。 */
function openPassword() {
  wm.setFloor('desk');
  wm.open('tab:password', '改密码', { w: 460, h: 300 });
}

export function Topbar({ branding, who, onLogout }: { branding: BrandingSettings; who: Whoami; onLogout: () => void }) {
  const snap = useWm();
  const narrow = useNarrow();
  const [sheet, setSheet] = useState(false);
  const active = snap.wins.find(w => w.id === snap.activeId);
  const chips = snap.wins.filter(w => w.min && w.home === snap.floor);
  const openKeys = new Set(snap.wins.map(w => w.key));

  return (
    <>
      <header id="bar">
        <div className="brand">
          <BrandIcon branding={branding} className="brand-logo" />
          <b>{branding.site_name}</b> <span>| 🛰️ 跨境网络小管家</span>
        </div>
        <div className="seg">
          <button aria-pressed={snap.floor === 'desk'} onClick={() => wm.setFloor('desk')}>
            管理
          </button>
          <button aria-pressed={snap.floor === 'topo'} onClick={() => wm.setFloor('topo')}>
            拓扑
          </button>
        </div>
        <nav id="menu" aria-label="功能菜单">
          {visibleTabs(who).map(t => {
            const key = `tab:${t.key}`;
            const pressed = active?.key === key && !active.min && active.home === snap.floor;
            return (
              <button
                key={t.key}
                className={openKeys.has(key) ? 'open' : ''}
                aria-pressed={pressed}
                onClick={() => openTab(t)}
              >
                {t.label}
              </button>
            );
          })}
        </nav>
        <span className="sp" />
        <button id="artbtn" className="btn" title="产物面板" onClick={() => artifactPanel.toggle()}>
          产物
        </button>
        <RevisionBadge />
        <div id="chips">
          {chips.map(c => (
            <button key={c.id} title="还原" onClick={() => wm.restore(c.id)}>
              ▣ {c.title}
            </button>
          ))}
        </div>
        <div className="who">
          <b>{who.operator_id}</b> · {who.role}
          <br />
          scope {who.tenant_scope ?? '—（全局）'}
        </div>
        <button className="btn" title="改自己的登录密码" onClick={openPassword}>
          改密码
        </button>
        <button className="btn" onClick={onLogout}>
          退出
        </button>
        {narrow && (
          <button id="hamburger" className="btn" aria-label="更多" aria-expanded={sheet} onClick={() => setSheet(true)}>
            ☰
          </button>
        )}
      </header>

      {narrow && sheet && <MoreSheet who={who} onClose={() => setSheet(false)} />}
      {narrow && <BottomTabs who={who} activeKey={active?.key} floor={snap.floor} />}
    </>
  );
}

/* 底部 tab bar：窄屏下由它承担窗口切换，因此功能窗的标题栏不再提供关闭按钮。 */
function BottomTabs({ who, activeKey, floor }: { who: Whoami; activeKey: string | undefined; floor: string }) {
  const tabs = BOTTOM_KEYS.map(k => visibleTabs(who).find(t => t.key === k)).filter((t): t is Tab => !!t);
  return (
    <nav id="tabbar" aria-label="主导航">
      {tabs.map(t => (
        <button key={t.key} aria-pressed={floor === 'desk' && activeKey === `tab:${t.key}`} onClick={() => openTab(t)}>
          {SHORT_LABEL[t.key] ?? t.label}
        </button>
      ))}
    </nav>
  );
}

// ☰ 抽屉：底部四格无法容纳的内容都在此——台面切换、其余功能页面、产物面板、身份。
// 退出不放在此处：顶栏已有一个入口，同一不可撤销操作提供两个入口会增加决策成本。
function MoreSheet({ who, onClose }: { who: Whoami; onClose: () => void }) {
  const rest = visibleTabs(who).filter(t => !BOTTOM_KEYS.includes(t.key));
  const go = (fn: () => void) => () => {
    fn();
    onClose();
  };
  return (
    <div id="sheet-wrap" role="dialog" aria-label="更多">
      <button className="sheet-scrim" aria-label="关闭" onClick={onClose} />
      <div className="sheet">
        <p className="eyebrow">台面</p>
        <div className="sheet-row">
          <button className="btn" onClick={go(() => wm.setFloor('desk'))}>
            管理台面
          </button>
          <button className="btn" onClick={go(() => wm.setFloor('topo'))}>
            拓扑台面
          </button>
        </div>

        {rest.length > 0 && (
          <>
            <p className="eyebrow">其他功能面</p>
            <div className="sheet-row">
              {rest.map(t => (
                <button key={t.key} className="btn" onClick={go(() => openTab(t))}>
                  {t.label}
                </button>
              ))}
            </div>
          </>
        )}

        <p className="eyebrow">产物</p>
        <div className="sheet-row">
          <button className="btn" onClick={go(() => artifactPanel.toggle())}>
            打开产物面板
          </button>
        </div>

        <p className="eyebrow">身份</p>
        <div className="sheet-who mono">
          <b>{who.operator_id}</b> · {who.role}
          <br />
          scope {who.tenant_scope ?? '—（全局）'}
        </div>
        <div className="sheet-row">
          <button className="btn" onClick={go(openPassword)}>
            改密码
          </button>
        </div>
      </div>
    </div>
  );
}
