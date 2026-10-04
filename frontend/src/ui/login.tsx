import { useEffect, useRef, useState, type ReactNode } from 'react';
import { useMutation, useQuery } from '@tanstack/react-query';
import {
  ApiError,
  fetchAuthState,
  fetchConsoleBootstrap,
  initAdmin,
  loginAdmin,
  loginUserDirect,
  type BrandingSettings,
} from '../api';
import type { Session } from '../app';
import { enterPublic } from '../session';
import { BrandIcon } from './branding';
import { PASSWORD_MIN_LENGTH } from './password-policy';

export interface DirectLoginCredentials {
  uuid: string;
  token: string;
}

/** Direct credentials stay in the URL fragment, which browsers never send in HTTP requests. */
export function parseDirectLoginHash(hash: string): DirectLoginCredentials | null {
  const parts = hash.replace(/^#\/?/, '').split('/');
  if (parts.length !== 3 || parts[0] !== 'login') return null;
  try {
    const uuid = decodeURIComponent(parts[1]).trim();
    const token = decodeURIComponent(parts[2]).trim();
    return uuid && token ? { uuid, token } : null;
  } catch {
    return null;
  }
}

export function directLoginUrl(uuid: string, token: string, currentUrl = window.location.href): string {
  const url = new URL(currentUrl);
  url.hash = `/login/${encodeURIComponent(uuid)}/${encodeURIComponent(token)}`;
  return url.toString();
}

export function DirectLogin({
  branding,
  credentials,
  onLogin,
  onUsePassword,
}: {
  branding: BrandingSettings;
  credentials: DirectLoginCredentials;
  onLogin: (session: Session) => void;
  onUsePassword: () => void;
}) {
  const attempted = useRef(false);
  const login = useMutation({
    mutationFn: async () => {
      await loginUserDirect(credentials);
      return fetchConsoleBootstrap();
    },
    onSuccess: onLogin,
  });

  useEffect(() => {
    if (attempted.current) return;
    attempted.current = true;
    // Clear both secrets before the request starts. Fragments never reach the HTTP server, but
    // leaving this one in place would retain it in copied URLs and the current history entry.
    window.history.replaceState(null, '', `${window.location.pathname}${window.location.search}#/users`);
    login.mutate();
  }, [login]);

  return (
    <LoginLayout branding={branding} title="直达登录">
      {login.isPending ? (
        <p className="login-note">正在验证直达页面…</p>
      ) : login.error ? (
        <div className="login-form">
          <div className="callout err">{directLoginError(login.error)}</div>
          <button className="btn primary login-submit" type="button" onClick={() => login.mutate()}>
            重试
          </button>
          <button className="login-alt" type="button" onClick={onUsePassword}>
            使用密码登录
          </button>
        </div>
      ) : (
        <p className="login-note">登录成功，正在打开用户页面…</p>
      )}
    </LoginLayout>
  );
}

export function Login({ branding, onLogin }: { branding: BrandingSettings; onLogin: (session: Session) => void }) {
  const auth = useQuery({ queryKey: ['auth-state'], queryFn: fetchAuthState });
  const initialized = auth.data?.initialized;

  const title = initialized === undefined ? (auth.error ? '暂不可用' : '加载中') : initialized ? '登录' : '初始化';

  return (
    <LoginLayout
      branding={branding}
      title={title}
      subtitle={initialized ? '管理员和用户都直接使用用户名登录' : undefined}
    >
      {auth.isPending ? (
        <p className="login-note">读取初始化状态…</p>
      ) : auth.error ? (
        <div className="callout err">{errorText(auth.error)}</div>
      ) : initialized ? (
        /* 控制面开启公开访问时，登录页需要提供返回路径：点击退出后才会进入该表单，
           而访客点击退出通常只是想关闭账号栏返回查看机器。 */
        <PasswordLogin onLogin={onLogin} publicOpen={auth.data?.public_open ?? false} />
      ) : (
        <InitializeAdmin onInitialized={onLogin} />
      )}
    </LoginLayout>
  );
}

/* 登录页版式：左侧品牌区（图标、字标、线路示意），右侧标题与表单，表单直接落在台面上。
   登录、初始化与直达登录共用。窄屏改为上下排列，品牌区只留图标与字标（见 styles.css）。 */
function LoginLayout({
  branding,
  title,
  subtitle,
  children,
}: {
  branding: BrandingSettings;
  title: string;
  subtitle?: string;
  children: ReactNode;
}) {
  return (
    <div className="login-page">
      <aside className="login-side">
        <div className="login-brand">
          <BrandIcon branding={branding} className="login-mark" />
          <span className="login-word">{branding.site_name}</span>
        </div>
        <LoginRoutes />
        <div className="login-tagline">跨境网络小管家</div>
      </aside>
      <main className="login-main">
        <div className="login-panel">
          <h1 className="login-title">
            {title}
            {subtitle && <small>{subtitle}</small>}
          </h1>
          {children}
        </div>
      </main>
    </div>
  );
}

/* 线路示意：六个地区节点，两个端点之间的一条线路高亮。只作装饰，读屏隐藏。
   每个节点带标签偏移，标签避开从该节点出发的连线。 */
const ROUTE_NODES = {
  CN: [70, 150, -26, 4],
  HK: [150, 212, -26, 18],
  JP: [238, 96, -8, -12],
  SG: [214, 262, 10, 16],
  US: [388, 132, 10, 16],
  DE: [330, 44, 10, -4],
} as const;
type RouteNode = keyof typeof ROUTE_NODES;
const ROUTE_LINKS: [RouteNode, RouteNode, boolean][] = [
  ['CN', 'HK', true],
  ['HK', 'JP', true],
  ['JP', 'US', true],
  ['CN', 'JP', false],
  ['HK', 'SG', false],
  ['SG', 'DE', false],
  ['JP', 'DE', false],
  ['SG', 'US', false],
];
const ROUTE_ENDS: RouteNode[] = ['CN', 'US'];

function LoginRoutes() {
  const curve = (from: RouteNode, to: RouteNode) => {
    const [x1, y1] = ROUTE_NODES[from];
    const [x2, y2] = ROUTE_NODES[to];
    return `M${x1},${y1} Q${(x1 + x2) / 2},${(y1 + y2) / 2 - Math.abs(x2 - x1) * 0.18} ${x2},${y2}`;
  };
  return (
    <svg className="login-routes" viewBox="0 0 440 300" aria-hidden="true">
      {ROUTE_LINKS.map(([from, to, hot]) => (
        <path key={`${from}-${to}`} className={hot ? 'lk hot' : 'lk'} d={curve(from, to)} />
      ))}
      {(Object.keys(ROUTE_NODES) as RouteNode[]).map(name => {
        const [x, y, dx, dy] = ROUTE_NODES[name];
        const end = ROUTE_ENDS.includes(name);
        return (
          <g key={name}>
            <circle className={end ? 'nd hot' : 'nd'} cx={x} cy={y} r={end ? 5 : 4} />
            <text x={x + dx} y={y + dy}>
              {name}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

export function PasswordLogin({ onLogin, publicOpen }: { onLogin: (session: Session) => void; publicOpen: boolean }) {
  const [operatorId, setOperatorId] = useState('root');
  const [password, setPassword] = useState('');
  const login = useMutation({
    mutationFn: async () => {
      await loginAdmin({ operator_id: operatorId.trim(), password });
      return fetchConsoleBootstrap();
    },
    onSuccess: result => onLogin(result),
  });
  const guest = useMutation({
    mutationFn: enterPublic,
    onSuccess: result => onLogin(result),
  });

  return (
    <form
      className="login-form"
      onSubmit={e => {
        e.preventDefault();
        // public readonly 账号是唯一允许空密码的身份，服务端会校验该约束。
        if (operatorId.trim()) login.mutate();
      }}
    >
      <label className="login-field">
        <span>用户名</span>
        <input className="f" autoFocus value={operatorId} onChange={e => setOperatorId(e.target.value)} />
      </label>
      <label className="login-field">
        <span>密码</span>
        <input className="f" type="password" value={password} onChange={e => setPassword(e.target.value)} />
      </label>
      {login.error && <div className="callout err">{loginError(login.error)}</div>}
      {guest.error && <div className="callout err">{loginError(guest.error)}</div>}
      <button className="btn primary login-submit" type="submit" disabled={login.isPending || !operatorId.trim()}>
        {login.isPending ? '登录中…' : '登录'}
      </button>
      {/* 位于「登录」下方：本页的主要操作是登录，访客模式是备用路径。
          type="button" 是必需的——form 内的按钮默认为 submit，不声明时
          点击「访客模式」会提交登录表单。 */}
      {publicOpen && (
        <button className="login-alt" type="button" disabled={guest.isPending} onClick={() => guest.mutate()}>
          {guest.isPending ? '进入中…' : '访客模式'}
        </button>
      )}
    </form>
  );
}

export function InitializeAdmin({ onInitialized }: { onInitialized: (session: Session) => void }) {
  const [bootstrapToken, setBootstrapToken] = useState('');
  const [operatorId, setOperatorId] = useState('root');
  const [password, setPassword] = useState('');
  const [reveal, setReveal] = useState(false);
  // 显示名默认与用户名相同：初始化页面不应要求重复输入同一内容。两者在模型中仍然独立——
  // 用户名是主键，会被 revisions.author 和 deployment actor 引用，不可修改。内部归属使用
  // 固定初始值，单租户产品不把实现细节暴露为一个可选字段。
  const init = useMutation({
    mutationFn: async () => {
      await initAdmin(bootstrapToken.trim(), {
        operator_id: operatorId.trim(),
        display_name: operatorId.trim(),
        password,
        root_tenant: 'platform',
      });
      return fetchConsoleBootstrap();
    },
    onSuccess: result => onInitialized(result),
  });
  const tooShort = password.length > 0 && password.length < PASSWORD_MIN_LENGTH;

  return (
    <form
      className="login-form"
      onSubmit={e => {
        e.preventDefault();
        if (bootstrapToken.trim() && operatorId.trim() && password.length >= PASSWORD_MIN_LENGTH) init.mutate();
      }}
    >
      <p className="login-lead">面板支持多用户统计；初始化时会预置使用者 zero，默认不开放登录。</p>
      <label className="login-field">
        <span>初始化凭据</span>
        <input
          className="f"
          type="password"
          autoComplete="one-time-code"
          autoFocus
          value={bootstrapToken}
          onChange={e => setBootstrapToken(e.target.value)}
        />
      </label>
      <p className="login-note">使用启动器提示的私有文件内容，或 BROCADE_BOOTSTRAP_TOKEN；成功后即失效。</p>
      <label className="login-field">
        <span>用户名</span>
        <input className="f" value={operatorId} onChange={e => setOperatorId(e.target.value)} />
      </label>
      <label className="login-field">
        <span>密码</span>
        <span className="login-pw">
          <input
            className="f"
            type={reveal ? 'text' : 'password'}
            value={password}
            onChange={e => setPassword(e.target.value)}
          />
          <button type="button" className="btn" onClick={() => setReveal(v => !v)} title="看一眼刚输的密码">
            {reveal ? '隐藏' : '显示'}
          </button>
        </span>
      </label>
      {tooShort && <div className="callout err">密码至少 {PASSWORD_MIN_LENGTH} 位</div>}
      {init.error && <div className="callout err">{initializationError(init.error)}</div>}
      <p className="login-note">用户名创建后不可修改；访客模式默认关闭，可在设置中开启。</p>
      <button
        className="btn primary login-submit"
        type="submit"
        disabled={
          init.isPending || !bootstrapToken.trim() || !operatorId.trim() || password.length < PASSWORD_MIN_LENGTH
        }
      >
        {init.isPending ? '初始化中…' : '创建管理员'}
      </button>
    </form>
  );
}

function loginError(error: unknown): string {
  if (error instanceof ApiError && error.status === 401) return '用户名或密码不正确';
  return errorText(error);
}

function directLoginError(error: unknown): string {
  if (error instanceof ApiError && error.status === 401) return '这个直达页面无效、已撤销或已被新链接替换';
  return errorText(error);
}

function initializationError(error: unknown): string {
  if (error instanceof ApiError && error.status === 401) return '初始化凭据不正确';
  if (error instanceof ApiError && error.status === 503) return '服务端尚未配置初始化凭据';
  return errorText(error);
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
