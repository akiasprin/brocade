import { useEffect, useState, useSyncExternalStore } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  DEFAULT_BRANDING,
  fetchAuthState,
  fetchBranding,
  fetchSessionWhoami,
  logoutAdmin,
  type BrandingSettings,
} from './api';
import { Login } from './ui/login';
import { Topbar, openTab, visibleTabs } from './ui/topbar';
import { WinLayer, useWm } from './ui/windows';
import { Pane } from './panes';
import { TopoCanvas } from './topo/canvas';
import { ArtifactPanel } from './panes/artifact';
import { artifactPanel } from './ui/artifact-panel';
import { autoPublicSuppressed, enterPublic, suppressAutoPublic, SessionProvider, type Session } from './session';
import { wm } from './wm/store';
import { ForgeShell } from './forge/shell';

export type { Session } from './session';

// 选择使用哪套外壳。
// `forge` 是编译台（三栏：模型 / 对象 / 产物），当前的默认值。
// `workbench` 是旧的工作台（浮动顶栏 + 可拖动窗口 + 拓扑台面），文件仍然保留，
// 修改该行即可切换回去——本仓库没有版本控制，回退路径需保留在代码中。
const SHELL: 'forge' | 'workbench' = 'forge';

/**
 * cookie 中没有会话时的备用路径：控制面启用了 public 免密账户时，自动以访客身份登录。
 *
 * 登录操作放在前端而非由服务端将无 cookie 的请求视为 public，是为了使访客和已登录用户
 * 走同一流程：都对应 admin_operators 中的一个身份、一个会话 cookie、一份 whoami。
 * 服务端因此不存在无身份请求这种状态，鉴权只有一处实现。
 *
 * 任一步骤失败时返回 null 并进入登录页——public 不存在、已设置密码、网络异常都是如此。
 */
async function restorePublic() {
  if (autoPublicSuppressed()) return null;
  try {
    const auth = await fetchAuthState();
    if (!auth.public_open) return null;
    return await enterPublic();
  } catch {
    return null;
  }
}

export function App() {
  const brandingQuery = useQuery({ queryKey: ['branding'], queryFn: fetchBranding, retry: false });
  const branding = brandingQuery.data ?? DEFAULT_BRANDING;
  /* 浏览器会话依赖 HttpOnly cookie；内存中只保存 whoami，用于菜单和权限提示。 */
  const [session, setSession] = useState<Session | null>(null);
  // 刷新后先用 cookie 恢复会话：恢复完成前渲染空白占位，避免短暂显示登录页。
  // 401 和网络错误同样进入登录页，只是不会先显示一帧表单。
  const [restoring, setRestoring] = useState(true);

  useEffect(() => {
    fetchSessionWhoami()
      .then(who => setSession({ who }))
      .catch(() => restorePublic().then(who => who && setSession({ who })))
      .finally(() => setRestoring(false));
  }, []);

  useEffect(() => {
    document.title = `${branding.site_name} · console`;
  }, [branding.site_name]);

  if (restoring) return <div id="stage" />;
  if (!session)
    return (
      <>
        <div id="stage" />
        <Login branding={branding} onLogin={setSession} />
      </>
    );

  const onLogout = async () => {
    // 退出后的下一帧即登录页，不能再被自动登录切回 public——否则在开放访问的控制面上
    // 退出不产生任何效果，无法显示登录表单。返回访客视角的路径是登录页的
    // 「访客模式」按钮（见 session.tsx 的 enterPublic）。
    suppressAutoPublic();
    // 必须等 logout 在服务端撤销会话后再清空本地状态：登录页挂载后会立即用当前 cookie
    // 请求 /whoami 尝试恢复会话（见 ui/login.tsx）。若此处不等待，cookie 尚未失效，
    // /whoami 返回访客身份，会把刚退出的访客立即登录回去——表现为点「登录」后停在访客
    // 视角，须手动刷新。等待后 /whoami 返回 401，登录表单才会停留。
    try {
      await logoutAdmin();
    } catch {
      /* 撤销请求失败（网络异常等）也继续进入登录页：用户已表达退出意图，cookie 可能已失效 */
    }
    setSession(null);
  };

  return (
    <SessionProvider value={session}>
      {SHELL === 'forge' ? (
        <ForgeShell branding={branding} session={session} onLogout={onLogout} />
      ) : (
        <>
          <div id="stage" />
          <Workbench branding={branding} session={session} onLogout={onLogout} />
        </>
      )}
    </SessionProvider>
  );
}

function Workbench({
  branding,
  session,
  onLogout,
}: {
  branding: BrandingSettings;
  session: Session;
  onLogout: () => void;
}) {
  useEffect(() => {
    wm.init(session.who.operator_id);
    const first = visibleTabs(session.who.role)[0];
    if (first) openTab(first);
  }, [session]);

  const snap = useWm();
  /* 面板展开时台面和画布需要让出宽度（不能被侧栏覆盖） */
  const panel = useSyncExternalStore(artifactPanel.subscribe, artifactPanel.snapshot);
  useEffect(() => {
    document.body.classList.toggle('has-artpanel', panel.open);
    return () => document.body.classList.remove('has-artpanel');
  }, [panel.open]);

  return (
    <>
      <Topbar branding={branding} who={session.who} onLogout={onLogout} />
      {snap.floor === 'topo' && <TopoCanvas />}
      <WinLayer render={win => <Pane win={win} />} />
      <ArtifactPanel />
    </>
  );
}
