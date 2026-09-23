// 链路与 MTU。
//
// 本页的依据是 `LinkMtuView` 结构中的一条约束：
// 测量按对进行（路径的属性），设置按节点进行（接口的属性）。
//
// wg 中每台机器一个 wg0、一个 MTU，`[Peer]` 段中没有该键——因此设置只能挂在节点上。
// 但决定该值的是某一条具体路径，因此服务端提供了 `tightest_peer`：
// 需要知道是哪条路径的 MTU 较小才能进行处理。两张表都需要且需要能相互对应；
// 只有节点表时，看到建议值 1380 但不知道由哪条路径决定；只有链路表时，不知道应修改哪台机器。
//
// `inconclusive` 不是零值而是提示：建议值只基于探测成功的路径，
// 未探测成功的越多，该建议偏大的可能性越高。偏小只影响吞吐，偏大会导致大包被丢弃。
//
// ## 版面：总览 → 分层读数 → 原始数据
//
// 页面先汇总可用链路、路径探测与 MTU 状态，再按数据层次展开：
//
// - 端到端表示用户当前能否使用——REALITY 参数错误、
//   规则遗漏转发、出口被封禁，这三种情况都不会使任何一跳的计数器停止增长，
//   逐跳计数器无法发现这些问题。
// - 节点 MTU 是依据：应修改为什么值、由哪条路径决定。本页只读——修改在机器详情
//   的 WIREGUARD 卡中进行，该处与同一机器的其他 wg0 参数相邻。
// - 逐对探测保留为默认折叠的原始数据，避免大量相似读数掩盖路径状态。

import { useQuery } from '@tanstack/react-query';
import {
  fetchE2eProbes,
  fetchLinkMtu,
  fetchLinkQuality,
  type E2eProbeItem,
  type HopLinkView,
  type LinkMtuItem,
  type NodeMtuItem,
} from '../api';
import { HopLinkTable } from './telemetry';
import { FleetNetPanel } from './fleet-net-panel';
import { Ago, Empty, ErrorBox, Loading } from '../ui/bits';
import { Icon, ListIcon, PanelTitle } from '../ui/icons';
import { useNodeNames } from '../ui/node-name';
import { ExitVerdict, ProbeBadge, ProbeSpark, toneOf } from '../ui/probe';

const STATUS_LABEL: Record<string, string> = {
  ok: '探通',
  unreachable: '不可达',
  blocked: 'ICMP 被挡',
  unsupported: '不支持',
};

type LinkTone = 'ok' | 'warn' | 'bad' | 'quiet';

/** 生效值与建议值的关系。偏大会导致丢包，因此单独作为一档。 */
function verdict(item: NodeMtuItem): { tone: LinkTone; label: string; detail: string } {
  if (item.suggested_mtu == null)
    return { tone: 'quiet', label: '暂无建议', detail: `${item.inconclusive} 条路径未完成` };
  if (item.suggested_mtu < item.current_mtu)
    return {
      tone: 'bad',
      label: '需要调整',
      detail: `高出建议 ${item.current_mtu - item.suggested_mtu}`,
    };
  if (item.suggested_mtu > item.current_mtu)
    return {
      tone: 'warn',
      label: '低于建议',
      detail: `可设为 ${item.suggested_mtu}`,
    };
  return { tone: 'ok', label: '合适', detail: '与建议一致' };
}

// 本页只读。此处此前有「采纳建议」——它向草稿推入一条 update_node，而同一字段在
// 机器详情的 WIREGUARD 卡中也可修改。两个入口写入同一字段时，草稿中会出现两条来源
// 不同的同名改动，而操作者只记得其中一次操作。
export function LinksPane() {
  const mtu = useQuery({
    queryKey: ['link-mtu'],
    queryFn: () => fetchLinkMtu(),
    refetchInterval: 60_000,
  });
  const probes = useQuery({
    queryKey: ['e2e-probes'],
    queryFn: () => fetchE2eProbes(),
    refetchInterval: 60_000,
  });
  // 逐跳链路质量。与本页的另外三类并列，但取数方式不同：另外三类都是主动探测
  // （ping 测量 MTU、建立一次连接），本类不发送任何探测包，读取的是内核在实际转发连接上
  // 已计算的估计值。`retry: false` 是因为该端点是后增加的，旧版控制面返回 404，
  // 此时整块不渲染即可。
  const quality = useQuery({
    queryKey: ['link-quality'],
    queryFn: () => fetchLinkQuality(),
    refetchInterval: 30_000,
    retry: false,
  });

  const links = mtu.data?.links ?? [];
  const nodes = mtu.data?.nodes ?? [];
  const defaultMtu = mtu.data?.default_mtu ?? 0;
  const chains = probes.data?.chains ?? [];
  const hops = quality.data?.hops ?? [];

  return (
    <div className="cardpage link-page">
      <section className="panel titled link-summary-panel" data-page-title="true">
        <header>
          <ListIcon of="link" />
          <h4>链路与 MTU</h4>
          <span className="hint">端到端、实际转发与路径尺寸</span>
          <span className="sp" />
          <span className="link-page-live">
            <i /> 自动刷新
          </span>
        </header>
        {mtu.isPending ? (
          <Loading variant="metrics" />
        ) : mtu.error ? (
          <ErrorBox error={mtu.error} />
        ) : (
          <LinkOverview
            chains={chains}
            links={links}
            nodes={nodes}
            hops={hops}
            defaultMtu={defaultMtu}
            pending={probes.isPending}
          />
        )}
      </section>

      {mtu.isPending ? (
        <>
          <Loading variant="chart-panel" />
          <Loading variant="table-panel" />
        </>
      ) : (
        mtu.data && (
          <>
            <FleetNetPanel />
            <E2eSection chains={chains} pending={probes.isPending} error={probes.error} />
            <LinkQualitySection hops={hops} />
            <MtuSection nodes={nodes} defaultMtu={defaultMtu} />
            <PairProbes links={links} />
          </>
        )
      )}
    </div>
  );
}

/** 逐跳链路质量。标识转换为机器名，与本页其他位置一致。 */
function LinkQualitySection({ hops }: { hops: HopLinkView[] }) {
  const nodeName = useNodeNames();
  if (hops.length === 0) return null;
  return <HopLinkTable hops={hops} nodeName={nodeName} title="逐跳质量" />;
}

function LinkOverview({
  chains,
  links,
  nodes,
  hops,
  defaultMtu,
  pending,
}: {
  chains: E2eProbeItem[];
  links: LinkMtuItem[];
  nodes: NodeMtuItem[];
  hops: HopLinkView[];
  defaultMtu: number;
  pending: boolean;
}) {
  if (pending) {
    return (
      <div className="link-overview" aria-label="链路概览">
        <Loading variant="metrics" />
      </div>
    );
  }
  const down = chains.filter(chain => toneOf(chain) === 'down');
  const mismatched = chains.filter(chain => chain.status === 'ok' && chain.exit_verdict === 'mismatch');
  const available = chains.filter(chain => chain.status === 'ok' && chain.exit_verdict !== 'mismatch').length;
  const pathOk = links.filter(link => link.status === 'ok').length;
  const pathDown = links.length - pathOk;
  const mtuExact = nodes.filter(node => node.suggested_mtu === node.current_mtu).length;
  const mtuHigh = nodes.filter(node => node.suggested_mtu != null && node.suggested_mtu < node.current_mtu);
  const mtuLow = nodes.filter(node => node.suggested_mtu != null && node.suggested_mtu > node.current_mtu);
  const activeHops = hops.filter(hop => hop.sample.conns > 0).length;
  const overallTone: LinkTone =
    down.length > 0 || pathDown > 0 || mtuHigh.length > 0
      ? 'bad'
      : mismatched.length > 0 || mtuLow.length > 0
        ? 'warn'
        : chains.length + links.length + nodes.length === 0
          ? 'quiet'
          : 'ok';
  const status =
    down.length > 0
      ? `${down.map(chain => chain.chain_name).join('、')} 当前不可用`
      : mismatched.length > 0
        ? `${mismatched.map(chain => chain.chain_name).join('、')} 的出口与预期不符`
        : pathDown > 0
          ? `${pathDown} 条机器间路径未探通`
          : mtuHigh.length > 0
            ? `${mtuHigh.length} 台机器的 MTU 高于探测建议`
            : mtuLow.length > 0
              ? `${mtuLow.length} 台机器的 MTU 低于探测建议`
              : chains.length + links.length + nodes.length === 0
                ? '等待机器上报第一轮探测结果'
                : '当前未发现链路异常';

  const statusTone = overallTone === 'bad' ? ' err' : overallTone === 'warn' ? ' warn' : '';
  return (
    <div className="link-overview" aria-label="链路概览">
      <div className="link-overview-grid">
        <LinkMetric
          icon="chains"
          label="端到端可用"
          value={chains.length === 0 ? '—' : `${available}/${chains.length}`}
          meta={mismatched.length > 0 ? `${mismatched.length} 条出口不符` : '完整用户路径'}
          tone={down.length > 0 ? 'bad' : mismatched.length > 0 ? 'warn' : chains.length > 0 ? 'ok' : 'quiet'}
        />
        <LinkMetric
          icon="observe"
          label="路径探通"
          value={links.length === 0 ? '—' : `${pathOk}/${links.length}`}
          meta="机器之间"
          tone={pathDown > 0 ? 'bad' : links.length > 0 ? 'ok' : 'quiet'}
        />
        <LinkMetric
          icon="nodes"
          label="MTU 一致"
          value={nodes.length === 0 ? '—' : `${mtuExact}/${nodes.length}`}
          meta={`全局默认 ${defaultMtu}`}
          tone={
            mtuHigh.length > 0
              ? 'bad'
              : mtuLow.length > 0
                ? 'warn'
                : nodes.length > 0 && mtuExact === nodes.length
                  ? 'ok'
                  : 'quiet'
          }
        />
        <LinkMetric
          icon="usage"
          label="实际转发"
          value={hops.length === 0 ? '—' : `${activeHops}/${hops.length}`}
          meta="有连接的逐跳样本"
          tone={activeHops > 0 ? 'ok' : 'quiet'}
        />
      </div>
      <div className={`callout link-overview-status${statusTone}`} role="status">
        <i />
        <span>{status}</span>
      </div>
    </div>
  );
}

function LinkMetric({
  icon,
  label,
  value,
  meta,
  tone,
}: {
  icon: 'chains' | 'observe' | 'nodes' | 'usage';
  label: string;
  value: string;
  meta: string;
  tone: LinkTone;
}) {
  return (
    <div className={`link-metric ${tone}`}>
      <span className="link-metric-icon">
        <Icon of={icon} size={15} />
      </span>
      <span className="link-metric-label">{label}</span>
      <strong>{value}</strong>
      <small>{meta}</small>
    </div>
  );
}

// 端到端探测。火花线在本页的作用高于单条链的详情页：横向对比多条链可直接看出
// 某条链从第几次开始变慢——该情况通常由某次发布导致，而非网络原因。
function E2eSection({ chains, pending, error }: { chains: E2eProbeItem[]; pending: boolean; error: unknown }) {
  const nameOf = useNodeNames();
  return (
    <section className="panel titled">
      <header>
        <PanelTitle of="chains">端到端探测</PanelTitle>
        <span className="hint">从入口经过完整用户路径到达外部落点</span>
        <span className="sp" />
        {chains.length > 0 && <span className="hint">{chains.length} 条链</span>}
      </header>
      <div className="link-card-body">
        {pending ? (
          <Loading variant="table" />
        ) : error ? (
          <ErrorBox error={error} />
        ) : chains.length === 0 ? (
          <Empty>还没有端到端探测结果。</Empty>
        ) : (
          <div className="link-table-wrap">
            <table className="tbl cards link-table">
              <thead>
                <tr>
                  <th>状态</th>
                  <th>链</th>
                  <th>入口机器</th>
                  <th>出口地址</th>
                  <th>出口核对</th>
                  <th>最近记录</th>
                  <th>更新时间</th>
                </tr>
              </thead>
              <tbody>
                {chains.map(chain => (
                  <tr key={`${chain.app_id}/${chain.chain_id}`}>
                    <td data-label="状态">
                      <ProbeBadge item={chain} />
                    </td>
                    <td data-label="链">
                      <b>{chain.chain_name}</b>
                      <span className="link-table-id mono">{chain.chain_id}</span>
                    </td>
                    <td data-label="入口机器" title={chain.node_id}>
                      {nameOf(chain.node_id)}
                    </td>
                    <td data-label="出口地址" className="mono dim">
                      {chain.exit_ip ?? '—'}
                      {chain.exit_loc && ` ${chain.exit_loc}`}
                    </td>
                    <td data-label="出口核对">
                      <ExitVerdict item={chain} />
                    </td>
                    <td data-label="最近记录">
                      <ProbeSpark samples={chain.samples} />
                    </td>
                    <td data-label="更新时间" className="dim">
                      <Ago at={chain.probed_at} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </section>
  );
}

// 节点 MTU。使用一张表完整列出，不折叠也不生成摘要——该表的六列各自表示不同信息
// （生效值、探测建议值、由哪条路径决定、有几条未探测成功、差值是否需要处理），
// 压缩为一句话需要从中选取一项，任何选择都会丢失其余信息。
//
// 只读。修改 MTU 在机器详情页的 WIREGUARD 卡中进行，该处与「入站传输」「是否加入
// overlay」相邻——同一机器的 wg0 参数集中在一处修改。本页用于诊断：它表示应修改为
// 什么值及其依据，而非提供修改入口。两处都可修改时，草稿中同一字段会有两个来源。
function MtuSection({ nodes, defaultMtu }: { nodes: NodeMtuItem[]; defaultMtu: number }) {
  const nameOf = useNodeNames();
  return (
    <section className="panel titled">
      <header>
        <PanelTitle of="nodes">机器 MTU</PanelTitle>
        <span className="hint">全局默认 {defaultMtu} · 在机器详情修改生效值</span>
        <span className="sp" />
        {nodes.length > 0 && <span className="hint">{nodes.length} 台机器</span>}
      </header>
      <div className="link-card-body">
        {nodes.length === 0 ? (
          <Empty>还没有机器上报 MTU 探测结果。</Empty>
        ) : (
          <div className="link-table-wrap">
            <table className="tbl cards link-table link-mtu-table">
              <thead>
                <tr>
                  <th>机器</th>
                  <th>当前值</th>
                  <th>建议值</th>
                  <th>最窄路径</th>
                  <th>未完成</th>
                  <th>状态</th>
                </tr>
              </thead>
              <tbody>
                {nodes.map(node => {
                  const state = verdict(node);
                  return (
                    <tr key={node.node_id}>
                      <td data-label="机器" title={node.node_id}>
                        <b>{nameOf(node.node_id)}</b>
                        <span className="link-table-id mono">{node.node_id}</span>
                      </td>
                      <td data-label="当前值">
                        <span className="link-mtu-value mono">{node.current_mtu}</span>
                        <span className="link-value-source">{node.overridden ? '本机设置' : '跟随全局'}</span>
                      </td>
                      <td data-label="建议值" className="mono link-mtu-value">
                        {node.suggested_mtu ?? '—'}
                      </td>
                      <td data-label="最窄路径" title={node.tightest_peer ?? ''}>
                        {node.tightest_peer ? nameOf(node.tightest_peer) : '—'}
                      </td>
                      <td data-label="未完成" className="mono">
                        {node.inconclusive || '—'}
                      </td>
                      <td data-label="状态">
                        <span className={`link-verdict ${state.tone}`}>
                          <b>{state.label}</b>
                          <small>{state.detail}</small>
                        </span>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </section>
  );
}

// 逐对探测。默认折叠：n 台机器对应 n×(n-1) 条路径，八台即 56 条，而多数情况下
// 它们的读数完全相同。摘要行表明是否存在异常，需要查看原始读数时再展开。
function PairProbes({ links }: { links: LinkMtuItem[] }) {
  const nameOf = useNodeNames();
  const bad = links.filter(l => l.status !== 'ok');
  /* 所有路径 MTU 相同时显示为一个数值——这是常态，显示后即可确定无需展开。 */
  const mtus = [...new Set(links.filter(l => l.path_mtu != null).map(l => l.path_mtu))];

  if (links.length === 0) {
    return (
      <section className="panel titled">
        <header>
          <PanelTitle of="observe">路径原始读数</PanelTitle>
          <span className="hint">每一对机器之间的 underlay 探测</span>
        </header>
        <div className="link-card-body">
          <Empty>还没有路径探测结果。</Empty>
        </div>
      </section>
    );
  }
  return (
    <details className="panel titled">
      <summary>
        <PanelTitle of="observe">路径原始读数</PanelTitle>
        <span className="hint">
          {bad.length > 0 ? `${links.length} 条中有 ${bad.length} 条未探通` : `${links.length} 条路径均已探通`}
          {bad.length === 0 && mtus.length === 1 && ` · 路径 MTU ${mtus[0]}`}
        </span>
      </summary>
      <div className="link-card-body">
        <div className="link-table-wrap">
          <table className="tbl cards link-table">
            <thead>
              <tr>
                <th>路径</th>
                <th>探测落点</th>
                <th>状态</th>
                <th>路径 MTU</th>
                <th>建议 WG MTU</th>
                <th>更新时间</th>
              </tr>
            </thead>
            <tbody>
              {links.map(link => (
                <tr key={`${link.node_id}>${link.peer_node_id}`}>
                  <td data-label="路径">
                    <b>
                      {nameOf(link.node_id)} <span className="dim">→</span> {nameOf(link.peer_node_id)}
                    </b>
                    <span className="link-table-id mono">
                      {link.node_id} → {link.peer_node_id}
                    </span>
                  </td>
                  <td data-label="探测落点" className="mono dim">
                    {link.endpoint_host}
                  </td>
                  <td data-label="状态">
                    <span className={`link-path-state ${link.status === 'ok' ? 'ok' : 'warn'}`}>
                      <i /> {STATUS_LABEL[link.status] ?? link.status}
                    </span>
                  </td>
                  <td data-label="路径 MTU" className="mono link-mtu-value">
                    {link.path_mtu ?? '—'}
                  </td>
                  <td data-label="建议 WG MTU" className="mono link-mtu-value">
                    {link.suggested_wg_mtu ?? '—'}
                  </td>
                  <td data-label="更新时间" className="dim">
                    <Ago at={link.probed_at} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </details>
  );
}
