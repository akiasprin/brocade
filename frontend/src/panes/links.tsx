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
// ## 版面：结论 → 待办 → 原始数据
//
// 三块按是否需要处理由上到下排列，而非按数据粒度排列：
//
// - 端到端是结论。它是唯一表示用户当前能否使用的部分——REALITY 参数错误、
//   规则遗漏转发、出口被封禁，这三种情况都不会使任何一跳的计数器停止增长，
//   下面两块都无法发现。因此它排在第一位，并另有一条横幅给出结论。
// - 节点 MTU 是依据：应修改为什么值、由哪条路径决定。本页只读——修改在机器详情
//   的 WIREGUARD 卡中进行，该处与同一机器的其他 wg0 参数相邻。
// - 逐对探测是原始数据，默认折叠。八台机器对应 32 条路径，每条的内容都是
//   `探通 / 1500 / 1440`——32 行相同的读数不提供信息，反而会掩盖有效信息。
//
// 此前这三块是三张相同结构的平级表格，且顺序相反：端到端排在最下方，
// 而它可能正在报告多条链的出口不符。

import { useQuery } from '@tanstack/react-query';
import {
  fetchE2eProbes,
  fetchLinkMtu,
  fetchLinkQuality,
  type E2eProbeItem,
  type LinkMtuItem,
  type NodeMtuItem,
} from '../api';
import { HopLinkTable } from './telemetry';
import { FleetNetPanel } from './nodes';
import { Ago, Empty, ErrorBox, Loading } from '../ui/bits';
import { useNodeNames } from '../ui/node-name';
import { ExitVerdict, ProbeBadge, ProbeSpark, toneOf } from '../ui/probe';

const STATUS_LABEL: Record<string, string> = {
  ok: '探通',
  unreachable: '不可达',
  blocked: 'ICMP 被挡',
  unsupported: '不支持',
};

/** 生效值与建议值的关系。偏大会导致丢包，因此单独作为一档。 */
function verdict(item: NodeMtuItem): { cls: string; text: string } {
  if (item.suggested_mtu == null) return { cls: 'st-skipped', text: `${item.inconclusive} 条路径没探通，给不出建议` };
  if (item.suggested_mtu < item.current_mtu)
    return {
      cls: 'st-halted',
      text: `比建议值大 ${item.current_mtu - item.suggested_mtu}——大包会被悄悄打掉`,
    };
  if (item.suggested_mtu > item.current_mtu)
    return { cls: 'st-warn', text: `还能加到 ${item.suggested_mtu}，偏小只是慢一点` };
  return { cls: 'st-succeeded', text: '跟建议值一致' };
}

// 本页只读。此处此前有「采纳建议」——它向草稿推入一条 update_node，而同一字段在
// 机器详情的 WIREGUARD 卡中也可修改。两个入口写入同一字段时，草稿中会出现两条来源
// 不同的同名改动，而操作者只记得其中一次操作。
export function LinksPane() {
  const mtu = useQuery({ queryKey: ['link-mtu'], queryFn: () => fetchLinkMtu() });
  const probes = useQuery({ queryKey: ['e2e-probes'], queryFn: () => fetchE2eProbes() });
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

  if (mtu.isPending) return <Loading />;
  if (mtu.error) return <ErrorBox error={mtu.error} />;

  const { links, nodes, default_mtu } = mtu.data;
  const chains = probes.data?.chains ?? [];
  const hops = quality.data?.hops ?? [];

  return (
    <>
      {/* 全机队网络吞吐总览：所有机器的网卡汇总对 XRAY 承载。放在链路各节之前作为整体背景。 */}
      <FleetNetPanel />
      <E2eBand chains={chains} pending={probes.isPending} />
      <E2eSection chains={chains} pending={probes.isPending} error={probes.error} />
      {/* 排在 MTU 之前：MTU 表示该链路单次可通过的最大包长，属于条件；
          本类表示该链路当前的实际带宽和瓶颈位置，属于结果。先查看结果，
          结果异常时再查看条件。 */}
      <LinkQualitySection hops={hops} />
      <MtuSection nodes={nodes} defaultMtu={default_mtu} />
      <PairProbes links={links} />
    </>
  );
}

/** 逐跳链路质量。标识转换为机器名，与本页其他位置一致。 */
function LinkQualitySection({ hops }: { hops: import('../api').HopLinkView[] }) {
  const nodeName = useNodeNames();
  return <HopLinkTable hops={hops} nodeName={nodeName} />;
}

// 一句话的结论。它是本页最需要优先呈现的内容——此前它位于第三张表的第四列，
// 只显示「对不上」，需要逐行查看才能定位。
//
// 三档分别表述，不合并：「不通」表示故障，「出口不符」表示连通但未穿过完整的链
// （通常从链头直接出网），后者发生在每一跳都连通、编译无警告的情况下，
// 是该类探测存在的主要原因。
function E2eBand({ chains, pending }: { chains: E2eProbeItem[]; pending: boolean }) {
  if (pending || chains.length === 0) return null;
  const down = chains.filter(c => toneOf(c) === 'down');
  const odd = chains.filter(c => c.status === 'ok' && c.exit_verdict === 'mismatch');
  /* 出口 IP 只有一个取值时直接显示：多条链指向同一地址时，该地址本身即是排查线索。 */
  const oddIps = [...new Set(odd.map(c => c.exit_ip).filter(Boolean))];
  /* 「没有对不上的」不等于「都对得上」。出口机器带 NAT 时核对根本没跑，说成核对通过
     就是把一次没做的检查报成做过了——而 NAT 恰恰是这里最常见的情形。 */
  const unchecked = chains.filter(c => c.status === 'ok' && c.exit_verdict === 'unknown');

  if (down.length === 0 && odd.length === 0) {
    return (
      <div className="lk-band">
        <span className="lk-band-dot ok" />
        <span className="lk-band-head">
          {unchecked.length === 0
            ? `${chains.length} 条链都通，出口也都对得上`
            : `${chains.length} 条链都通，${
                unchecked.length === chains.length ? '出口都' : `其中 ${unchecked.length} 条出口`
              }核对不了`}
        </span>
      </div>
    );
  }
  return (
    <div className={`lk-band ${down.length ? 'bad' : 'warn'}`}>
      <span className={`lk-band-dot ${down.length ? 'bad' : 'warn'}`} />
      <span className="lk-band-head">
        {down.length > 0
          ? `${down.length} 条链不通`
          : `${chains.length} 条链都通，但${odd.length === chains.length ? '出口全都' : `有 ${odd.length} 条出口`}对不上`}
      </span>
      <span className="lk-band-why">
        {down.length > 0 ? (
          <>断在这几条：{down.map(c => c.chain_name).join('、')}。</>
        ) : (
          <>
            {oddIps.length === 1 ? (
              <>
                出口 IP 都是 <b className="mono">{oddIps[0]}</b>
                {odd[0]?.exit_loc ? `（${odd[0].exit_loc}）` : ''}——那不是这些链的出口机器。
              </>
            ) : (
              <>出口 IP 不是这些链的出口机器。</>
            )}{' '}
            流量多半从链头就直接出网了。每跳的计数器照样在涨、编译也没有警告， 所以只有这一格看得见。
          </>
        )}
      </span>
    </div>
  );
}

// 端到端探测。火花线在本页的作用高于单条链的详情页：横向对比多条链可直接看出
// 某条链从第几次开始变慢——该情况通常由某次发布导致，而非网络原因。
function E2eSection({ chains, pending, error }: { chains: E2eProbeItem[]; pending: boolean; error: unknown }) {
  const nameOf = useNodeNames();
  return (
    <div className="lk-sec">
      <header>
        <h4>端到端</h4>
        <span className="hint">由链头为每条链发起一次探测</span>
      </header>
      {pending ? (
        <Loading />
      ) : error ? (
        <ErrorBox error={error} />
      ) : chains.length === 0 ? (
        <Empty>还没有链被探过。链头的 agent 每轮自己探，刚建好的链要等一会儿。</Empty>
      ) : (
        <table className="tbl cards lk-t">
          <thead>
            <tr>
              <th>端到端</th>
              <th>链</th>
              <th>链头</th>
              <th>出口地址</th>
              <th>核对</th>
              <th>最近</th>
              <th>探于</th>
            </tr>
          </thead>
          <tbody>
            {chains.map(c => (
              <tr key={`${c.app_id}/${c.chain_id}`}>
                <td data-label="端到端">
                  <ProbeBadge item={c} />
                </td>
                <td data-label="链">
                  {c.chain_name} <span className="dim mono">{c.chain_id}</span>
                </td>
                <td data-label="链头" title={c.node_id}>
                  {nameOf(c.node_id)}
                </td>
                <td data-label="出口地址" className="mono dim">
                  {c.exit_ip ?? '—'}
                  {c.exit_loc && ` ${c.exit_loc}`}
                </td>
                <td data-label="核对">
                  <ExitVerdict item={c} />
                </td>
                <td data-label="最近">
                  <ProbeSpark samples={c.samples} />
                </td>
                <td data-label="探于" className="dim">
                  <Ago at={c.probed_at} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <p className="note lk-note">
        探测走的是<b>与用户相同的路径</b>：链头使用一份编译期派生的隐藏凭据连接自己的入口，穿过整条链到达设置中
        指定的落点。REALITY 参数、路由规则、出口出网都在这一次探测的覆盖范围内，而这三项出问题时，下面两块 都无法察觉。
      </p>
    </div>
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
    <div className="lk-sec">
      <header>
        <h4>节点 MTU</h4>
        <span className="hint">全局默认 {defaultMtu} · 在机器详情的 WIREGUARD 卡中修改</span>
      </header>
      {nodes.length === 0 ? (
        <Empty>还没有机器上报探测结果。</Empty>
      ) : (
        <table className="tbl cards lk-t">
          <thead>
            <tr>
              <th>机器</th>
              <th>生效</th>
              <th>建议</th>
              <th>最窄那条路</th>
              <th>没探通</th>
              <th>判断</th>
            </tr>
          </thead>
          <tbody>
            {nodes.map(n => {
              const v = verdict(n);
              return (
                <tr key={n.node_id}>
                  <td data-label="机器" title={n.node_id}>
                    {nameOf(n.node_id)}
                  </td>
                  <td data-label="生效">
                    <span className="mono">{n.current_mtu}</span>{' '}
                    <span className={`st ${n.overridden ? 'st-gold' : 'st-skipped'}`}>
                      {n.overridden ? '本机设的' : '跟全局'}
                    </span>
                  </td>
                  <td data-label="建议" className="mono">
                    {n.suggested_mtu ?? '—'}
                  </td>
                  <td data-label="最窄" className="dim" title={n.tightest_peer ?? ''}>
                    {n.tightest_peer ? nameOf(n.tightest_peer) : '—'}
                  </td>
                  <td data-label="没探通" className="mono">
                    {n.inconclusive || '—'}
                  </td>
                  <td data-label="判断">
                    <span className={`st ${v.cls}`}>{v.text}</span>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

// 逐对探测。默认折叠：n 台机器对应 n×(n-1) 条路径，八台即 56 条，而多数情况下
// 它们的读数完全相同。摘要行表明是否存在异常，需要查看原始读数时再展开。
function PairProbes({ links }: { links: LinkMtuItem[] }) {
  const bad = links.filter(l => l.status !== 'ok');
  /* 所有路径 MTU 相同时显示为一个数值——这是常态，显示后即可确定无需展开。 */
  const mtus = [...new Set(links.filter(l => l.path_mtu != null).map(l => l.path_mtu))];

  if (links.length === 0) {
    return (
      <div className="lk-sec">
        <header>
          <h4>逐对探测</h4>
        </header>
        <Empty>还没有探测结果。</Empty>
      </div>
    );
  }
  return (
    <details className="lk-fold lk-sec">
      <summary>
        <span className={`lk-dot ${bad.length ? 'warn' : 'ok'}`} />
        逐对探测 ·{' '}
        {bad.length > 0 ? (
          <>
            <b>
              {links.length} 条里 {bad.length} 条没探通
            </b>
          </>
        ) : (
          <>
            <b>{links.length} 条全通</b>
            {mtus.length === 1 && <>，路径 MTU 一律 {mtus[0]}</>}
          </>
        )}
        <span className="sum">agent 上报</span>
      </summary>
      <div className="b">
        <table className="tbl cards lk-t">
          <thead>
            <tr>
              <th>从</th>
              <th>探的落点</th>
              <th>状态</th>
              <th>path MTU</th>
              <th>建议 wg MTU</th>
              <th>探于</th>
            </tr>
          </thead>
          <tbody>
            {links.map(l => (
              <tr key={`${l.node_id}>${l.peer_node_id}`}>
                <td data-label="从" className="mono">
                  {l.node_id} <span className="dim">→</span> {l.peer_node_id}
                </td>
                <td data-label="落点" className="mono dim">
                  {l.endpoint_host}
                </td>
                <td data-label="状态">
                  <span className={`st ${l.status === 'ok' ? 'st-succeeded' : 'st-warn'}`}>
                    {STATUS_LABEL[l.status] ?? l.status}
                  </span>
                </td>
                <td data-label="path MTU" className="mono">
                  {l.path_mtu ?? '—'}
                </td>
                <td data-label="建议" className="mono">
                  {l.suggested_wg_mtu ?? '—'}
                </td>
                <td data-label="探于" className="dim">
                  <Ago at={l.probed_at} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        <p className="note lk-note">探测的是 underlay 落点，不是 overlay。建议值 = 路径 MTU − wg 封装开销。</p>
      </div>
    </details>
  );
}
