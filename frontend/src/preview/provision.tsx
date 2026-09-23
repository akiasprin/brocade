import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { fetchNodes, fetchRevisions, fetchTenants } from '../api';
import { useAgentLiveness } from '../ui/agent-alive';
import { ErrorBox, Loading, SegSwitch } from '../ui/bits';
import { navigate, returnTo } from '../forge/route';
import { nodeIdError } from '../provision-form';
import { WizardHeader, WizardSummary, WizardSummaryItem } from '../ui/wizard';
import { useUnsavedChanges } from '../ui/navigation-guard';
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
// - 版面：机器尚未入库时填写一张表单；创建后显示容器安装日志与上线状态。页头沿用
//   生产流程的进度语义和 `.wz-*` 组件，让相同阶段在两种环境中处于相同位置。
// - 流程存在实际差异，不是重复实现：此处不填写公网 IP（容器 IP 由 preview 分配）、
//   安装由容器自动执行生产环境的安装脚本（显示日志，而非提供命令供手动执行）、
//   末尾增加一步订阅验证。
//
// 两侧共用的判断只有一项：agent 是否上线——两者读取同一份 `/nodes/agent-state`。
export type PreviewWizDrill =
  { p: 'provision'; step: number } | { p: 'install'; node: string; step: number; result?: PreviewProvisionNodeResult };

const PREVIEW_FORM_DEFAULTS = {
  id: '',
  name: '',
  public_ipv4_nat: false,
  public_ipv6_nat: false,
  egress_allowed: true,
};

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
    <div className="callout blue wz-preview-banner">
      <div>
        <b>Preview 模式</b>
        <span>自动分配容器地址，并在 Docker 中执行生产安装脚本。</span>
      </div>
      <div className="wz-preview-meta">
        <span>网络 {status.network}</span>
        <span>IPv4 {status.subnet}</span>
        <span>IPv6 {status.subnet_ipv6}</span>
        <span>镜像 {status.node_image}</span>
        <span>Agent {status.agent_binary_ready ? '已就绪' : '自动构建'}</span>
      </div>
    </div>
  );
}

/* 第一种状态：机器尚未入库。 */
function PreviewForm({ go, status }: { go: (d: Drill) => void; status: PreviewStatus }) {
  const qc = useQueryClient();
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  const options = [...(tenants.data?.tenants ?? [])].sort((a, b) => a.id.localeCompare(b.id));
  const existingNodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const defaultTenant = options.length === 1 ? options[0].id : '';
  const [form, setForm] = useState(() => ({ ...PREVIEW_FORM_DEFAULTS }));
  const [attempted, setAttempted] = useState(false);
  const tenantId = defaultTenant;
  const guardScope = 'preview-node-provision';
  const dirty = JSON.stringify(form) !== JSON.stringify(PREVIEW_FORM_DEFAULTS);
  const clearUnsavedChanges = useUnsavedChanges(dirty, 'Preview 纳管表单', guardScope);

  const idError = nodeIdError(form.id, new Set((existingNodes.data?.nodes ?? []).map(node => node.node_id)));
  const tenantError =
    options.length === 0
      ? '当前账号没有可用于创建预览机器的租户'
      : options.length > 1
        ? '当前流程要求恰好一个可见租户，请先收窄账号范围'
        : null;

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
      clearUnsavedChanges();
      go({ p: 'install', node: next.node.id, step: 2, result: next });
    },
  });

  if (tenants.isPending || existingNodes.isPending) return <Loading variant="form" />;
  if (tenants.error || existingNodes.error) return <ErrorBox error={tenants.error ?? existingNodes.error} />;

  const blocker = idError ?? tenantError;
  const ready = !blocker && !!tenantId;

  return (
    <form
      className="wz"
      onSubmit={e => {
        e.preventDefault();
        setAttempted(true);
        if (!ready || provision.isPending) return;
        provision.mutate();
      }}
    >
      <WizardHeader
        eyebrow="机器 / Preview 纳管"
        title="创建预览机器"
        context="新容器"
        description="登记机器身份后，Preview 会创建容器并自动运行生产安装脚本。"
        stages={[
          { label: '登记配置', state: 'current' },
          { label: '启动容器', state: 'next' },
          { label: '上线验证', state: 'next' },
        ]}
        aside={<span className="wz-mode preview">Docker Preview</span>}
      />

      <PreviewBanner status={status} />

      <WizardSummary label="预览机器配置摘要">
        <WizardSummaryItem label="名称">{form.name.trim() || form.id.trim() || '待填写'}</WizardSummaryItem>
        <WizardSummaryItem label="IPv4">{form.public_ipv4_nat ? 'NAT' : '直连'}</WizardSummaryItem>
        <WizardSummaryItem label="IPv6">{form.public_ipv6_nat ? 'NAT' : '直连'}</WizardSummaryItem>
        <WizardSummaryItem label="出网">{form.egress_allowed ? '允许' : '禁止'}</WizardSummaryItem>
      </WizardSummary>

      <h4 className="sec">
        基本信息
        <span className="rule" />
      </h4>

      <div className="wz-fields">
        <div className="wz-fld">
          <label htmlFor="preview-node-id">机器 ID</label>
          <input
            id="preview-node-id"
            className="f mono"
            value={form.id}
            placeholder="hk-01"
            autoFocus
            autoComplete="off"
            autoCapitalize="none"
            spellCheck={false}
            aria-invalid={!!idError && (attempted || !!form.id)}
            aria-describedby="preview-node-id-note"
            onChange={e => setForm({ ...form, id: e.target.value })}
          />
          {idError && (attempted || !!form.id) ? (
            <p className="note warn" id="preview-node-id-note">
              {idError}。
            </p>
          ) : (
            <p className="note" id="preview-node-id-note">
              唯一键，创建后不可修改。容器名由它派生。
            </p>
          )}
        </div>
        <div className="wz-fld">
          <label htmlFor="preview-node-name">机器名称</label>
          <input
            id="preview-node-name"
            className="f"
            value={form.name}
            placeholder="香港入口"
            autoComplete="off"
            onChange={e => setForm({ ...form, name: e.target.value })}
          />
          <p className="note">列表和拓扑图上显示的名称，可随时修改</p>
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
              <SegSwitch
                checked={form.public_ipv4_nat}
                onChange={v => setForm({ ...form, public_ipv4_nat: v })}
                off="直连"
                on="经 NAT"
                ariaLabel="预览机器 IPv4 可达方式"
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
              <SegSwitch
                checked={form.public_ipv6_nat}
                onChange={v => setForm({ ...form, public_ipv6_nat: v })}
                off="直连"
                on="经 NAT"
                ariaLabel="预览机器 IPv6 可达方式"
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
            <SegSwitch
              checked={form.egress_allowed}
              onChange={v => setForm({ ...form, egress_allowed: v })}
              off="禁止"
              on="允许"
              ariaLabel="预览机器出网权限"
            />
          </div>
          <p className="note">禁止时它只能作为中转节点，指向它的本机出网规则会在编译时被拒绝。</p>
        </div>
      </div>

      {tenants.data && tenants.data.tenants.length !== 1 && (
        <div className="callout warn" style={{ marginTop: 12 }}>
          系统归属配置异常：当前必须恰好有一条，实际为 {tenants.data.tenants.length} 条。
        </div>
      )}
      {provision.error && <ErrorBox error={provision.error} />}

      <div className="wz-foot">
        <span className="wz-submit-note" id="preview-submit-note" aria-live="polite">
          <b>{blocker ? '还不能创建' : '准备就绪'}</b>
          <small>{blocker ?? '将创建机器记录、Preview 容器与一版修订。'}</small>
        </span>
        <span className="sp" />
        <button type="button" className="btn" disabled={provision.isPending} onClick={() => returnTo('nodes')}>
          取消
        </button>
        <button
          className="btn primary"
          type="submit"
          disabled={!ready || provision.isPending}
          aria-describedby="preview-submit-note"
          aria-busy={provision.isPending}
        >
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
  const online = probe?.state === 'online';
  const redeemed = probe?.state === 'polling' || online;

  const [userId, setUserId] = useState('');
  const verify = useMutation({
    mutationFn: () => previewVerifySubscription({ tenant_id: tenant, user_id: userId }),
  });

  if (nodes.isPending || revisions.isPending) return <Loading variant="form" />;
  if (nodes.error || revisions.error) return <ErrorBox error={nodes.error ?? revisions.error} />;

  return (
    <>
      <WizardHeader
        eyebrow="机器 / Preview 纳管"
        title={online ? '预览机器已上线' : '启动并验证'}
        context={`${nodeLabel} / ${node}`}
        description="容器正在执行生产安装脚本；上线后可用真实订阅做一次端到端验证。"
        stages={[
          { label: '登记配置', state: 'done' },
          { label: '启动容器', state: redeemed ? 'done' : 'current' },
          { label: '上线验证', state: online ? 'done' : redeemed ? 'current' : 'next' },
        ]}
        aside={<span className="wz-mode preview">Docker Preview</span>}
      />

      <PreviewBanner status={status} />

      <WizardSummary label="预览纳管状态摘要">
        <WizardSummaryItem label="机器">{nodeLabel}</WizardSummaryItem>
        <WizardSummaryItem label="容器">{logs.data?.container_name || '启动中'}</WizardSummaryItem>
        <WizardSummaryItem label="Agent">{online ? '在线' : redeemed ? '等待心跳' : '安装中'}</WizardSummaryItem>
        <WizardSummaryItem label="订阅验证">
          {verify.data ? (verify.data.ok ? '通过' : '未通过') : '尚未执行'}
        </WizardSummaryItem>
      </WizardSummary>

      <div className="wz-hops wz-flow" aria-live="polite">
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
              <Loading variant="code" />
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
              <label className="k" htmlFor="preview-verify-user">
                用户
              </label>
              <input
                id="preview-verify-user"
                className="f mono wz-user-input"
                value={userId}
                placeholder="alice"
                autoComplete="off"
                onChange={e => setUserId(e.target.value)}
              />
            </span>
            <span className="attr">
              <span className="note">使用该用户当前生效的订阅验证。HTTP 拨测与 Xray 用户 stats 全部通过才算通过。</span>
            </span>
          </span>
        </div>
      </div>

      {verify.error && <ErrorBox error={verify.error} />}
      {verify.data && !verify.data.ok && <pre className="code">{JSON.stringify(verify.data, null, 2)}</pre>}

      <div className="wz-foot">
        <span className="wz-submit-note" aria-live="polite">
          <b>{online ? '预览机器已就绪' : redeemed ? '正在等待 Agent' : '容器正在安装'}</b>
          <small>{online ? '可先验证订阅，也可以直接查看发布计划。' : '日志和上线状态每 3 秒自动刷新。'}</small>
        </span>
        <span className="sp" />
        <button type="button" className="btn" onClick={() => returnTo('nodes')}>
          回机器列表
        </button>
        <button type="button" className="btn" onClick={() => go({ p: 'node', id: node })}>
          看{nodeLabel}
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={target == null || probe?.state !== 'online'}
          title={probe?.state === 'online' ? '' : '等 agent 上线后再发布'}
          onClick={() => {
            navigate('deploy', { p: 'plan', revision: target });
          }}
        >
          {probe?.state === 'online' ? `去发布 · 计划预览（修订 ${target ?? '…'}）` : '等 agent 上线…'}
        </button>
      </div>
    </>
  );
}
