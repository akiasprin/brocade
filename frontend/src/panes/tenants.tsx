import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { createTenant, fetchNodes, fetchOperators, fetchTenants, type AdminOperator } from '../api';
import { can, useSession } from '../session';
import { Empty, ErrorBox, Loading } from '../ui/bits';

// 判断某个操作者的范围是否覆盖某个租户。
// 与服务端 `brocade-store/src/admin.rs` 的 `tenant_in_scope` 一致：路径相等，或
// scope 之后紧跟一个点。段边界的判定不能省略——纯字符串前缀匹配会使
// `platformx` 落入 `platform` 的范围，而它属于另一棵树。
// system-admin 的 scope 为空但可访问全部（`can_access_tenant` 优先判断该角色），
// 而 scope 为空的非 system-admin 属于全局只读，无法访问任何具体租户。
const tenantInScope = (tenantId: string, scope: string) =>
  tenantId === scope || (tenantId.startsWith(scope) && tenantId[scope.length] === '.');

const covers = (op: AdminOperator, tenantId: string) =>
  op.role === 'system-admin' || (op.tenant_scope != null && tenantInScope(tenantId, op.tenant_scope));

// 租户路径采用 platform.acme.sub 这类前缀形式，可见性即前缀包含关系。
// 列表按路径排序后，子租户按段数缩进，即呈现为树形结构。
export function TenantsPane() {
  const { who } = useSession();
  const qc = useQueryClient();
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  // 展开一行需要的两份数据。获取失败时少显示一部分内容，不影响整页渲染——
  // readonly 角色打开本页时 /admin/operators 可能返回 403，这是预期的权限行为而非错误。
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), retry: false });
  const operators = useQuery({ queryKey: ['operators'], queryFn: () => fetchOperators(), retry: false });

  const [id, setId] = useState('');
  const [name, setName] = useState('');
  const [creating, setCreating] = useState(false);
  const [open, setOpen] = useState<string | null>(null);

  const create = useMutation({
    mutationFn: () => createTenant({ id: id.trim(), name: name.trim() || id.trim() }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['tenants'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      setId('');
      setName('');
      setCreating(false);
    },
  });

  if (tenants.isPending) return <Loading />;
  if (tenants.error) return <ErrorBox error={tenants.error} />;

  const list = [...tenants.data.tenants].sort((a, b) => a.id.localeCompare(b.id));
  const editable = can(who.role, 'manage-tenants');
  const allNodes = nodes.data?.nodes ?? [];
  const allOps = operators.data?.operators ?? [];
  /* 示意中的两行使用实际的根租户作为示例，比使用 example.com 更易对应 */
  const root = list[0]?.id ?? 'platform';

  return (
    <>
      <div className="sh">
        <h4>租户</h4>
        <span className="sub">路径即前缀，前缀即可见范围</span>
        <span className="sp" />
        <button className="btn" disabled={!editable} onClick={() => setCreating(!creating)}>
          {creating ? '收起' : '＋ 建租户'}
        </button>
      </div>

      {creating && (
        <form
          className="newform"
          onSubmit={e => {
            e.preventDefault();
            if (id.trim()) create.mutate();
          }}
        >
          <p className="fh">新租户</p>
          <div className="row">
            <input
              className="f"
              style={{ width: 220 }}
              placeholder={`租户路径，如 ${root}.acme`}
              value={id}
              onChange={e => setId(e.target.value)}
            />
            <input
              className="f"
              style={{ width: 160 }}
              placeholder="显示名"
              value={name}
              onChange={e => setName(e.target.value)}
            />
            <button className="btn primary" type="submit" disabled={!editable || create.isPending || !id.trim()}>
              {create.isPending ? '提交中…' : '建租户'}
            </button>
            <span className="hint">子租户为父路径加一段。写入先进入草稿。</span>
          </div>
        </form>
      )}

      {create.error && <ErrorBox error={create.error} />}

      {/* 规则说明位于数据之前。该块说明路径的构成方式及其约束范围——
          此前该内容是页尾的一行小字，而它是理解整页的前提。 */}
      <div className="rulebox">
        <p className="rh">租户树就是权限树</p>
        <div className="pathdemo">
          <span className="p">
            <span className="hl">{root}</span>
          </span>
          <span className="d">根租户</span>
          <span className="p">
            {root}.<span className="hl">acme</span>
          </span>
          <span className="d">子租户 = 父路径加一段</span>
          <span className="p">
            {root}.acme.<span className="hl">eu</span>
          </span>
          <span className="d">再往下一层</span>
          <span className="p out">{root}x</span>
          <span className="d">不属于 {root} 的子树：需按段对齐，仅字符串前缀相同不算</span>
        </div>
        <p className="note">
          一个操作者的 <span className="mono">scope</span> 覆盖<b>它自身那一段及其下的整棵子树</b>。scope 为{' '}
          <span className="mono">{root}</span> 时可见 <span className="mono">{root}.acme</span>
          ，反之不成立。system-admin 不带 scope，可见全部。
        </p>
        <p className="note">
          tenant-admin 管理的是<b>子树内的租户和操作者</b>，不含机器和控制面。
        </p>
      </div>

      {list.length === 0 ? (
        <Empty>
          还没有租户。路径用点分段，子租户是父路径加一段（<span className="mono">{root}.acme</span>）。
        </Empty>
      ) : (
        <table className="tbl cards">
          <thead>
            <tr>
              <th>租户路径</th>
              <th>名称</th>
              <th>节点</th>
              <th>用户</th>
              <th>操作者</th>
            </tr>
          </thead>
          <tbody>
            {list.map(t => {
              const depth = t.id.split('.').length - 1;
              const leaf = t.id.slice(t.id.lastIndexOf('.') + 1);
              const isOpen = open === t.id;
              // 直接归属于该租户的机器。不包含子树中的——那些属于子租户的机器，
              // 在子租户的行中展开时才与其计数对应。
              const mine = allNodes.filter(n => n.tenant_id === t.id);
              const reach = allOps.filter(o => covers(o, t.id));
              return [
                <tr
                  key={t.id}
                  className={isOpen ? 'tnt-row on' : 'tnt-row'}
                  onClick={() => setOpen(isOpen ? null : t.id)}
                >
                  <td className="mono" style={{ paddingLeft: 10 + depth * 18 }}>
                    {depth > 0 && <span className="dim">└ </span>}
                    <b>{leaf}</b>
                    {depth > 0 && <span className="dim"> · {t.id}</span>}
                  </td>
                  <td className="d2" data-label="名称">
                    {t.name}
                  </td>
                  <td className="mono" data-label="节点">
                    <span className="cnt">{t.node_count}</span>
                  </td>
                  <td className="mono" data-label="用户">
                    <span className="cnt">{t.user_count}</span>
                  </td>
                  <td className="mono" data-label="操作者">
                    <span className="cnt">{t.operator_count}</span>
                  </td>
                </tr>,
                isOpen && (
                  <tr key={`${t.id}:open`} className="tnt-open">
                    <td colSpan={5}>
                      <div className="in">
                        <div className="tnt-blk">
                          <p className="bh">谁的范围覆盖它</p>
                          {operators.error ? (
                            <p className="note dim" style={{ margin: 0 }}>
                              取不到操作者名单。
                            </p>
                          ) : reach.length === 0 ? (
                            <p className="note dim" style={{ margin: 0 }}>
                              没有操作者的范围覆盖到这里。
                            </p>
                          ) : (
                            <div className="chips">
                              {reach.map(o => (
                                <span className="chip" key={o.id}>
                                  {o.id}
                                  <span className="r">
                                    {o.role} ·{' '}
                                    {o.tenant_scope == null
                                      ? '全局'
                                      : o.tenant_scope === t.id
                                        ? '本租户'
                                        : `子树 ${o.tenant_scope}`}
                                  </span>
                                </span>
                              ))}
                            </div>
                          )}
                        </div>
                        <div className="tnt-blk">
                          <p className="bh">挂在这个租户名下的机器</p>
                          {mine.length === 0 ? (
                            <p className="note dim" style={{ margin: 0 }}>
                              没有机器直接归属于这一层{t.node_count > 0 && '（另有 ' + t.node_count + ' 台在子树中）'}。
                            </p>
                          ) : (
                            <div className="chips">
                              {mine.map(n => (
                                <span className="chip" key={n.node_id}>
                                  {n.node_id}
                                  {n.name && <span className="r">{n.name}</span>}
                                </span>
                              ))}
                            </div>
                          )}
                        </div>
                      </div>
                    </td>
                  </tr>
                ),
              ];
            })}
          </tbody>
        </table>
      )}
    </>
  );
}
