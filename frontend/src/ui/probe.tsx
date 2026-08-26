// 端到端探测的展示组件。四个位置使用同一份判定：项目页的链行、链详情的结论横幅、
// 链路页的总览表、机器详情。判定分散实现时，同一条链在两个页面上会显示为不同的
// 健康状态，无法确定以哪个为准。
//
// ## 使用四档而非两档的原因
//
// 「连通但出口 IP 不符」必须作为独立一档。它表示流量未穿过完整的链（通常从链头直接
// 出网），而此时规则表合法、每一跳都连通、编译无警告——静态校验无法发现该情况。
// 归入成功会隐藏该功能最需要报告的状态；归入失败会导致排查一条实际连通的链。
//
// 另有第五档「未探测」：它与探测后不通完全不同，合并后新建的链会持续显示为故障。

import type { E2eExitVerdict, E2eProbeItem, E2eProbeSample, E2eProbeStatus } from '../api';

export type ProbeTone = 'ok' | 'slow' | 'odd' | 'down' | 'none';

// 超过该值标为黄色。它不表示故障，而表示需要关注——跨洲链路本身延迟较高，
// 因此它只改变颜色，不改变结论。
const SLOW_MS = 300;

/** 火花线最多画几根柱子。
 *
 * 接口返回的是最近六小时的全部样本（store 的 `SAMPLE_WINDOW_SECS`），按 60 秒的探测间隔
 * 是三百多个。而 `.spark i` 是定宽 4px 加 2px 间隔，三百多根排下来接近两千像素——
 * `.probe-face` 的第三列是 `auto`，撑到多宽就多宽，于是整块溢出容器；`links.tsx` 里那一列
 * 更窄，溢得更多。
 *
 * 20 是保留策略改成时间窗口之前的条数（store 里的 `SAMPLE_KEEP`），也就是这条火花线本来
 * 就是按这个数排版的：20 根合计 118px，横幅和表格列都放得下。
 *
 * 取最后 20 个而不是等距抽样：这条线读的是「最近有没有变慢」，抽样会把一次尖峰抽没，
 * 而尖峰正是要看的东西。六小时窗口对这个组件是多余的数据，不是它要表达的跨度——
 * 真按时间轴铺开是另一种画法（见 store 里 `samples` 字段的注释），不是加一个上限能做到的。 */
const SPARK_BARS = 20;

// 标签上的文字。此处使用短词——它需要放入列表的一格，无法容纳完整句子。
// 横幅处使用完整的表述（`headline`），两者不可互换。
const STATUS_TEXT: Record<E2eProbeStatus, string> = {
  ok: '通',
  'handshake-failed': '连不上入口',
  'chain-broken': '链上断了',
  timeout: '超时',
  unsupported: '探不了',
};

// 一句话的结论，给出状态和事实，不给出推断。
//
// 「连通，但出口 IP 显示是 1.2.3.4（SG）」——读取后可直接获得用于核对的信息。
// 此处此前的表述是「已连通，但不是从该链的出口出网的」，那是一个推断结论：
// 它已完成推理过程，而推理依据（IP 地址）被移到第二行。
// 只提供结论时无法验证；提供 IP 时可自行查询该地址对应的机器。
//
// 推断和后续处理方式放在第二行（`ProbeBanner` 的 `why`），该处空间充足。
function exitText(item: E2eProbeItem): string {
  const ip = item.exit_ip ?? '?';
  return item.exit_loc ? `${ip}（${item.exit_loc}）` : ip;
}

export function headline(item: E2eProbeItem): string {
  if (item.status !== 'ok') {
    // 不通的各档中，需要说明的是中断的环节，而非 agent 上报的完整原因——
    // 原因放在第二行。`unsupported` 不归入不通：它表示该机器无法执行探测，
    // 与链本身的状态无关，合并后会导致排查一条正常的链。
    return item.status === 'unsupported' ? '探不了，这台机器上起不了探测' : `不通，${STATUS_TEXT[item.status]}`;
  }
  if (item.exit_verdict === 'mismatch') {
    return `连通，但出口 IP 显示是 ${exitText(item)}`;
  }
  return `连通，出口 IP 是 ${exitText(item)}`;
}

export function toneOf(item: E2eProbeItem | null | undefined): ProbeTone {
  if (!item) return 'none';
  if (item.status !== 'ok') return 'down';
  if (item.exit_verdict === 'mismatch') return 'odd';
  return (item.ttfb_ms ?? 0) > SLOW_MS ? 'slow' : 'ok';
}

/** 悬停时显示的说明：与横幅使用同一结论，其后补充细节。
 *
 *  四处共用同一个 `headline`，避免同一状态在列表和详情页中的表述不一致——
 *  不一致会被理解为两种不同的状态。 */
export function toneTitle(item: E2eProbeItem | null | undefined): string {
  if (!item) return '还没探过这条链';
  const head = headline(item);
  if (item.status !== 'ok') {
    return `${head}｜${item.detail ?? '没有细节'}`;
  }
  return `${head}｜端到端 ${item.ttfb_ms ?? '?'}ms`;
}

/** 一个状态点加一个数值。这是该功能的基础组件，四处都使用它。 */
export function ProbeBadge({ item, size }: { item: E2eProbeItem | null | undefined; size?: 'lg' }) {
  const tone = toneOf(item);
  return (
    <span className={`pb pb-${tone}${size === 'lg' ? ' pb-lg' : ''}`} title={toneTitle(item)}>
      <i className="dot" />
      <span className="ms">
        {tone === 'none' ? (
          '没探过'
        ) : item && item.status !== 'ok' ? (
          // 失败时不显示毫秒数：该值是超时时间，与链路速度无关，显示后会被与正常值比较。
          // 显示中断环节更有价值。
          STATUS_TEXT[item.status]
        ) : (
          <>
            {item?.ttfb_ms ?? '?'}
            <u>ms</u>
          </>
        )}
      </span>
    </span>
  );
}

// 火花线：最近 N 次的结果。用于观察波动情况而非具体数值——因此不绘制刻度和坐标轴。
//
// 它在总览页的作用高于单条链的页面：横向对比多条链可直接看出某条链从第几次开始变慢，
// 该情况通常由某次发布导致而非网络原因。该判断在单条链的页面上无法得出。
export function ProbeSpark({ samples }: { samples: E2eProbeSample[] }) {
  if (samples.length === 0) return <span className="dim">—</span>;
  const shown = samples.slice(-SPARK_BARS);
  // 按最慢的一次归一化，且只看画出来的这些。拿整个六小时窗口的峰值归一化时，六小时前的
  // 一次尖峰会把最近二十次全压到贴底的平线——而这条线正是用来看最近这二十次的波动的。
  const peak = Math.max(...shown.map(s => s.ttfb_ms ?? 0), 1);
  return (
    <span className="spark" title={`最近 ${shown.length} 次`}>
      {shown.map(s => {
        const failed = s.status !== 'ok';
        const height = failed ? 100 : Math.max(12, ((s.ttfb_ms ?? 0) / peak) * 100);
        return (
          <i
            key={s.probed_at}
            className={failed ? 'down' : (s.ttfb_ms ?? 0) > SLOW_MS ? 'slow' : ''}
            style={{ height: `${height}%` }}
            title={`${s.probed_at.slice(0, 19)} · ${failed ? STATUS_TEXT[s.status] : `${s.ttfb_ms}ms`}`}
          />
        );
      })}
    </span>
  );
}

/** 出口核对字段。三档各表示不同状态，不压缩为单一标记。
 *
 *  这是表格中的一格，只显示结论——具体的 IP 在相邻列中，
 *  同一张表内重复显示会占用一列宽度。悬停可查看完整说明。 */
export function ExitVerdict({ item }: { item: E2eProbeItem }) {
  const map: Record<E2eExitVerdict, { cls: string; text: string }> = {
    match: { cls: 'st-succeeded', text: '一致' },
    mismatch: { cls: 'st-gold', text: '对不上' },
    unknown: { cls: 'st-skipped', text: '核对不了' },
  };
  if (item.status !== 'ok') return <span className="dim">—</span>;
  const v = map[item.exit_verdict];
  return (
    <span className={`st ${v.cls}`} title={toneTitle(item)}>
      {v.text}
    </span>
  );
}

// 链详情顶部的结论横幅，是进入该页后首先看到的内容。
//
// 它与本页的接入面、XRAY 链路使用同一结构（`.blk`：一条带底色的标题栏加下方内容），
// 区别在于标题和底色使用语义色。此前它是全页唯一有底色的元素——上方是无样式标题、
// 下方是小字加分隔线的节，加上 `.callout` 自带的 `max-width:78ch` 使其宽度小于下方所有内容、
// 右边缘不对齐，无论如何排布都显得是额外叠加的。需要修改的不是底色（底色是本页的主要
// 视觉元素），而是其周围缺少同类结构。
//
// 内部分为三列：左侧是大号延迟数值、中间是两行文字（上行是状态和事实，下行是推断和依据）、
// 右侧是火花线。延迟使用大号字体，因为它是本屏唯一需要与历史值比较的量，其余都是判定结果。
export function ProbeBanner({ item }: { item: E2eProbeItem | null | undefined }) {
  const tone = toneOf(item);
  // slow 与 ok 使用同一颜色：它表示延迟较高但结果正确，与出口不符不属于同一严重程度，
  // 而延迟情况已由数值和火花线中的柱高表示。
  const toneCls = tone === 'down' ? 'tone-err' : tone === 'odd' ? 'tone-odd' : 'tone-ok';
  if (!item) {
    return (
      <div className="blk">
        <div className="blk-hd">连通性 · 还没探过</div>
        <div className="blk-bd">
          <div className="probe-face">
            <span className="probe-lat">
              <span className="n" style={{ fontSize: 16, color: 'var(--ink-4)' }}>
                未知
              </span>
            </span>
            <span className="probe-say">
              <span className="l1">还没探过这条链。</span>
              <span className="l2">链头的 agent 每轮会自己探一次，刚建好的链要等一会儿。</span>
            </span>
          </div>
        </div>
      </div>
    );
  }

  /* 标题栏中的词是结论，正文第一行是事实。标题需要足够简短。 */
  const verdict =
    item.status !== 'ok'
      ? STATUS_TEXT[item.status]
      : item.exit_verdict === 'mismatch'
        ? '出口对不上'
        : item.exit_verdict === 'unknown'
          ? '出口核对不了'
          : '出口一致';

  // 上行是状态和事实（`headline`），下行是推断和后续处理方式。
  // 顺序相反时需要先接受结论再查找依据。
  const why =
    item.status !== 'ok'
      ? (item.detail ?? '没有细节')
      : item.exit_verdict === 'mismatch'
        ? /* 只陈述事实，不推测原因。此处此前还有一句「检查链头的规则表是否遗漏转发，
             流量可能在该处直接出网」——那只是**一种**可能，流量可能在任意一跳出网。
             将推测表述为建议会导致优先排查一个未必相关的位置。 */
          '这不是这条链的出口机器。'
        : item.exit_verdict === 'unknown'
          ? /* 三种情况都无法核对：出口位于 NAT 之后（v4 或 v6 任一在 NAT 后即无法核对，
               因为从哪一族出网由落点和路由决定，探测不去猜）；模型中没有公网地址；
               地址会变化（PPPoE 拨号、动态分配），模型中的值已过期。
               最后一种更难发现——该字段有取值，但取值不正确。 */
            '核对不了这个 IP 是不是这条链的出口：出口机器在 NAT 后面、没有公网地址，或地址会变。'
          : '跟这条链的出口机器对得上。';

  return (
    <div className={`blk ${toneCls}`}>
      <div className="blk-hd">连通性 · {verdict}</div>
      <div className="blk-bd">
        <div className="probe-face">
          {/* 不通一档左侧显示文字而非数值：该毫秒数是超时时间，与链路速度无关，
              显示后会被与正常值比较。 */}
          <span className="probe-lat">
            {item.status === 'ok' ? (
              <>
                <span className="n">{item.ttfb_ms ?? '?'}</span>
                <span className="u">ms</span>
              </>
            ) : (
              <span className="n">不通</span>
            )}
          </span>
          <span className="probe-say">
            <span className="l1">{headline(item)}</span>
            <span className="l2">
              {why}
              <span className="when">{item.probed_at.slice(0, 19)}</span>
            </span>
          </span>
          <span className="probe-rt">
            <ProbeSpark samples={item.samples} />
          </span>
        </div>
      </div>
    </div>
  );
}

/** 按链 id 建立索引，四处都需要执行该查找。 */
export function byChain(items: E2eProbeItem[] | undefined): Map<string, E2eProbeItem> {
  return new Map((items ?? []).map(item => [item.chain_id, item]));
}
