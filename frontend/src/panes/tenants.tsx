import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { createTenant, fetchNodes, fetchOperators, fetchTenants, type AdminOperator } from '../api';
import { can, useSession } from '../session';
import { Empty, ErrorBox, Loading } from '../ui/bits';

const tenantInScope = (tenantId: string, scope: string) =>
  tenantId === scope || (tenantId.startsWith(scope) && tenantId[scope.length] === '.');

const covers = (admin: AdminOperator, tenantId: string) =>
  admin.id !== 'public' &&
  (admin.role === 'system-admin' || (admin.tenant_scope != null && tenantInScope(tenantId, admin.tenant_scope)));

// 租户路径采用 platform.acme.sub 这类前缀形式，可见性即前缀包含关系。
// 列表按路径排序后，子租户按段数缩进，即呈现为树形结构。
export function TenantsPane() {
  const { who } = useSession();
  const qc = useQueryClient();
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  // 展开一行需要机器与管理员名单。访客读不到管理员名单时只显示聚合数量。
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), retry: false });
  const admins = useQuery({ queryKey: ['operators'], queryFn: () => fetchOperators(), retry: false });

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
  const allAdmins = admins.data?.operators ?? [];
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
        <p className="note">租户路径用于归组机器和用户；管理员可管理自身范围及其下的完整子树。</p>
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
              <th>管理员</th>
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
              const reach = allAdmins.filter(admin => covers(admin, t.id));
              return [
                <tr
                  key={t.id}
                  className={isOpen ? 'tnt-row on' : 'tnt-row'}
                  tabIndex={0}
                  aria-expanded={isOpen}
                  onClick={() => setOpen(isOpen ? null : t.id)}
                  onKeyDown={event => {
                    if (event.key === 'Enter' || event.key === ' ') {
                      event.preventDefault();
                      setOpen(isOpen ? null : t.id);
                    }
                  }}
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
                  <td className="mono" data-label="管理员">
                    <span className="cnt">{t.operator_count}</span>
                  </td>
                </tr>,
                isOpen && (
                  <tr key={`${t.id}:open`} className="tnt-open">
                    <td colSpan={5}>
                      <div className="in">
                        <div className="tnt-blk">
                          <p className="bh">管理这个租户</p>
                          {admins.error ? (
                            <p className="note dim" style={{ margin: 0 }}>
                              管理员名单读取失败或当前角色不可见。
                            </p>
                          ) : reach.length === 0 ? (
                            <p className="note dim" style={{ margin: 0 }}>
                              还没有管理员。
                            </p>
                          ) : (
                            <div className="chips">
                              {reach.map(admin => (
                                <span className="chip" key={admin.id}>
                                  {admin.display_name || admin.id}
                                  <span className="r">{admin.role === 'system-admin' ? '全局管理员' : admin.tenant_scope}</span>
                                </span>
                              ))}
                            </div>
                          )}
                        </div>
                        <div className="tnt-blk">
                          <p className="bh">挂在这个租户名下的机器</p>
                          {nodes.error ? (
                            <p className="note bad" style={{ margin: 0 }}>
                              机器名单读取失败，不能据此判断这一层是否为空。
                            </p>
                          ) : mine.length === 0 ? (
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
