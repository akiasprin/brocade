import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  createOperator,
  fetchOperators,
  fetchTenants,
  issueOperatorToken,
  resetOperatorPassword,
  revokeOperatorToken,
  type AdminOperator,
  type AdminRole,
} from '../api';
import { can, useSession } from '../session';
import { Ago, Empty, ErrorBox, Loading } from '../ui/bits';
import { copyText } from '../ui/platform';

// 五个角色等级，由高到低。该顺序即版面顺序——`can()` 使用序号比较
// （`RANK[role] >= RANK.editor`），五档严格递进，不存在「可发布但不可修改」这类组合。
// 因此页面按等级排列，不在表格中增加角色列要求自行比较。
//
// `adds` 写的是相对下一档新增的权限，而非该档的全部权限——递进关系用该方式表述
// 才不需要将基础权限重复五次。依据是 session.tsx 的 can() 及各页面的调用点。
const LADDER: { role: AdminRole; adds: string; where: string }[] = [
  { role: 'system-admin', adds: '改设置、节点身份与退役、建链、回滚', where: '外加下面所有档的' },
  { role: 'tenant-admin', adds: '建租户、管操作者', where: '管的是自己 scope 底下那棵子树' },
  { role: 'publisher', adds: '把草稿发出去', where: '发布要盖修订' },
  { role: 'editor', adds: '改用户、改链路', where: '写进草稿，不发布' },
  { role: 'readonly', adds: '只看', where: '' },
];

const ROLES: AdminRole[] = ['readonly', 'editor', 'publisher', 'tenant-admin', 'system-admin'];

export function OperatorsPane() {
  const { who } = useSession();
  const qc = useQueryClient();
  const operators = useQuery({ queryKey: ['operators'], queryFn: () => fetchOperators() });
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });

  const [form, setForm] = useState<{ id: string; role: AdminRole; scope: string; password: string }>({
    id: '',
    role: 'readonly',
    scope: '',
    password: '',
  });
  const [creating, setCreating] = useState(false);
  /* 一次性 token：签发后只保存在内存中直到被复制，不写入任何存储 */
  const [issued, setIssued] = useState<{ id: string; token: string } | null>(null);
  /* 代为设置的一次性密码，同样只保存在内存中 */
  const [issuedPassword, setIssuedPassword] = useState<{ id: string; password: string } | null>(null);

  const refresh = () => qc.invalidateQueries({ queryKey: ['operators'] });
  const create = useMutation({
    mutationFn: () =>
      createOperator({
        id: form.id.trim(),
        display_name: form.id.trim(),
        role: form.role,
        /* system-admin 不允许设置 scope，其他角色必须设置 */
        tenant_scope: form.role === 'system-admin' ? null : form.scope || defaultScope,
        /* 只有固定的 public readonly 账号允许免密。 */
        password: form.password || undefined,
      }),
    onSuccess: () => {
      refresh();
      setForm({ ...form, id: '', password: '' });
      setCreating(false);
    },
  });
  const resetPassword = useMutation({
    mutationFn: (id: string) => resetOperatorPassword(id),
    onSuccess: r => {
      setIssuedPassword({ id: r.operator_id, password: r.password });
      refresh();
    },
  });
  const issue = useMutation({
    mutationFn: (id: string) => issueOperatorToken(id),
    onSuccess: r => {
      setIssued({ id: r.operator_id, token: r.token });
      refresh();
    },
  });
  const revoke = useMutation({
    mutationFn: (id: string) => revokeOperatorToken(id),
    onSuccess: refresh,
  });

  if (operators.isPending) return <Loading />;
  if (operators.error) return <ErrorBox error={operators.error} />;

  const tenantOptions = [...(tenants.data?.tenants ?? [])].sort((a, b) => a.id.localeCompare(b.id));
  const defaultScope = tenantOptions.find(t => t.id === who.tenant_scope)?.id ?? tenantOptions[0]?.id ?? '';
  const manage = can(who.role, 'manage-tenants');
  const list = operators.data.operators;

  return (
    <>
      <div className="sh">
        <h4>操作者</h4>
        <span className="sub">密码用于登录控制台，token 用于脚本调用 API，两者互不依赖</span>
        <span className="sp" />
        <button className="btn" disabled={!manage} onClick={() => setCreating(!creating)}>
          {creating ? '收起' : '＋ 建操作者'}
        </button>
      </div>

      {creating && (
        <form
          className="newform"
          onSubmit={e => {
            e.preventDefault();
            const publicReadonly = form.id.trim() === 'public' && form.role === 'readonly';
            if (form.id.trim() && (publicReadonly || form.password.length >= 8)) create.mutate();
          }}
        >
          <p className="fh">新操作者</p>
          <div className="row">
            <input
              className="f"
              style={{ width: 150 }}
              placeholder="operator id"
              value={form.id}
              onChange={e => setForm({ ...form, id: e.target.value })}
            />
            <select
              className="f"
              value={form.role}
              onChange={e => setForm({ ...form, role: e.target.value as AdminRole })}
            >
              {ROLES.map(r => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
            {form.role === 'system-admin' ? (
              <span className="hint mono">scope —（system-admin 不带租户范围）</span>
            ) : (
              <select
                className="f"
                value={form.scope || defaultScope}
                onChange={e => setForm({ ...form, scope: e.target.value })}
              >
                {tenantOptions.map(t => (
                  <option key={t.id} value={t.id}>
                    {t.id}
                  </option>
                ))}
              </select>
            )}
            <input
              className="f"
              style={{ width: 170 }}
              type="password"
              autoComplete="new-password"
              placeholder={
                form.id.trim() === 'public' && form.role === 'readonly' ? '登录密码（可留空）' : '登录密码（至少 8 位）'
              }
              value={form.password}
              onChange={e => setForm({ ...form, password: e.target.value })}
            />
            <button
              className="btn primary"
              type="submit"
              disabled={
                !manage ||
                create.isPending ||
                !form.id.trim() ||
                (!(form.id.trim() === 'public' && form.role === 'readonly') && form.password.length < 8)
              }
            >
              {create.isPending ? '提交中…' : '建操作者'}
            </button>
          </div>
          <p className="hint" style={{ marginTop: 8 }}>
            只有固定的 <span className="mono">public</span> + <span className="mono">readonly</span> 账号可留空，
            用于访客页面；其他操作者必须设置至少 8 位密码。
          </p>
        </form>
      )}

      {(create.error || issue.error || revoke.error || resetPassword.error) && (
        <ErrorBox error={create.error ?? issue.error ?? revoke.error ?? resetPassword.error} />
      )}

      {/* 规则说明位于名单之前。角色是本页需要表达的核心内容，此前它只是表格中的一个字符串标签。 */}
      <div className="rulebox">
        <p className="rh">角色是一条线上的五档，不是五个平行的角色</p>
        <div className="rolestep">
          {[...LADDER].reverse().map(step => (
            <div
              key={step.role}
              className={[step.role === 'system-admin' ? 'top' : '', step.role === who.role ? 'mine' : '']
                .filter(Boolean)
                .join(' ')}
            >
              <div className="r">{step.role}</div>
              <div className="c">{step.role === 'readonly' ? step.adds : `＋ ${step.adds}`}</div>
              {step.where && <div className="cum">{step.where}</div>}
              {step.role === who.role && <div className="me">当前角色</div>}
            </div>
          ))}
        </div>
        <p className="note">
          高一档的角色包含低档的全部能力：<span className="mono">can()</span> 比较的是序号而非能力集合，因此不存在
          「能发布但不能修改」这样的组合。按角色隐藏入口只是<b>避免无效操作</b>，服务端每个写入口都会再鉴权一次。
        </p>
      </div>

      {issuedPassword && (
        <div className="callout warn">
          <b>{issuedPassword.id}</b> 的登录密码 —— 只显示这一次，库里只留 argon2 hash。 这个人原有的登录和 API token
          都已作废，得拿新密码重登；脚本需要时再重签 token：
          <div className="mono" style={{ overflowWrap: 'anywhere', color: 'var(--gold)', margin: '8px 0' }}>
            {issuedPassword.password}
          </div>
          <div className="toolbar">
            <button className="btn" onClick={() => void copyText(issuedPassword.password)}>
              复制
            </button>
            <span className="sp" />
            <button className="btn primary" onClick={() => setIssuedPassword(null)}>
              我抄好了
            </button>
          </div>
        </div>
      )}

      {issued && (
        <div className="callout warn">
          <b>{issued.id}</b> 的 API token —— 只显示这一次，库里只留 hash：
          <div className="mono" style={{ overflowWrap: 'anywhere', color: 'var(--gold)', margin: '8px 0' }}>
            {issued.token}
          </div>
          <div className="toolbar">
            <button className="btn" onClick={() => void copyText(issued.token)}>
              复制
            </button>
            <span className="sp" />
            <button className="btn primary" onClick={() => setIssued(null)}>
              我抄好了
            </button>
          </div>
        </div>
      )}

      {list.length === 0 ? (
        <Empty>还没有别的操作者。</Empty>
      ) : (
        LADDER.map(step => {
          const mine = list.filter(o => o.role === step.role);
          return (
            <div className={step.role === 'system-admin' ? 'op-rank top' : 'op-rank'} key={step.role}>
              <div className="rh">
                <span className="r">{step.role}</span>
                <span className="can">{step.role === 'readonly' ? step.adds : `＋ ${step.adds}`}</span>
                <span className="rule" />
                {/* 空档同样占一行：某个角色没有成员本身即是需要呈现的信息 */}
                <span className="n">{mine.length === 0 ? '没有人' : `${mine.length} 人`}</span>
              </div>
              {mine.map(o => (
                <OperatorCard
                  key={o.id}
                  op={o}
                  manage={manage}
                  busy={resetPassword.isPending || issue.isPending || revoke.isPending}
                  onResetPassword={() => resetPassword.mutate(o.id)}
                  onIssue={() => issue.mutate(o.id)}
                  onRevoke={() => revoke.mutate(o.id)}
                />
              ))}
            </div>
          );
        })
      )}
    </>
  );
}

// 一个操作者一张卡：身份 / 登录 / API token / 操作。
// 两套凭据并列显示而非作为相邻两列——某个操作者可能只具备其中一种，并列显示才能看出缺少哪一种。
function OperatorCard({
  op,
  manage,
  busy,
  onResetPassword,
  onIssue,
  onRevoke,
}: {
  op: AdminOperator;
  manage: boolean;
  busy: boolean;
  onResetPassword: () => void;
  onIssue: () => void;
  onRevoke: () => void;
}) {
  const live = Boolean(op.token_prefix) && !op.token_revoked_at;
  return (
    // 免密时整张卡着色。它不是中性状态：知道该 id 的任何人都可以用它登录，
    // 而 id 就显示在该卡的第一行。此前它是角落中的一个小标签，影响说明写在页尾。
    <div className={op.passwordless ? 'op-card risky' : 'op-card'}>
      <div className="op-who">
        <div className="id">
          {op.id}
          {op.display_name !== op.id && <span className="dim"> {op.display_name}</span>}
        </div>
        <div className="sc">{op.tenant_scope == null ? '全局 · 不带租户范围' : op.tenant_scope}</div>
      </div>

      <div className="cred">
        <div className="k">登录</div>
        <div className="v">
          {op.passwordless ? (
            <>
              <span className="st st-gold" title="没设密码，空密码即可登入">
                免密
              </span>
              <span className="dim">知道 id 就能登进来</span>
            </>
          ) : (
            <span className="st">密码</span>
          )}
        </div>
      </div>

      <div className="cred">
        <div className="k">API TOKEN</div>
        <div className="v">
          {op.token_revoked_at ? (
            <span className="st st-halted">已吊销</span>
          ) : op.token_prefix ? (
            <>
              <span className="mono">{op.token_prefix}…</span>
              <span className="dim">{op.token_last_used_at ? <Ago at={op.token_last_used_at} /> : '没用过'}</span>
            </>
          ) : (
            <span className="st st-pending">未签发</span>
          )}
        </div>
      </div>

      <div className="acts">
        <button className="btn sm" disabled={!manage || busy} onClick={onResetPassword}>
          {op.passwordless ? '设密码' : '重置密码'}
        </button>
        <button className="btn sm" disabled={!manage || busy} onClick={onIssue}>
          {live ? '重签' : '签发'}
        </button>
        {live && (
          <button className="btn sm danger" disabled={!manage || busy} onClick={onRevoke}>
            吊销
          </button>
        )}
      </div>
    </div>
  );
}
