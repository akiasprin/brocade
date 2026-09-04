import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { fetchNodes, fetchRevisions, fetchTenants } from '../api';
import { useAgentLiveness } from '../ui/agent-alive';
import { useSession } from '../session';
import { ErrorBox, Loading } from '../ui/bits';
import { openTabByKey } from '../ui/topbar';
import { navigate } from '../forge/route';
import {
  fetchPreviewNodeLogs,
  previewProvisionNode,
  previewVerifySubscription,
  type PreviewProvisionNodeResult,
  type PreviewStatus,
} from './api';
import type { Drill } from '../panes/nodes';

// Preview 的纳管向导。
//
// 与生产流程（panes/nodes.tsx 的 Provision）版面结构相同、流程不同：
//
// - 版面：两种状态，不使用步骤条——机器尚未入库（一张表单），机器已入库但尚未上线
//   （容器执行脚本加一个状态指示）。使用同一套 `.wz-*` 组件。
// - 流程存在实际差异，不是重复实现：此处不填写公网 IP（容器 IP 由 preview 分配）、
//   安装由容器自动执行生产环境的安装脚本（显示日志，而非提供命令供手动执行）、
//   末尾增加一步订阅验证。
//
// 两侧共用的判断只有一项：agent 是否上线——两者读取同一份 `/nodes/agent-state`。
export type PreviewWizDrill =
  { p: 'provision'; step: number } | { p: 'install'; node: string; step: number; result?: PreviewProvisionNodeResult };

export function PreviewProvision({
  drill,
  go,
  status,
}: {
  drill: PreviewWizDrill;
  go: (d: Drill) => void;
  status: PreviewStatus;
}) {
  /* 存在 node 表示容器和库中的机器记录都已创建；result 只是创建时的响应 */
  const node = drill.p === 'install' ? drill.node : null;
  const result = drill.p === 'install' ? drill.result : undefined;
  if (node) return <PreviewInstall node={node} result={result} go={go} status={status} />;
  return <PreviewForm go={go} status={status} />;
}

// 环境状态行：网络、子网、镜像、agent 二进制是否就绪。两种状态下都显示——
// preview 与生产环境的界面越接近，越需要有位置标明当前处于 preview 环境。
function PreviewBanner({ status }: { status: PreviewStatus }) {
  return (
    <div className="callout blue" style={{ marginBottom: 14 }}>
      <b>Preview 模式</b>：自动分配容器 IP，在 Docker 容器里跑生产安装脚本。
      <div className="note mono" style={{ marginTop: 5 }}>
        {status.network} · {status.subnet} · {status.subnet_ipv6} · {status.node_image} · agent{' '}
        {status.agent_binary_ready ? 'ready' : 'auto-build'}
      </div>
    </div>
  );
}

/* 第一种状态：机器尚未入库。 */
function PreviewForm({ go, status }: { go: (d: Drill) => void; status: PreviewStatus }) {
  const qc = useQueryClient();
  const { who } = useSession();
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  const options = [...(tenants.data?.tenants ?? [])].sort((a, b) => a.id.localeCompare(b.id));
  const existingNodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const defaultTenant = options.find(t => t.id === who.tenant_scope)?.id ?? (options.length ? options[0].id : '');
  const [form, setForm] = useState({
    id: '',
    name: '',
    tenant_id: '',
    public_ipv4_nat: false,
    public_ipv6_nat: false,
    egress_allowed: true,
  });
  const tenantId = form.tenant_id || defaultTenant;

  /* id 是 slug，与服务端 brocade_core::model::is_valid_slug 使用同一规则。 */
  const SLUG_RE = /^[a-z0-9._-]{1,32}$/;
  const idInvalid = form.id !== '' && !SLUG_RE.test(form.id);
  const idTaken = form.id.trim() !== '' && (existingNodes.data?.nodes ?? []).some(n => n.node_id === form.id.trim());

  const provision = useMutation({
    mutationFn: () =>
      previewProvisionNode({
        id: form.id.trim(),
        tenant_id: tenantId,
        name: form.name.trim() || form.id.trim(),
        public_ipv4_nat: form.public_ipv4_nat,
        public_ipv6_nat: form.public_ipv6_nat,
        egress_allowed: form.egress_allowed,
        dns: { t: 'system' },
      }),
    onSuccess: next => {
      qc.invalidateQueries({ queryKey: ['nodes'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      go({ p: 'install', node: next.node.id, step: 2, result: next });
    },
  });

  const ready = !!form.id.trim() && !idInvalid && !idTaken && !!tenantId;

  return (
    <form
      className="wz"
      onSubmit={e => {
        e.preventDefault();
        provision.mutate();
      }}
    >
      <div className="chain-hd">
        <b>纳管向导</b>
        <span className="subid mono">新容器</span>
      </div>

      <PreviewBanner status={status} />

      <div className="wz-fields">
        <div className="wz-fld">
          <label>机器 ID</label>
          <input
            className="f mono"
            value={form.id}
            placeholder="hk-01"
            onChange={e => setForm({ ...form, id: e.target.value })}
          />
          {idInvalid ? (
            <p className="note warn">只能使用 a-z 0-9 . _ -，最长 32 个字符。</p>
          ) : idTaken ? (
            <p className="note warn">该 ID 已存在。</p>
          ) : (
            <p className="note">唯一键，创建后不可修改。容器名由它派生。</p>
          )}
        </div>
        <div className="wz-fld">
          <label>机器名称</label>
          <input
            className="f"
            value={form.name}
            placeholder="香港入口"
            onChange={e => setForm({ ...form, name: e.target.value })}
          />
          <p className="note">列表和拓扑图上显示的名称，可随时修改</p>
        </div>
        <div className="wz-fld">
          <label>归属租户</label>
          {/* 始终使用下拉框，只有一个选项时同样如此（与生产流程和建链向导的规则一致） */}
          <select className="f" value={tenantId} onChange={e => setForm({ ...form, tenant_id: e.target.value })}>
            {options.length === 0 && <option value="">还没有租户</option>}
            {options.map(t => (
              <option key={t.id} value={t.id}>
                {t.name && t.name !== t.id ? `${t.name}（${t.id}）` : t.id}
              </option>
            ))}
          </select>
          <p className="note">创建后不可修改。</p>
        </div>
      </div>

      {/* ── 网络：preview 中地址由系统分配，此处只标明是否可被连接 ── */}
      <h4 className="sec">
        网络
        <span className="rule" />
      </h4>
      <div className="wz-hops">
        <div className="wz-hop">
          <span className="idx">v4</span>
          <span className="who">
            <b>容器 IPv4</b>
            {form.public_ipv4_nat ? (
              <span className="st">经 NAT，不可直连</span>
            ) : (
              <span className="st b-role">可直连</span>
            )}
          </span>
          <span className="ctl" />
          <span className="attrs">
            <span className="attr">
              <span className="k">可达</span>
              <SegSw
                checked={form.public_ipv4_nat}
                onChange={v => setForm({ ...form, public_ipv4_nat: v })}
                off="直连"
                on="经 NAT"
              />
            </span>
            <span className="attr">
              <span className="note">地址由 preview 从 {status.subnet} 中分配，无需填写。</span>
            </span>
          </span>
        </div>
        <div className="wz-hop">
          <span className="idx">v6</span>
          <span className="who">
            <b>容器 IPv6</b>
            {form.public_ipv6_nat ? (
              <span className="st">经 NAT，不可直连</span>
            ) : (
              <span className="st b-role">可直连</span>
            )}
          </span>
          <span className="ctl" />
          <span className="attrs">
            <span className="attr">
              <span className="k">可达</span>
              <SegSw
                checked={form.public_ipv6_nat}
                onChange={v => setForm({ ...form, public_ipv6_nat: v })}
                off="直连"
                on="经 NAT"
              />
            </span>
            <span className="attr">
              <span className="note">从 {status.subnet_ipv6} 中分配。</span>
            </span>
          </span>
        </div>
      </div>

      {/* ── 角色：preview 的容器一律加入 overlay，因此只保留出网这一项 ── */}
      <h4 className="sec">
        角色
        <span className="rule" />
      </h4>
      <div className="wz-fields">
        <div className="wz-fld">
          <label>出网</label>
          <div>
            <SegSw
              checked={form.egress_allowed}
              onChange={v => setForm({ ...form, egress_allowed: v })}
              off="禁止"
              on="允许"
            />
          </div>
          <p className="note">禁止时它只能作为中转，指向它的落地规则会在编译时被拒绝。</p>
        </div>
      </div>

      {tenants.data && tenants.data.tenants.length === 0 && (
        <div className="callout warn" style={{ marginTop: 12 }}>
          还没有租户。机器必须归属一个租户，先去「租户」面建一个。
        </div>
      )}
      {provision.error && <ErrorBox error={provision.error} />}

      <div className="wz-foot">
        {/* 与生产流程的说明一致：该步骤不进入草稿。preview 中还会额外创建一个容器。 */}
        <span className="note warn">
          <b>这一步没有草稿</b>：提交后立即写入库、盖出一版新修订，并启动一个容器。
        </span>
        <span className="sp" />
        <button type="button" className="btn" onClick={() => go({ p: 'list' })}>
          取消
        </button>
        <button className="btn primary" type="submit" disabled={!ready || provision.isPending}>
          {provision.isPending ? '启动中…' : '创建 preview 容器'}
        </button>
      </div>
    </form>
  );
}

// 第二种状态：容器已启动，等待其上线，然后执行验证和发布。
//
// 生产流程中该位置提供带 token 的命令供手动在机器上执行；preview 中脚本已在
// 容器内前台运行，因此该位置显示日志。上线判定读取同一份 `/nodes/agent-state`。
function PreviewInstall({
  node,
  result,
  go,
  status,
}: {
  node: string;
  result?: PreviewProvisionNodeResult;
  go: (d: Drill) => void;
  status: PreviewStatus;
}) {
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), refetchInterval: 3_000 });
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const logs = useQuery({
    queryKey: ['preview-node-logs', node],
    queryFn: () => fetchPreviewNodeLogs(node),
    refetchInterval: 3_000,
  });

  const nodeRow = (nodes.data?.nodes ?? []).find(x => x.node_id === node);
  const nodeLabel = nodeRow?.name || node;
  const tenant = result?.node.tenant_id ?? nodeRow?.tenant_id ?? '';
  const target = result?.revision_id ?? current;
  // 上线判定与纳管流程使用同一个 hook：token 被使用 → agent 启动 → 首次 desired 心跳。
  const probe = useAgentLiveness(nodeRow);

  const [userId, setUserId] = useState('');
  const verify = useMutation({
    mutationFn: () => previewVerifySubscription({ tenant_id: tenant, user_id: userId }),
  });

  return (
    <>
      <div className="chain-hd">
        <b>纳管向导</b>
        <span className="subid mono">
          {nodeLabel} / {node}
        </span>
      </div>

      <PreviewBanner status={status} />

      <div className="wz-hops">
        <div className="wz-hop">
          <span className="idx">01</span>
          <span className="who">
            <b>容器里的安装脚本</b>
            <span className="mono dim">{logs.data?.container_name ?? ''}</span>
          </span>
          <span className="ctl">
            <span className="note">每 3 秒刷新</span>
          </span>
          <span className="attrs" style={{ display: 'block' }}>
            {logs.error ? (
              <ErrorBox error={logs.error} />
            ) : logs.data ? (
              <pre className="code" style={{ margin: 0, maxHeight: 220 }}>
                {logs.data.install_log || logs.data.container_log || '暂无日志'}
              </pre>
            ) : (
              <Loading />
            )}
          </span>
        </div>

        <div className="wz-hop">
          <span className="idx">02</span>
          <span className="who">
            <b>等它上线</b>
            {probe?.state === 'online' ? (
              <span className="st st-succeeded">已上线</span>
            ) : probe?.state === 'polling' ? (
              <span className="st st-warn">已兑换，等心跳</span>
            ) : (
              <span className="st">等待兑换</span>
            )}
          </span>
          <span className="ctl" />
          <span className="attrs">
            <span className="note">
              {probe?.state === 'online'
                ? `agent 心跳于 ${probe.agoSec} 秒前。`
                : probe?.state === 'polling'
                  ? probe.onceOnline
                    ? 'token 已兑换，但超过 60 秒没有心跳，容器可能已停止。'
                    : 'token 被用了；agent 起来后 15 秒内会来拉配置。'
                  : '安装脚本还没拿这枚 token 去纳管。'}
            </span>
          </span>
        </div>

        {/* preview 特有：使用一个实际用户的订阅执行一次探测，端到端确认该机器可用 */}
        <div className="wz-hop">
          <span className="idx">03</span>
          <span className="who">
            <b>验证订阅</b>
            {verify.data ? (
              verify.data.ok ? (
                <span className="st st-succeeded">通过</span>
              ) : (
                <span className="st st-warn">没通过</span>
              )
            ) : (
              <span className="st">可选</span>
            )}
          </span>
          <span className="ctl">
            <button
              type="button"
              className="btn sm"
              disabled={!userId.trim() || !tenant || verify.isPending}
              onClick={() => verify.mutate()}
            >
              {verify.isPending ? '验证中…' : '验证'}
            </button>
          </span>
          <span className="attrs">
            <span className="attr">
              <span className="k">用户</span>
              <input
                className="f mono"
                style={{ width: 150 }}
                value={userId}
                placeholder="alice"
                onChange={e => setUserId(e.target.value)}
              />
            </span>
            <span className="attr">
              <span className="note">
                在租户 <span className="mono">{tenant || '…'}</span> 下查找该用户的订阅。HTTP 探测与 Xray 用户 stats
                全部通过才算通过。
              </span>
            </span>
          </span>
        </div>
      </div>

      {verify.error && <ErrorBox error={verify.error} />}
      {verify.data && !verify.data.ok && <pre className="code">{JSON.stringify(verify.data, null, 2)}</pre>}

      <div className="wz-foot">
        <span className="sp" />
        <button className="btn" onClick={() => go({ p: 'list' })}>
          回机器列表
        </button>
        <button className="btn" onClick={() => go({ p: 'node', id: node })}>
          看{nodeLabel}
        </button>
        <button
          className="btn primary"
          disabled={target == null || probe?.state !== 'online'}
          title={probe?.state === 'online' ? '' : '等 agent 上线后再发布'}
          onClick={() => {
            /* 与 panes/nodes.tsx 中的对应按钮一致：旧外壳使用窗口，新外壳使用 nav 和地址栏 */
            openTabByKey('deploy');
            navigate('deploy', { p: 'plan', revision: target });
            go({ p: 'list' });
          }}
        >
          {probe?.state === 'online' ? `去发布 · 计划预览（修订 ${target ?? '…'}）` : '等 agent 上线…'}
        </button>
      </div>
    </>
  );
}

// 分段控件。生产流程使用 panes/nodes.tsx 中的 SegSwitch——该组件未导出，
// 而为一个十行的组件将其提取到公共模块并在两处引入，成本高于在此保留一份。
// 外观由 .segsw 决定，两处一致。
function SegSw({
  checked,
  onChange,
  off,
  on,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  off: string;
  on: string;
}) {
  return (
    <span className="segsw" role="group">
      {([false, true] as const).map(v => (
        <button key={String(v)} type="button" aria-pressed={checked === v} onClick={() => onChange(v)}>
          {v ? on : off}
        </button>
      ))}
    </span>
  );
}
