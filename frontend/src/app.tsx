import { lazy } from './ui/lazy';
import { Suspense, useCallback, useEffect, useState } from 'react';
import { useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';
import {
  fetchAuthState,
  fetchBranding,
  fetchConsoleBootstrap,
  fetchNodes,
  fetchSnapshot,
  initialBranding,
  logoutAdmin,
  type BrandingSettings,
} from './api';
import { DirectLogin, Login, parseDirectLoginHash, parseInitializationHash } from './ui/login';
import {
  announceSessionChange,
  autoPublicSuppressed,
  isPublic,
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
      void import('./panes/nodes')
        .then(module => module.prefetchNodeDetailData(queryClient, nodeId, !isVisitor(session.who)))
        .catch(() => undefined);
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
  const [directCredentials, setDirectCredentials] = useState(() => parseDirectLoginHash(window.location.hash));
  // A direct credential must take precedence over any cookie already in this browser. Remember the
  // startup choice even if the user later falls back to the password form; do not restore the old
  // identity after explicitly leaving a failed direct login.
  const [restoreExistingSession] = useState(directCredentials === null);
  /* 登录账号使用 HttpOnly cookie，访客由服务端按当前开关解析；内存保存界面身份。 */
  const [session, setSession] = useState<Session | null>(null);
  useEffect(() => {
    if (session && parseInitializationHash(window.location.hash)) {
      window.history.replaceState(null, '', `${window.location.pathname}${window.location.search}`);
    }
  }, [session]);
  // 刷新后先用 cookie 恢复会话：恢复完成前只保留中性台面，避免短暂闪出登录表单。
  // 401 和网络错误同样进入登录页。
  const [restoring, setRestoring] = useState(restoreExistingSession);
  // Query keys describe resources rather than identities. Clear them whenever a new identity
  // enters, otherwise an administrator's cached user list can survive into a public session.
  const onLogin = useCallback(
    (next: Session) => {
      const branding = queryClient.getQueryData<BrandingSettings>(['branding']);
      queryClient.clear();
      if (branding) queryClient.setQueryData(['branding'], branding);
      announceSessionChange();
      prefetchInitialRouteData(queryClient, window.location.hash, next);
      setSession(next);
    },
    [queryClient],
  );

  useEffect(() => {
    if (!restoreExistingSession) return;
    let active = true;
    // `/bootstrap`, the shell chunk and the current pane chunk are independent. Starting all three
    // here removes two avoidable network waterfalls without merging the split chunks.
    void loadForgeShell().catch(() => {
      forgeShellModule = undefined;
    });
    void preloadPaneForHash(window.location.hash).catch(() => undefined);
    // Bootstrap resolves both signed-in accounts and stateless visitors. Seed Login's auth-state
    // query in parallel so a closed visitor switch or explicit sign-out can show the form promptly.
    void queryClient
      .fetchQuery({ queryKey: ['auth-state'], queryFn: fetchAuthState, staleTime: 5_000, retry: false })
      .catch(() => undefined);
    const restore = async () => {
      try {
        const bootstrap = await fetchConsoleBootstrap();
        if (isPublic(bootstrap.who) && autoPublicSuppressed()) return;
        if (active) {
          prefetchInitialRouteData(queryClient, window.location.hash, bootstrap);
          setSession(bootstrap);
        }
      } catch {
        // Missing, expired or invalid credentials return to Login; visitors need no login retry.
      } finally {
        if (active) setRestoring(false);
      }
    };
    void restore();
    return () => {
      active = false;
    };
  }, [queryClient, restoreExistingSession]);

  useEffect(() => {
    if (brandingQuery.data) {
      document.title = brandingQuery.data.site_name;
      syncFavicon(brandingQuery.data.icon_data_url);
    }
  }, [brandingQuery.data]);

  if (brandingQuery.isPending) return <div id="stage" />;
  if (directCredentials)
    return (
      <>
        <div id="stage" />
        <DirectLogin
          branding={branding}
          credentials={directCredentials}
          onLogin={next => {
            setDirectCredentials(null);
            onLogin(next);
          }}
          onUsePassword={() => {
            suppressAutoPublic();
            void logoutAdmin().finally(() => setDirectCredentials(null));
          }}
        />
      </>
    );
  if (restoring) return <div id="stage" />;
  if (!session)
    return (
      <>
        <div id="stage" />
        <Login branding={branding} onLogin={onLogin} />
      </>
    );

  const onLogout = async () => {
    // 退出后的下一帧即登录页，不能再自动切回访客——否则在开放访问的控制面上
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
    announceSessionChange();
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
