import { useEffect, useState } from 'react';
import { useMutation, useQuery } from '@tanstack/react-query';
import { ApiError, fetchAuthState, fetchSessionWhoami, initAdmin, loginAdmin, type BrandingSettings } from '../api';
import type { Session } from '../app';
import { enterPublic } from '../session';

export function Login({ branding, onLogin }: { branding: BrandingSettings; onLogin: (session: Session) => void }) {
  const auth = useQuery({ queryKey: ['auth-state'], queryFn: fetchAuthState });
  const whoami = useQuery({
    queryKey: ['session-whoami'],
    queryFn: fetchSessionWhoami,
    retry: false,
  });
  useEffect(() => {
    if (whoami.data) onLogin({ who: whoami.data });
  }, [onLogin, whoami.data]);

  const initialized = auth.data?.initialized;

  return (
    <div className="fw active login-fw">
      <div className="fw-head">
        <span className="fw-kind">
          {initialized === undefined ? (auth.error ? '暂不可用' : '加载中') : initialized ? '登录' : '初始化'}
        </span>
        <span className="fw-title">{branding.site_name} | 跨境网络小管家</span>
      </div>
      <div className="fw-body">
        {auth.isPending ? (
          <p className="note">读取初始化状态…</p>
        ) : auth.error ? (
          <div className="callout err">{errorText(auth.error)}</div>
        ) : initialized ? (
          /* 控制面开启公开访问时，登录页需要提供返回路径：点击退出后才会进入该表单，
             而访客点击退出通常只是想关闭账号栏返回查看机器。 */
          <PasswordLogin onLogin={onLogin} publicOpen={auth.data?.public_open ?? false} />
        ) : (
          <InitializeAdmin onInitialized={onLogin} />
        )}
      </div>
    </div>
  );
}

export function PasswordLogin({ onLogin, publicOpen }: { onLogin: (session: Session) => void; publicOpen: boolean }) {
  const [operatorId, setOperatorId] = useState('root');
  const [password, setPassword] = useState('');
  const login = useMutation({
    mutationFn: () => loginAdmin({ operator_id: operatorId.trim(), password }),
    onSuccess: result => onLogin({ who: result.admin }),
  });
  const guest = useMutation({
    mutationFn: enterPublic,
    onSuccess: who => onLogin({ who }),
  });

  return (
    <form
      onSubmit={e => {
        e.preventDefault();
        // public readonly 账号是唯一允许空密码的身份，服务端会校验该约束。
        if (operatorId.trim()) login.mutate();
      }}
    >
      <p className="note">管理员和用户都直接使用用户名登录。公开访客可使用「访客模式」。</p>
      <label className="fieldline">
        <span>用户名</span>
        <input className="f" autoFocus value={operatorId} onChange={e => setOperatorId(e.target.value)} />
      </label>
      <label className="fieldline">
        <span>密码</span>
        <input className="f" type="password" value={password} onChange={e => setPassword(e.target.value)} />
      </label>
      {login.error && <div className="callout err">{loginError(login.error)}</div>}
      {guest.error && <div className="callout err">{loginError(guest.error)}</div>}
      <div className="toolbar">
        <span className="sp" />
        {/* 位于「登录」左侧：本页的主要操作是登录，访客模式是备用路径。
            type="button" 是必需的——form 内的按钮默认为 submit，不声明时
            点击「访客模式」会提交登录表单。 */}
        {publicOpen && (
          <button className="btn" type="button" disabled={guest.isPending} onClick={() => guest.mutate()}>
            {guest.isPending ? '进入中…' : '访客模式'}
          </button>
        )}
        <button className="btn primary" type="submit" disabled={login.isPending || !operatorId.trim()}>
          {login.isPending ? '登录中…' : '登录'}
        </button>
      </div>
    </form>
  );
}

export function InitializeAdmin({ onInitialized }: { onInitialized: (session: Session) => void }) {
  const [operatorId, setOperatorId] = useState('root');
  const [password, setPassword] = useState('');
  const [reveal, setReveal] = useState(false);
  // 显示名默认与用户名相同：初始化页面不应要求重复输入同一内容。两者在模型中仍然独立——
  // 用户名是主键，会被 revisions.author 和 deployment actor 引用，不可修改。内部归属使用
  // 固定初始值，单租户产品不把实现细节暴露为一个可选字段。
  const init = useMutation({
    mutationFn: () =>
      initAdmin({
        operator_id: operatorId.trim(),
        display_name: operatorId.trim(),
        password,
        root_tenant: 'platform',
      }),
    onSuccess: result => onInitialized({ who: result.admin }),
  });
  const tooShort = password.length > 0 && password.length < 8;

  return (
    <form
      onSubmit={e => {
        e.preventDefault();
        if (operatorId.trim() && password.length >= 8) init.mutate();
      }}
    >
      <p className="note">面板支持多用户统计；初始化时会预置使用者 zero，默认不开放登录。</p>
      <label className="fieldline">
        <span>用户名</span>
        <input className="f" autoFocus value={operatorId} onChange={e => setOperatorId(e.target.value)} />
      </label>
      <label className="fieldline">
        <span>密码</span>
        <span className="pw">
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
      {tooShort && <div className="callout err">密码至少 8 位</div>}
      {init.error && <div className="callout err">{errorText(init.error)}</div>}
      <p className="note dim">用户名创建后不可修改；访客模式默认关闭，可在设置中开启。</p>
      <div className="toolbar">
        <span className="sp" />
        <button
          className="btn primary"
          type="submit"
          disabled={init.isPending || !operatorId.trim() || password.length < 8}
        >
          {init.isPending ? '初始化中…' : '创建管理员'}
        </button>
      </div>
    </form>
  );
}

function loginError(error: unknown): string {
  if (error instanceof ApiError && error.status === 401) return '用户名或密码不正确';
  return errorText(error);
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
