import { lazy, Suspense, useCallback, useEffect, useState } from 'react';
import { useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';
import {
  fetchAuthState,
  fetchBranding,
  fetchConsoleBootstrap,
  fetchNodes,
  fetchSnapshot,
  initialBranding,
  logoutAdmin,
  type AuthState,
  type BrandingSettings,
} from './api';
import { Login } from './ui/login';
import {
  autoPublicSuppressed,
  enterPublic,
  isVisitor,
  suppressAutoPublic,
  SessionProvider,
  type Session,
} from './session';
import { syncFavicon } from './ui/branding';
import { initialNodeDetailFromHash, initialPaneFromHash, preloadPaneForHash } from './panes/preload';
import { draft } from './draft';

type ForgeShellModule = typeof import('./forge/shell');

let forgeShellModule: Promise<ForgeShellModule> | undefined;
const loadForgeShell = () => (forgeShellModule ??= import('./forge/shell'));
const ForgeShell = lazy(() => loadForgeShell().then(module => ({ default: module.ForgeShell })));

/**
 * Seed high-frequency list routes whose complete initial dependency set is small and known here.
 * Starting these reads immediately after authentication lets them overlap the shell
 * and pane chunks; React Query shares the in-flight promise with the mounted page.
 */
function prefetchInitialRouteData(queryClient: QueryClient, hash: string, session: Session) {
  // Snapshot reads are generation-guarded. Restore the operator's draft before starting one, or
  // ForgeShell's later idempotent init would advance that generation and intentionally retry the
  // response we just prefetched.
  draft.init(session.who.operator_id);
  const pane = initialPaneFromHash(hash);
  if (pane === 'nodes') {
    void queryClient.prefetchQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
    const nodeId = initialNodeDetailFromHash(hash);
    if (nodeId) {
      // The nodes chunk is already being preloaded for this route. Reuse that import and start its
      // chart chunk plus bounded default-range reads while authentication UI is still settling.
      void import('./panes/nodes').then(module =>
        module.prefetchNodeDetailData(queryClient, nodeId, !isVisitor(session.who)),
      );
    }
    return;
  }
  if (pane === 'chains') {
    void queryClient.prefetchQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
    void queryClient.prefetchQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
    return;
  }
  if (pane === 'tunnels' && hash.replace(/^#\/?/, '').split('/').filter(Boolean).length === 1) {
    void queryClient.prefetchQuery({ queryKey: ['snapshot'], queryFn: fetchSnapshot });
  }
}

export type { Session } from './session';

/**
 * cookie 中没有会话时的备用路径：控制面启用了 public 免密账户时，自动以访客身份登录。
 *
 * 登录操作放在前端而非由服务端将无 cookie 的请求视为 public，是为了使访客和已登录用户
 * 走同一流程：都对应 admin_operators 中的一个身份、一个会话 cookie、一份 whoami。
 * 服务端因此不存在无身份请求这种状态，鉴权只有一处实现。
 *
 * 任一步骤失败时返回 null 并进入登录页——public 不存在、已设置密码、网络异常都是如此。
 */
async function restorePublic(auth: AuthState) {
  if (autoPublicSuppressed()) return null;
  try {
    if (!auth.public_open) return null;
    return await enterPublic();
  } catch {
    return null;
  }
}

export function App() {
  const queryClient = useQueryClient();
  const brandingQuery = useQuery({
    queryKey: ['branding'],
    queryFn: fetchBranding,
    retry: false,
    initialData: initialBranding,
    initialDataUpdatedAt: 0,
  });
  // A failed request is not evidence that the operator chose the product defaults.
  const branding = brandingQuery.data ?? { site_name: '控制台', icon_data_url: null };
  /* 浏览器会话依赖 HttpOnly cookie；内存中保存身份及首帧骨架所需的轻量统计。 */
  const [session, setSession] = useState<Session | null>(null);
  // 刷新后先用 cookie 恢复会话：恢复完成前只保留中性台面，避免短暂闪出登录表单。
  // 401 和网络错误同样进入登录页。
  const [restoring, setRestoring] = useState(true);
  // Query keys describe resources rather than identities. Clear them whenever a new identity
  // enters, otherwise an administrator's cached user list can survive into a public session.
  const onLogin = useCallback(
    (next: Session) => {
      const branding = queryClient.getQueryData<BrandingSettings>(['branding']);
      queryClient.clear();
      if (branding) queryClient.setQueryData(['branding'], branding);
      prefetchInitialRouteData(queryClient, window.location.hash, next);
      setSession(next);
    },
    [queryClient],
  );

  useEffect(() => {
    let active = true;
    // `/bootstrap`, the shell chunk and the current pane chunk are independent. Starting all three
    // here removes two avoidable network waterfalls without merging the split chunks.
    void loadForgeShell().catch(() => {
      forgeShellModule = undefined;
    });
    void preloadPaneForHash(window.location.hash).catch(() => undefined);
    // Start both reads together. A valid cookie may enter immediately when /bootstrap wins; a missing
    // cookie can reuse the already-running auth-state request instead of paying a second RTT.
    // fetchQuery also seeds Login's query, so falling through to the form does not fetch it again.
    const authState = queryClient
      .fetchQuery({ queryKey: ['auth-state'], queryFn: fetchAuthState, staleTime: 5_000, retry: false })
      .then(
        value => value,
        () => null,
      );
    const restore = async () => {
      try {
        const bootstrap = await fetchConsoleBootstrap();
        if (active) {
          prefetchInitialRouteData(queryClient, window.location.hash, bootstrap);
          setSession(bootstrap);
        }
      } catch {
        const auth = await authState;
        if (!active || !auth) return;
        const bootstrap = await restorePublic(auth);
        if (active && bootstrap) {
          prefetchInitialRouteData(queryClient, window.location.hash, bootstrap);
          setSession(bootstrap);
        }
      } finally {
        if (active) setRestoring(false);
      }
    };
    void restore();
    return () => {
      active = false;
    };
  }, [queryClient]);

  useEffect(() => {
    if (brandingQuery.data) {
      document.title = `${brandingQuery.data.site_name} | 跨境网络小管家`;
      syncFavicon(brandingQuery.data.icon_data_url);
    }
  }, [brandingQuery.data]);

  if (restoring || brandingQuery.isPending) return <div id="stage" />;
  if (!session)
    return (
      <>
        <div id="stage" />
        <Login branding={branding} onLogin={onLogin} />
      </>
    );

  const onLogout = async () => {
    // 退出后的下一帧即登录页，不能再被自动登录切回 public——否则在开放访问的控制面上
    // 退出不产生任何效果，无法显示登录表单。返回访客视角的路径是登录页的
    // 「访客模式」按钮（见 session.tsx 的 enterPublic）。
    suppressAutoPublic();
    // 必须等 logout 在服务端撤销会话后再清空本地状态：若界面先进入登录页，旧 cookie
    // 仍可能短暂授权同一页面上的并发请求，刷新也可能恢复刚退出的身份。等待服务端确认后，
    // 登录表单出现时便不存在“界面已退出、会话仍有效”的中间状态。
    try {
      await logoutAdmin();
    } catch {
      /* 撤销请求失败（网络异常等）也继续进入登录页：用户已表达退出意图，cookie 可能已失效 */
    }
    setSession(null);
    const currentBranding = queryClient.getQueryData<BrandingSettings>(['branding']);
    queryClient.clear();
    if (currentBranding) queryClient.setQueryData(['branding'], currentBranding);
  };

  return (
    <SessionProvider value={session}>
      <Suspense fallback={<div id="stage" />}>
        <ForgeShell branding={branding} session={session} onLogout={onLogout} />
      </Suspense>
    </SessionProvider>
  );
}
