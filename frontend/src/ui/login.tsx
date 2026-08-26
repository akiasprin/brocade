import { useEffect, useState } from 'react';
import { useMutation, useQuery } from '@tanstack/react-query';
import { ApiError, createTenantNow, fetchAuthState, fetchSessionWhoami, initAdmin, loginAdmin } from '../api';
import type { Session } from '../app';
import { enterPublic } from '../session';

export function Login({ onLogin }: { onLogin: (session: Session) => void }) {
  const auth = useQuery({ queryKey: ['auth-state'], queryFn: fetchAuthState });
  const whoami = useQuery({
    queryKey: ['session-whoami'],
    queryFn: fetchSessionWhoami,
    retry: false,
  });
  useEffect(() => {
    if (whoami.data) onLogin({ who: whoami.data });
  }, [onLogin, whoami.data]);

  const initialized = auth.data?.initialized ?? true;

  return (
    <div className="fw active login-fw">
      <div className="fw-head">
        <span className="fw-kind">{initialized ? '登录' : '初始化'}</span>
        <span className="fw-title">brocade console</span>
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

function PasswordLogin({ onLogin, publicOpen }: { onLogin: (session: Session) => void; publicOpen: boolean }) {
  const [operatorId, setOperatorId] = useState('admin');
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
      <p className="note">使用操作者用户名和密码登录。公开访客请使用旁边的「访客模式」。</p>
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

function InitializeAdmin({ onInitialized }: { onInitialized: (session: Session) => void }) {
  const [operatorId, setOperatorId] = useState('admin');
  const [rootTenant, setRootTenant] = useState('platform');
  const [password, setPassword] = useState('');
  const [reveal, setReveal] = useState(false);
  // 显示名默认与用户名相同：初始化页面不应要求重复输入同一内容。两者在模型中仍然独立——
  // 用户名是主键，会被 revisions.author 和 deployment actor 引用，不可修改；显示名只是
  // 展示名称，后续可在操作者页面随时修改。根租户也在该步骤创建：节点和用户都必须归属某个
  // 租户，缺少它无法纳管第一台机器，它与第一个管理员同属初始状态的组成部分。
  const init = useMutation({
    mutationFn: async () => {
      const result = await initAdmin({
        operator_id: operatorId.trim(),
        display_name: operatorId.trim(),
        password,
      });
      // 此时 session cookie 已存在，创建租户走正常的鉴权流程。
      // 创建失败不阻止进入——可在租户页面重新创建。
      const tenant = rootTenant.trim();
      if (tenant) {
        try {
          await createTenantNow({ id: tenant, name: tenant });
        } catch {
          /* 租户创建失败不视为初始化失败：进入后可在租户页面补充创建 */
        }
      }
      return { who: result.admin };
    },
    onSuccess: onInitialized,
  });
  const tooShort = password.length > 0 && password.length < 8;

  return (
    <form
      onSubmit={e => {
        e.preventDefault();
        if (operatorId.trim() && password.length >= 8) init.mutate();
      }}
    >
      <p className="note">数据库中还没有管理员。请先创建第一个 system-admin，之后此入口将关闭。</p>
      <label className="fieldline">
        <span>用户名</span>
        <input className="f" autoFocus value={operatorId} onChange={e => setOperatorId(e.target.value)} />
      </label>
      <label className="fieldline">
        <span>根租户</span>
        <input className="f" value={rootTenant} onChange={e => setRootTenant(e.target.value)} />
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
      <p className="note dim">用户名创建后不可修改，显示名可以随时修改。</p>
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
