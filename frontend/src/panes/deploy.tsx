import { Fragment, useState, useSyncExternalStore } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  discardPendingChanges,
  cancelDeployment,
  cancelRollbackDeployment,
  confirmWave,
  createDeployment,
  createRollback,
  fetchArtifactContent,
  fetchDeployment,
  fetchDeployments,
  fetchRevisions,
  haltDeployment,
  isolateDeploymentTarget,
  planDeployment,
  retryTarget,
  verifyDeployment,
  type ArtifactIndexEntry,
  type DeploymentListItem,
  type DeploymentTargetDetail,
  type PlannedAction,
  type PlannedTarget,
} from '../api';
import { draft } from '../draft';
import { AgentReleaseSection } from './agent-release';
import { entryId, useRevisionDiff } from '../forge/artifacts';
import { artifactFile, artifactFmt, countChanges, diffLines, highlight } from '../forge/diff';
import { can, useSession } from '../session';
import { Ago, Confirm, Empty, ErrorBox, Loading, STATUS_TEXT, Status } from '../ui/bits';
import { PanelTitle } from '../ui/icons';
import { useNodeNames } from '../ui/node-name';
import { randomKey } from '../ui/platform';
import { wm, type CrumbSeg, type Win } from '../wm/store';
import { useCrumb } from '../wm/crumb';

type Drill =
  | { p: 'list' }
  // key 是本次预览的幂等键：进入预览时生成一次，重复点击「创建」返回同一条 deployment，
  // 不会因重复点击创建出两条。每次重新生成则失去幂等性。
  | { p: 'plan'; revision?: number; key?: string }
  | { p: 'detail'; id: number };

/* 动作决定是否具有破坏性，破坏性决定波次划分 */
const ACTION_NOTE: Record<PlannedAction, string> = {
  'apply-phantun': '同步 phantun · fake TCP 封装进程变更',
  'apply-hy2-port-hop': '装端口跳转 · 只改 nft，进程不动，没人掉线',
  'sync-grants': '同步授权 · 进程不动，没人掉线',
  'apply-wire-guard': '同步 WireGuard · 已有链路不断',
  'apply-xray': '重写 xray · 重启，断这台上所有连接',
  'disable-phantun': '停用 phantun · 依赖 fake TCP 的链路会断',
  'disable-hy2-port-hop': '撤端口跳转 · 客户端只剩落点那一个口能连',
  'disable-wire-guard': '停用 WireGuard · 经过它的链路全断',
  'disable-xray': '停用 xray · 这台上所有连接断',
};

// 失败处理建议：deployment target 的 error 是 agent 侧的自由文本（服务端没有结构化
// 错误码，deployment.rs 原样透传 agent 的 report.error），只能按子串匹配推断。
// 顺序即优先级，第一条匹配的生效；均不匹配时使用兜底建议「先查看日志」。
//
// 由于是推断，表述需要限定在观察层面：先复述报错内容，再给出常见原因。
// 使用断言式表述的代价是按其排查一个不存在的问题，返回后发现报错没有变化——
// 此后该提示不再被信任，后续匹配正确的条目也失去作用。
const FAILURE_GUIDES: { re: RegExp; guide: string }[] = [
  {
    re: /Exec format error|cannot execute|\(os error 8\)/,
    guide:
      'agent 二进制在这台上跑不起来，多半是架构不符。控制面内嵌 x86_64 和 aarch64 两种，先在这台上看 uname -m；两种都不是才需要自己编一个，用 --agent-bin-url 重跑 install.sh。',
  },
  {
    re: /desired request failed: HTTP 4|probe targets request failed: HTTP 4|link probe failed: HTTP 4/,
    guide:
      '控制面拒了这台的请求。最常见是 node token 失效或权限不够——到节点页「重签 token」再重试；不是的话，agent 日志里有那条请求的完整响应。',
  },
  {
    re: /HTTP 5\d\d/,
    guide: '控制面内部错误。等一会儿重试；反复失败查控制面日志。',
  },
  {
    re: /connection refused|timed out|timeout|temporary failure/,
    guide: '这台机器和控制面之间没接通。查它的出网、控制面在不在，以及安装时配的 BROCADE_AGENT_SERVER。',
  },
  {
    re: /permission denied|Operation not permitted/,
    guide:
      'agent 动不了系统。最常见是装的时候没以 root 跑 install.sh（--apply linux 要求 root）；也可能是容器或内核策略挡住了这一步。',
  },
  {
    re: /不在这台机器上/,
    guide:
      '这台要用伪 TCP，而 agent 说 phantun 不在这台机器上。重跑 install.sh 补装，或让控制面配好 BROCADE_PHANTUN_SERVER_URL / BROCADE_PHANTUN_CLIENT_URL 再重试。',
  },
  {
    re: /健康检查有/,
    guide:
      '收敛后自检没过。上机器看 journalctl -u brocade-agent -n 50，确认具体哪一项没通过（端口没监听、xray 没起来、wg 握手失败）。',
  },
];

const guideFor = (error: string) =>
  FAILURE_GUIDES.find(g => g.re.test(error))?.guide ??
  /* 均不匹配时使用兜底建议：agent 的报错是自由文本，给出错误的推断比不推断更差
   （deployment.md 中没有错误码） */
  '上机器看 journalctl -u brocade-agent -n 50，找到报错前的现场；修好后逐台 retry。';

/* 将当前下钻层级转换为外壳顶部的面包屑。顶层那一段（「发布」）由外壳补全。 */
const crumbOf = (d: Drill): CrumbSeg[] => {
  switch (d.p) {
    case 'list':
      return [];
    case 'plan':
      return [{ label: d.revision == null ? '计划预览' : `计划预览 · 修订 ${d.revision}` }];
    case 'detail':
      return [{ label: `#${d.id}` }];
  }
};

export function DeployPane({ win }: { win: Win }) {
  const { who } = useSession();
  const drill = (win.data.drill as Drill | undefined) ?? { p: 'list' };
  const go = (d: Drill) => wm.setData(win.id, { ...win.data, drill: d });
  useCrumb(win, crumbOf(drill));

  if (drill.p === 'plan') {
    /* 从其他位置进入的预览（如纳管向导）没有携带幂等键，此处补全一次并写入窗口状态 */
    if (!drill.key) {
      const key = randomKey();
      wm.setData(win.id, { ...win.data, drill: { ...drill, key } });
      return <Loading />;
    }
    return <PlanPreview revision={drill.revision} idempotencyKey={drill.key} go={go} />;
  }
  // key 按 deployment 确定：Detail 中的 ask / confirmedWave 是该条发布的状态，
  // 切换发布（如回滚跳转到新工单）时应重置，不能带入下一条。
  if (drill.p === 'detail') return <Detail key={drill.id} id={drill.id} go={go} />;

  /* 顶层两段并排，版式与设置页共用（.cardpage / .duo）。目录取消了：两段而已，
     一列目录占掉的宽度比它省下的滚动还多。下钻页（计划预览、发布详情）自带标题栏。

     配置发布在左：它是这一页的主任务——产生工单、分波推送、要人确认，进行中的发布还会
     置顶为一张卡。agent 更新在右：批准之后机器自行替换，不产生工单，看一眼台数即可。
     两段各自的读数写在自己的标题栏里（「N 台待发布 · 修订 N」「vX 可发 · N 台未替换」），
     此前那两枚挂在目录条目上的角标因此没有丢。 */
  return (
    <div className="cardpage">
      <div className="duo">
        <div className="col">
          <ConfigSection go={go} />
        </div>
        <div className="col">
          <AgentReleaseSection editable={can(who.role, 'system')} />
        </div>
      </div>
    </div>
  );
}

// 协议中的取值是 config / grants，界面按发起方式表述：变更单由人工发起，需要关注分波和确认；
// 自动化授权单由权限操作或配额执行自动发起，只增删运行时的名单。
const KIND_LABEL = { all: '全部', config: '变更单', grants: '自动化授权单' } as const;

// 发布列表通常由服务端按 id 倒序返回，但基线判定不能依赖调用方排序。回滚也会产生更大的
// 修订号，因此比较的是 deployment id（实际发生顺序），返回该次发布引用的修订。
export function latestSuccessfulRevision(items: DeploymentListItem[]): number | null {
  const latest = items.reduce<DeploymentListItem | null>(
    (best, item) => (item.status === 'succeeded' && (best === null || item.id > best.id) ? item : best),
    null,
  );
  return latest?.revision_id ?? null;
}

function ConfigSection({ go }: { go: (d: Drill) => void }) {
  const [kind, setKind] = useState<'all' | 'config' | 'grants'>('all');

  // ForgeShell 全局轮询 ['deployments']（顶栏需要常驻显示发布状态），
  // 因此不筛选时与其使用同一查询键共享缓存，筛选时使用独立的键。
  const list = useQuery({
    queryKey: kind === 'all' ? ['deployments'] : ['deployments', kind],
    queryFn: () => fetchDeployments(kind === 'all' ? undefined : kind),
  });
  // 两类发布各自独立限流（deployments_single_flight 按 kind 分别持有），因此当前是否可发布
  // 需要分别判断：变更单进行中时授权单仍可下发。该判断不受筛选条件影响。
  const all = useQuery({
    queryKey: ['deployments'],
    queryFn: () => fetchDeployments(),
  });

  // 服务端拒绝不涉及任何机器的发布，因此此处同样应提前拦截。
  // 查询键与 ForgeShell 的 verify 一致，共享缓存不产生额外请求。
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const verify = useQuery({
    queryKey: ['deployment-verify', current],
    queryFn: () => verifyDeployment({ revision_id: current! }),
    enabled: current != null,
  });

  // 历史、当前活动单和当前修订共同决定本段按钮是否可用。缺一项时继续渲染会把“未知”
  // 误当成“没有活动发布”或“已收敛”。
  if (list.isPending || all.isPending || revisions.isPending) return <Loading />;
  if (list.error || all.error || revisions.error) {
    return <ErrorBox error={list.error ?? all.error ?? revisions.error} />;
  }

  const items = list.data.deployments;
  const activeOf = (k: 'config' | 'grants') => (all.data?.deployments ?? []).find(d => d.active && d.kind === k);
  return (
    <section className="panel titled" id="dp-config">
      <header>
        <PanelTitle of="deploy">配置发布</PanelTitle>
        {/* 段抬头的读数：进入本段首先需要了解的是待发布的机器数量。
            两类发布的忙闲状态排在其后——它们已有更显著的表达方式（进行中的发布会
            置顶为一张卡），此处只在确实有进行中的发布时才显示。 */}
        <span className="hint">
          {current == null ? (
            '还没有可发布修订'
          ) : verify.error ? (
            '待发布状态读取失败'
          ) : verify.data?.summary.changed_targets ? (
            <>
              <b>{verify.data.summary.changed_targets} 台</b> 待发布
              {current != null && ` · 修订 ${current}`}
            </>
          ) : verify.isPending ? (
            '检查待发布…'
          ) : (
            `已收敛${current != null ? ` · 修订 ${current}` : ''}`
          )}
          {(['config', 'grants'] as const).map(k => {
            const on = activeOf(k);
            return on ? (
              <span key={k} style={{ marginLeft: 12, color: 'var(--gold)' }}>
                {k === 'config' ? '变更单' : '自动化授权单'} #{on.id} 进行中
              </span>
            ) : null;
          })}
        </span>
        <span className="sp" />
        {/* 筛选。默认为「全部」——按时间顺序查看历史记录是主要用法，
            按类型筛选只在查找特定类型时使用。
            使用下拉框而非一排按钮：三个按钮中有两个始终未选中，占用的宽度与真正需要
            点击的主操作相同；而该行右端才是本页的主要操作。 */}
        <select
          /* `words`：此处内容是中文词语而非取值，使用 sans——等宽字体的中文字形比相邻按钮窄一档 */
          className="f words"
          value={kind}
          aria-label="按类型筛选"
          onChange={e => setKind(e.target.value as 'all' | 'config' | 'grants')}
        >
          {(['all', 'config', 'grants'] as const).map(k => (
            <option key={k} value={k}>
              {KIND_LABEL[k]}
            </option>
          ))}
        </select>
        <PlanButton
          pending={current == null || verify.isPending || !!verify.error}
          changed={verify.data?.summary.changed_targets}
          onClick={() => go({ p: 'plan', key: randomKey() })}
        />
      </header>
      {verify.error && <ErrorBox error={verify.error} />}
      {/* 进行中的发布置顶：本页的三项内容中只有它有时效性。
          它同时保留在下方的历史记录中——历史记录按时间排列，此处表示当前状态。 */}
      {items
        .filter(d => d.active)
        .map(d => (
          <LiveDeployment key={`live-${d.id}`} item={d} go={go} />
        ))}

      {/* 筛选后为空与确实没有任何记录是两种情况，不应都提示去执行计划预览 */}
      {items.length === 0 ? (
        <Empty>
          {kind === 'grants'
            ? '还没有自动化授权单。修改授权、停用或启用用户、轮换 UUID，以及额度自动调整都会落在这里。'
            : kind === 'config'
              ? '还没有变更单。'
              : '还没有发布记录。'}
        </Empty>
      ) : (
        <div className="dp-list">
          {items.map((d, i) => {
            // 按天分组：连续列出时，「25 分钟前」和「4 小时前」之间是否跨天无法判断。
            // 组标题只在跨天的那一行出现。
            const day = dayKey(d.created_at);
            const newDay = i === 0 || day !== dayKey(items[i - 1].created_at);
            // 同一修订发布过多次时标出次序。列表按时间倒序排列，
            // 因此需要向后统计（更早的记录）。
            const tries = items.filter(x => x.revision_id === d.revision_id && x.kind === d.kind);
            const nth = tries.length > 1 ? tries.length - tries.indexOf(d) : 0;
            // 新发布的备注由服务端生成为「变更内容 · 机器数」（store 的 `default_note`），
            // 每条不同，适合作为主要内容显示。
            // 保留该正则只为识别历史数据：此项改动之前，控制台为每条发布填入的是
            // 「console · 修订 N」——重复了右侧已显示的编号，全表各行内容相同。
            // 这些记录仍在库中（备注是历史记录，不回填），识别后降级为灰色副标题。
            const legacy = new RegExp(`^\\S+ · 修订 ${d.revision_id}$`);
            const written = d.note && !legacy.test(d.note) ? d.note : null;
            return (
              <Fragment key={d.id}>
                {newDay && <div className="dp-day">{day}</div>}
                <div
                  role="button"
                  tabIndex={0}
                  className="dp-row"
                  onClick={() => go({ p: 'detail', id: d.id })}
                  onKeyDown={e => {
                    if (e.target !== e.currentTarget) return;
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault();
                      go({ p: 'detail', id: d.id });
                    }
                  }}
                >
                  {/* 编号移至最左作为标识——此前它位于副标题中且字号最小，而主位显示的是
                      「console · 修订 N」：发起方全表相同，修订号重复显示。 */}
                  <span className="id">#{d.id}</span>
                  <span className="what">
                    <b>{written ?? `修订 ${d.revision_id}`}</b>
                    <span className="sub">
                      {written ? `修订 ${d.revision_id}` : ''}
                      {nth > 1 ? `${written ? ' · ' : ''}第 ${nth} 次` : ''}
                    </span>
                    {/* 自动化授权单单独标记：它不重启进程、不重建隧道，代价比变更单低一个数量级。
                        变更单是常态，不加标记。 */}
                    {d.kind === 'grants' && (
                      <span className="st st-skipped" title="后台自己发的：只同步名单，不重启进程、不断线">
                        自动化授权
                      </span>
                    )}
                    {d.rollback_of_deployment_id != null && (
                      <span className="st st-warn">回滚到 #{d.rollback_of_deployment_id}</span>
                    )}
                    {d.sync_of_deployment_id != null && (
                      <span className="st st-gold">补推 #{d.sync_of_deployment_id}</span>
                    )}
                  </span>
                  <span className="stat">
                    <DeployStatus item={d} />
                  </span>
                  <When at={d.created_at} />
                </div>
              </Fragment>
            );
          })}
        </div>
      )}
    </section>
  );
}

// 存在草稿时不禁用：verify 查询的是已提交的修订，此时结果为 0 属于正常，
// 而预览页的「不是当前修订」提示比禁用按钮表达得更明确。
function PlanButton({
  pending,
  changed,
  onClick,
}: {
  pending: boolean;
  changed: number | undefined;
  onClick: () => void;
}) {
  useSyncExternalStore(draft.subscribe, draft.version);
  const dirty = !draft.isEmpty();
  const nothing = !dirty && changed === 0;
  return (
    <button
      className="btn primary"
      disabled={pending || nothing}
      title={nothing ? '所有机器的产物都已经是当前修订的样子' : dirty ? '预览的是已提交的修订，不含草稿' : ''}
      onClick={onClick}
    >
      {nothing ? '无变更' : '计划预览'}
    </button>
  );
}

// 用一条进度条替代原有的「变更/总数 · 失败 · 波次」三列。
// 该条表示本次发布的目标构成，而非实时进度——列表接口只返回总数、变更数、跳过数、
// 失败数，不包含已完成数量，因此不绘制推断出的进度。
// 当前执行到哪一步由左侧的状态标签表示（running 状态带动画）。
// 本次发布涉及哪些机器不在列表中显示。
// 此处此前有一条按 `changed/total` 填充的进度条，后改为「6 台中改 1 台」加点阵——
// 两个版本都在列表中占用一整列，而它们表示的是细节，不是浏览该表时需要的信息：
// 本页需要确定的是哪一条发布、是否成功、发布时间。机器数量、跳过和失败的逐台分布
// 都在详情页中。

// 状态的显示文案。含失败的取消需要与普通取消区分：前者是人工中止，
// 后者是推送失败后的收尾，排查时属于两种不同情况。
function DeployStatus({ item }: { item: DeploymentListItem }) {
  // 「等确认」的优先级高于「推送中」：该发布确实处于 running 状态，但停止推进的原因不是
  // 机器响应慢，而是缺少人工确认。显示为「推送中」会导致继续等待，而它不会自动继续。
  const text =
    item.activation_status === 'activated' && item.settlement_status !== 'converged'
      ? `已生效 · ${item.debt_targets} 台待补偿`
      : item.activation_status === 'activated'
        ? '已生效'
        : item.status === 'succeeded' && item.activation_status === 'waiting'
          ? '执行完成 · 待生效'
          : item.awaiting_confirmation
            ? '等确认'
            : item.status === 'canceled' && item.failed_targets > 0
              ? '失败后取消'
              : (STATUS_TEXT[item.status] ?? item.status);
  const cls =
    item.activation_status === 'activated' && item.settlement_status !== 'converged'
      ? 'st-gold'
      : item.activation_status === 'activated'
        ? 'st-succeeded'
        : item.status === 'succeeded' && item.activation_status === 'waiting'
          ? 'st-gold'
          : item.failed_targets > 0
            ? 'st-halted'
            : item.awaiting_confirmation
              ? 'st-gold'
              : item.status === 'succeeded'
                ? 'st-succeeded'
                : item.status === 'running'
                  ? 'st-gold'
                  : '';
  return (
    <span
      className={`st ${cls}`}
      title={item.awaiting_confirmation ? `${item.status}：破坏性波次在等人确认` : item.status}
    >
      {text}
    </span>
  );
}

// 时间只占一行。绝对时间不再单独显示为一行小字：那会使每行高度增加一档，而该表的作用
// 在于一屏可浏览的记录数量。日期由上方按天分组的组标题表示，同一天内的具体时刻
// 写入 title（由 `Ago` 提供）——排查时悬停即可查看。
function When({ at }: { at: string }) {
  const t = Date.parse(at.endsWith('Z') || at.includes('+') ? at : `${at}Z`);
  const d = new Date(t);
  const pad = (n: number) => String(n).padStart(2, '0');
  return (
    <span className="dp-when" title={Number.isNaN(t) ? at : d.toLocaleString()}>
      {Number.isNaN(t) ? at : `${pad(d.getHours())}:${pad(d.getMinutes())}`}
    </span>
  );
}

// 按天分组的组标题。今天 / 昨天 / 具体日期——连续列出时，
// 「25 分钟前」和「4 小时前」之间是否跨天无法判断。
function dayKey(at: string): string {
  const t = Date.parse(at.endsWith('Z') || at.includes('+') ? at : `${at}Z`);
  if (Number.isNaN(t)) return '';
  const d = new Date(t);
  const midnight = new Date();
  midnight.setHours(0, 0, 0, 0);
  const days = Math.floor((midnight.getTime() - new Date(d).setHours(0, 0, 0, 0)) / 86_400_000);
  if (days <= 0) return '今天';
  if (days === 1) return '昨天';
  const pad = (n: number) => String(n).padStart(2, '0');
  return d.getFullYear() === new Date().getFullYear()
    ? `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
    : `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

// 进行中的发布：置顶为一张卡。
// 本页的三项内容中只有它有时效性——当前是否有发布在执行。卡片中显示波次，
// 波次才表示实际的分段推进（一波收敛后才推送下一波）。
function LiveDeployment({ item, go }: { item: DeploymentListItem; go: (d: Drill) => void }) {
  const waves = Math.max(1, item.max_wave + 1);
  const done = Math.max(0, Math.min(item.max_wave, waves - 1));
  return (
    <div className="dp-live">
      <div className="h">
        <b>
          #{item.id} · 修订 {item.revision_id}
        </b>
        <DeployStatus item={item} />
        <span className="note">
          第 {done + 1} 波 / 共 {waves} 波
        </span>
        <span className="sp" />
        <button className="btn sm" onClick={() => go({ p: 'detail', id: item.id })}>
          看详情
        </button>
      </div>
      <div className="waves">
        {Array.from({ length: waves }, (_, i) => (
          <i key={i} className={i < done ? 'done' : i === done ? 'now' : ''} />
        ))}
      </div>
    </div>
  );
}

function PlanPreview({
  revision,
  idempotencyKey,
  go,
}: {
  revision?: number;
  idempotencyKey: string;
  go: (d: Drill) => void;
}) {
  const { who } = useSession();
  const qc = useQueryClient();
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  /* picked 只在重新预览当前修订时设置：用户操作优先于从路由传入的 revision 参数 */
  const [picked, setPicked] = useState<number | undefined>(undefined);
  const target = picked ?? revision ?? revisions.data?.current_revision;
  const stale = revisions.data && target !== revisions.data.current_revision;

  const plan = useQuery({
    queryKey: ['plan', target],
    queryFn: () => planDeployment(target!),
    enabled: !!target,
    retry: false,
  });

  // 撤销该版本需要检查是否已有发布引用它——服务端会拒绝，但按钮本身不应显示：
  // 点击后必然报错的按钮比不提供该按钮更容易造成困惑。查询键与发布列表相同，
  // 命中缓存。
  const deployments = useQuery({ queryKey: ['deployments'], queryFn: () => fetchDeployments() });
  const [askAbort, setAskAbort] = useState(false);
  const abort = useMutation({
    mutationFn: (rev: number) => discardPendingChanges(rev),
    onSuccess: () => {
      // 模型整体回退一个版本，所有由模型派生的缓存都需要重新获取：编译视图、快照、
      // 机器列表（退役状态可能在该版本中修改）、以及 verify 的汇总结果——发布页的按钮
      // 依据它变为无可发布内容。
      for (const key of [
        ['revisions'],
        ['compile'],
        ['snapshot'],
        ['nodes'],
        ['deployments'],
        ['deployment-verify'],
        ['plan'],
      ])
        qc.invalidateQueries({ queryKey: key });
      /* 返回列表：预览的修订已被撤销，停留在本页显示的是一份已不存在的计划。 */
      go({ p: 'list' });
    },
  });
  // 基线为最近一次成功发布对应的修订。按发布 id 排序（即时间顺序），不按修订号排序：
  // 回滚发布的修订号高于它回滚的那些，按修订号排序会将基线定位到更早的模型。
  const publishedBase = latestSuccessfulRevision(deployments.data?.deployments ?? []);
  /* 该区间内存在仍然有效的发布时不能丢弃——服务端使用同一判定。 */
  const blockedByDeployment =
    publishedBase != null &&
    target != null &&
    (deployments.data?.deployments ?? []).some(
      d => d.revision_id > publishedBase && d.revision_id <= target && d.status !== 'canceled',
    );
  const canAbort =
    target != null &&
    !stale &&
    publishedBase != null &&
    publishedBase < target &&
    !blockedByDeployment &&
    can(who.role, 'system');

  const create = useMutation({
    mutationFn: () =>
      // 不填写 note：服务端会按「该修订的变更内容 · 机器数」生成备注
      // （store 的 `default_note`）。在此填写只能生成「console · 修订 71」——
      // 编号已显示在右侧，而变更内容需要查询修订备注，该数据在服务端。
      createDeployment({
        revision_id: target!,
        idempotency_key: idempotencyKey,
      }),
    onSuccess: res => {
      qc.invalidateQueries({ queryKey: ['deployments'] });
      qc.invalidateQueries({ queryKey: ['deployment-verify'] });
      go({ p: 'detail', id: res.deployment_id });
    },
  });

  if (revisions.isPending || deployments.isPending || !target || plan.isPending) return <Loading />;
  if (revisions.error || deployments.error) {
    return <ErrorBox error={revisions.error ?? deployments.error} />;
  }

  return (
    <>
      {plan.error ? (
        <ErrorBox error={plan.error} />
      ) : (
        <>
          <div className="chipline" style={{ marginBottom: 8 }}>
            <span className="st">total {plan.data.summary.total_targets}</span>
            <span className="st st-gold">changed {plan.data.summary.changed_targets}</span>
            <span className="st st-skipped">skipped {plan.data.summary.skipped_targets}</span>
            <span className={`st ${plan.data.summary.disruptive_targets ? 'st-warn' : ''}`}>
              disruptive {plan.data.summary.disruptive_targets}
            </span>
            <span className="st">max_wave {plan.data.summary.max_wave}</span>
          </div>
          {plan.data.warnings.map((w, i) => (
            <div key={i} className="callout">
              <span className="mono" style={{ color: 'var(--warn)' }}>
                {w.code}
              </span>{' '}
              · <span className="mono dim">{w.location}</span>
              <br />
              <span className="note">{w.message}</span>
            </div>
          ))}
          <PlanTargets targets={plan.data.targets} />
          {plan.data.base_revision_id != null ? (
            <>
              <div className="wavehead" style={{ marginTop: 14 }}>
                <span>变更内容 · 跟修订 {plan.data.base_revision_id} 比</span>
                <span className="rule" />
              </div>
              <ArtifactChanges
                revision={target}
                base={plan.data.base_revision_id}
                targets={plan.data.targets.filter(t => t.status !== 'skipped')}
              />
            </>
          ) : (
            <div className="callout">第一次发布，没有可比的上一版。</div>
          )}
          {stale ? (
            <div className="callout warn">
              当前修订已经是 <b className="mono">{revisions.data?.current_revision}</b>，预览的是{' '}
              <b className="mono">{target}</b>——期间模型又动过，现在创建会被拒。
              <div className="toolbar">
                <span className="sp" />
                <button className="btn" onClick={() => setPicked(revisions.data?.current_revision)}>
                  重新预览当前修订
                </button>
              </div>
            </div>
          ) : (
            <div className="callout">
              创建会带上 <b className="mono">revision_id = {target}</b>；期间模型又动过就拒绝。
            </div>
          )}
          <div className="toolbar">
            <span className="sp" />
            {/* 查看 diff 后判断该批改动有误的操作在本页完成，不应要求退出后另行查找。
                撤销的是上方 diff 的全部内容，撤销后待发布内容归零——退出预览只是关闭页面，
                改动仍保留在模型中等待发布。 */}
            {canAbort && (
              <button
                className="btn danger"
                disabled={abort.isPending}
                title={`丢弃上面这些改动：模型退回已发布的修订 ${publishedBase}，一台机器都不用动`}
                onClick={() => setAskAbort(true)}
              >
                {abort.isPending ? '撤销中…' : '撤销计划'}
              </button>
            )}
            <button
              className="btn primary"
              disabled={
                !can(who.role, 'publish') ||
                create.isPending ||
                /* 判定与服务端一致：机器均在线且产物无变化时同样无法创建发布。 */
                plan.data.summary.changed_targets === 0 ||
                !!stale
              }
              title={
                stale
                  ? '预览的修订不是当前修订，服务端会拒'
                  : plan.data.summary.changed_targets === 0
                    ? '一台都不用动'
                    : ''
              }
              onClick={() => create.mutate()}
            >
              {create.isPending ? '创建中…' : '创建变更单'}
            </button>
          </div>
          {askAbort && target != null && publishedBase != null && (
            <Confirm
              title="撤销这次计划？"
              body={
                <>
                  丢弃<b>上面这份 diff 的全部内容</b>——从已发布的修订 <b className="mono">{publishedBase}</b> 到现在的{' '}
                  <b className="mono">{target}</b> 之间 <b>{target - publishedBase}</b>{' '}
                  版改动，一次全撤。不是只撤最后一次提交。
                  <br />
                  这些改动一次都没发下去过，机器上跑的仍是修订 {publishedBase} 的产物——
                  所以撤它不动任何机器，模型退回去就已经收敛，待发的变更归零。
                  <br />
                  撤完会记一个新修订号（内容等于修订 {publishedBase}），被丢掉的那些在历史里 标成 aborted。
                </>
              }
              confirmLabel="撤销计划"
              onConfirm={() => {
                setAskAbort(false);
                abort.mutate(target);
              }}
              onCancel={() => setAskAbort(false)}
            />
          )}
          {(create.error || abort.error) && <ErrorBox error={create.error ?? abort.error} />}
        </>
      )}
    </>
  );
}

function PlanTargets({ targets }: { targets: PlannedTarget[] }) {
  const nameOf = useNodeNames();
  if (targets.length === 0) return <Empty>这次没有目标：所有机器的产物都没变。</Empty>;
  const waves = [...new Set(targets.filter(t => t.status !== 'skipped').map(t => t.wave))].sort((a, b) => a - b);
  return (
    <>
      {waves.map(w => {
        const inWave = targets.filter(t => t.status !== 'skipped' && t.wave === w);
        const disruptive = inWave.some(t => t.disruptive);
        return (
          <div key={w}>
            <div className="wavehead">
              <span>
                wave {w} · {disruptive ? '破坏性 —— 要人确认，一台一波' : '不掉线，一波推完'}
              </span>
              <span className="rule" />
            </div>
            <table className="tbl dp-wave">
              <tbody>
                {inWave.map(t => (
                  <tr key={t.node_id}>
                    <td title={t.node_id}>{nameOf(t.node_id)}</td>
                    <td>
                      {t.actions.map(a => (
                        <span key={a} className="st" style={{ fontSize: '10.5px' }}>
                          {a}
                        </span>
                      ))}
                    </td>
                    <td className="dim" style={{ fontSize: 12 }}>
                      {t.actions.map(a => ACTION_NOTE[a]).join('；')}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        );
      })}
    </>
  );
}

// 该发布对产物的修改内容。
// 产物是模型快照的纯函数，分别编译两个 revision 即可逐行比较，服务端无需额外计算。
// 基线是 base_revision_id——同类上一次成功推送的版本，而非上一个修订：
// 期间提交但未发布的修订不属于本次发布。
function ArtifactChanges({
  revision,
  base,
  targets,
}: {
  revision: number;
  base: number | null;
  // 详情页传入 DeploymentTargetDetail[]，预览页传入 PlannedTarget[]——只需要 node_id。
  // 本次发布的目标机器由调用方确定，此处只负责比较产物。
  targets: { node_id: string }[];
}) {
  const nameOf = useNodeNames();
  const { list, changed, known, pending, error } = useRevisionDiff(revision, base);

  if (base == null) {
    return <div className="callout">第一次发布，没有可比的上一版。完整内容见右侧产物栏。</div>;
  }
  if (error) return <ErrorBox error={error} />;
  if (pending || !known) return <Loading />;

  // 按机器筛选而非按产物类型筛选：这些机器上与基线不同的全部列出，包括 grants.json-rpc。
  // 它是运行时的名单，两类发布都可能包含——重启 xray 后需要重新加载该名单。
  const mine = new Set(targets.map(t => t.node_id));
  const nodeChanges = list.filter(a => a.target_kind === 'node' && mine.has(a.target_id) && changed.has(entryId(a)));
  // 订阅不通过发布下发，但发生变化时需要提示。按用户数统计而非文件数：
  // 一个用户对应 clash 和 uri 两份，显示为「2 份变更」会被理解为涉及两个用户。
  const subs = [
    ...new Set(list.filter(a => a.target_kind === 'user' && changed.has(entryId(a))).map(a => a.target_id)),
  ];

  if (nodeChanges.length === 0) {
    return (
      <div className="callout">
        跟修订 {base} 比，这几台的产物没变
        {subs.length > 0 ? `（另有 ${subs.length} 人的订阅变了）` : ''}。
      </div>
    );
  }

  const byNode = [...new Set(nodeChanges.map(a => a.target_id))];
  return (
    <>
      {byNode.map(node => (
        <NodeDiff
          key={node}
          name={nameOf(node)}
          nodeId={node}
          entries={nodeChanges.filter(a => a.target_id === node)}
          revision={revision}
          base={base}
        />
      ))}
      {subs.length > 0 && (
        <div className="note" style={{ marginTop: 8 }}>
          另有 {subs.length} 人的订阅发生变更：{subs.join('、')}
        </div>
      )}
    </>
  );
}

/* 每台一个折叠块，内容按需拉取：每份产物需要获取两个版本，十台全部展开即产生四十个请求。 */
function NodeDiff({
  name,
  nodeId,
  entries,
  revision,
  base,
}: {
  name: string;
  nodeId: string;
  entries: ArtifactIndexEntry[];
  revision: number;
  base: number;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className="dp-node">
      <button className="dp-nodehead" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span className="tw">{open ? '▾' : '▸'}</span>
        <span className="nm" title={nodeId}>
          {name}
        </span>
        <span className="files">
          {entries.map(e => (
            <span key={e.artifact_kind} className="st">
              {artifactFile(e.artifact_kind)}
            </span>
          ))}
        </span>
        <span className="sp" />
        <span className="note">{entries.length} 份变更</span>
      </button>
      {open && (
        <div className="dp-nodebody">
          {entries.map(e => (
            <FileDiff key={entryId(e)} entry={e} revision={revision} base={base} />
          ))}
        </div>
      )}
    </div>
  );
}

function FileDiff({ entry, revision, base }: { entry: ArtifactIndexEntry; revision: number; base: number }) {
  const key = [entry.target_kind, entry.target_id, entry.artifact_kind] as const;
  const here = useQuery({
    queryKey: ['artifact', revision, ...key],
    queryFn: () => fetchArtifactContent(...key, revision),
  });
  const there = useQuery({
    queryKey: ['artifact', base, ...key],
    queryFn: () => fetchArtifactContent(...key, base),
  });

  const fmt = artifactFmt(entry.artifact_kind);
  const head = (extra?: React.ReactNode) => (
    <div className="cfg-bar">
      <span className="cfg-file">{artifactFile(entry.artifact_kind)}</span>
      <span className="cfg-sp" />
      {extra}
      <span className={`cfg-fmt ${fmt}`}>{fmt}</span>
    </div>
  );

  if (here.isPending || there.isPending)
    return (
      <div className="dp-file">
        {head()}
        <Loading />
      </div>
    );
  if (here.error || there.error)
    return (
      <div className="dp-file">
        {head()}
        <ErrorBox error={here.error ?? there.error} />
      </div>
    );

  const text = here.data.content ?? '';
  const before = there.data.content ?? '';
  const ops = diffLines(before, text);
  const counts = countChanges(ops);
  /* 产物有数百行，全部展开会使改动内容难以定位。 */
  const shown = collapseContext(ops, 3);

  return (
    <div className="dp-file">
      {head(
        <span className="fg-delta" style={{ marginRight: 6 }}>
          <span className="add">+{counts.add}</span> <span className="del">−{counts.del}</span>
        </span>,
      )}
      <div className="fg-code dp-diff">
        <table>
          <tbody>
            {shown.map((row, i) =>
              row === null ? (
                <tr key={`gap-${i}`} className="gap">
                  <td className="ln">⋯</td>
                  <td className="src" />
                </tr>
              ) : (
                <tr key={i} className={row.t === '+' ? 'add' : row.t === '-' ? 'del' : undefined}>
                  <td className="ln">{row.n ?? ''}</td>
                  <td className="src" dangerouslySetInnerHTML={{ __html: highlight(row.s, fmt) || '&nbsp;' }} />
                </tr>
              ),
            )}
          </tbody>
        </table>
      </div>
      {here.data.redacted && (
        <div className="fg-afoot">
          <span className="st st-warn">已打码</span>
          <span className="note">私钥原文仅 system-admin 可见。</span>
        </div>
      )}
    </div>
  );
}

/** 将未改动的长段落折叠为一个省略行，改动行前后各保留 `pad` 行。`null` 表示被折叠的段落。 */
function collapseContext<T extends { t: ' ' | '-' | '+' }>(ops: T[], pad: number): (T | null)[] {
  const keep = new Set<number>();
  ops.forEach((op, i) => {
    if (op.t === ' ') return;
    for (let j = Math.max(0, i - pad); j <= Math.min(ops.length - 1, i + pad); j++) keep.add(j);
  });
  const out: (T | null)[] = [];
  let gap = false;
  ops.forEach((op, i) => {
    if (keep.has(i)) {
      out.push(op);
      gap = false;
    } else if (!gap) {
      out.push(null);
      gap = true;
    }
  });
  return out;
}

const OPEN_STATES = new Set(['planned', 'running', 'dispatched', 'converging']);
// 熔断只停止波次推进，不释放限流锁（active 仍为 TRUE），因此熔断后仍需支持取消——
// 取消是将进行中的 target 置为 canceled 并释放锁的唯一方式
// （brocade-store 允许 planned/running/halted 状态执行取消）。
const CANCELABLE = new Set([...OPEN_STATES, 'halted']);
/* target 尚未进入终态表示该波仍处于打开状态（与服务端 current_wave 的判定一致） */
const LIVE_TARGET = new Set(['pending', 'dispatched', 'converging']);

const actionsOf = (t: DeploymentTargetDetail): string[] => {
  const actions = (t.desired_structure as { actions?: unknown } | null)?.actions;
  return Array.isArray(actions) ? (actions as string[]) : [];
};

// 服务端只接受当前打开的那一波的确认，且只有满足以下条件的波次才接受确认
// （brocade-store::deployment 的 confirm SQL）：具有破坏性，且 wave > 1 或包含停用动作。
// 前端使用同一规则，避免渲染出点击后必然返回 400 的按钮。
const waveNeedsConfirm = (targets: DeploymentTargetDetail[], wave: number) =>
  targets.some(
    t => t.disruptive && (wave > 1 || actionsOf(t).some(a => a === 'disable-xray' || a === 'disable-wire-guard')),
  );

/* 待确认的危险操作。确认框渲染在页面底部，同时只显示一个。 */
type Ask =
  | { kind: 'halt' }
  | { kind: 'cancel' }
  | { kind: 'cancelRollback' }
  | { kind: 'rollback' }
  | { kind: 'wave'; wave: number };

const ISOLATABLE_TARGET = new Set(['pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty']);

function Detail({ id, go }: { id: number; go: (d: Drill) => void }) {
  const nameOf = useNodeNames();
  const { who } = useSession();
  const qc = useQueryClient();
  const [ask, setAsk] = useState<Ask | null>(null);
  // 已成功确认的波次。confirm 只记录确认状态，不关闭波次
  // （deployment.rs::confirm_deployment_wave），target 需要等 agent 拉取后才离开 live 集合，
  // openWave 不会立即前移——不记录该状态时，按钮在请求成功后到状态更新之间仍可点击，
  // 同一个波会被确认两次。
  const [confirmedWave, setConfirmedWave] = useState<number | null>(null);
  const detail = useQuery({
    queryKey: ['deployment', id],
    queryFn: () => fetchDeployment(id),
    /* agent 采用拉取模型并异步回报，前端通过轮询获取进度 */
    refetchInterval: q =>
      OPEN_STATES.has((q.state.data?.status ?? '') as string) || q.state.data?.settlement_status === 'debt'
        ? 3_000
        : false,
  });

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['deployment', id] });
    qc.invalidateQueries({ queryKey: ['deployments'] });
    qc.invalidateQueries({ queryKey: ['deployment-verify'] });
  };
  const confirm = useMutation({
    mutationFn: (wave: number) => confirmWave(id, wave),
    onSuccess: (_res, wave) => {
      setConfirmedWave(wave);
      refresh();
    },
  });
  const halt = useMutation({ mutationFn: () => haltDeployment(id), onSuccess: refresh });
  const cancel = useMutation({
    mutationFn: () => cancelDeployment(id),
    onSuccess: () => {
      refresh();
    },
  });
  const cancelRollback = useMutation({
    mutationFn: () => cancelRollbackDeployment(id),
    onSuccess: res => {
      refresh();
      if (res.rollback_deployment_id) go({ p: 'detail', id: res.rollback_deployment_id });
    },
  });
  const rollback = useMutation({
    mutationFn: () =>
      createRollback({
        target_deployment_id: id,
        idempotency_key: randomKey(),
        note: `restore model snapshot from deployment #${id}`,
      }),
    onSuccess: res => {
      refresh();
      go({ p: 'detail', id: res.deployment_id });
    },
  });
  const retry = useMutation({ mutationFn: (node: string) => retryTarget(id, node), onSuccess: refresh });
  const isolate = useMutation({
    mutationFn: (target: DeploymentTargetDetail) =>
      isolateDeploymentTarget(id, target.node_id, {
        expected_target_status: target.status,
        acknowledge_uncertain: target.status !== 'pending',
      }),
    onSuccess: () => {
      refresh();
      qc.invalidateQueries({ queryKey: ['nodes'] });
    },
  });

  if (detail.isPending) return <Loading />;
  if (detail.error) return <ErrorBox error={detail.error} />;

  const d = detail.data;
  // 只列出实际有操作的机器。十余行 skipped 会使实际执行的行难以定位，
  // 而本次发布涉及哪些机器正是详情页的主要内容。总数在列表页的 changed/total 中。
  const acting = d.targets.filter(t => t.status !== 'skipped');
  const waves = [...new Set(acting.map(t => t.wave))].sort((a, b) => a - b);
  const publisher = can(who.role, 'publish');
  const dirty = d.targets.filter(t => t.status === 'failed-dirty');
  const failed = d.targets.filter(t => t.status.startsWith('failed'));
  const live = d.targets.filter(t => LIVE_TARGET.has(t.status));
  const openWave = live.length ? Math.min(...live.map(t => t.wave)) : null;

  return (
    <>
      <div className="chipline" style={{ marginBottom: 8 }}>
        <Status value={d.status} />
        {d.activation_status === 'activated' && d.settlement_status === 'debt' && (
          <span className="st st-gold">已生效 · {d.debt_targets} 台待补偿</span>
        )}
        {d.activation_status === 'activated' && d.settlement_status === 'converged' && (
          <span className="st st-succeeded">已生效</span>
        )}
        {d.status === 'succeeded' && d.activation_status === 'waiting' && (
          <span className="st st-gold">执行完成 · 待生效</span>
        )}
        {d.settlement_status === 'uncertain' && <span className="st st-warn">补偿状态待确认</span>}
        <span className="mono d2">修订 {d.revision_id}</span>
        {d.active ? <span className="st st-gold">active</span> : null}
        {d.rollback_of_deployment_id ? (
          <span className="st st-warn">rollback_to #{d.rollback_of_deployment_id}</span>
        ) : null}
        {d.sync_of_deployment_id ? <span className="st st-gold">sync_of #{d.sync_of_deployment_id}</span> : null}
        <span className="dim">
          actor {d.actor ?? '—'} · <Ago at={d.created_at} />
        </span>
      </div>

      {d.status === 'halted' && (
        <div className="callout err">
          <b>熔断。</b>
          {dirty.length > 0
            ? `${dirty.map(t => nameOf(t.node_id)).join('、')} 进了 failed-dirty——agent 死在半路，这台现在什么状态没人知道。`
            : failed.length > 0
              ? `${failed.map(t => nameOf(t.node_id)).join('、')} 收敛失败，后面的波次已停住。`
              : '人手停的：后面的波次不再下发，已成功的保持成功。'}
          {failed.length > 0 ? '修好后逐台 retry；' : ''}
          想换个修订重来就点「取消」。
        </div>
      )}

      {waves.map(w => {
        const inWave = acting.filter(t => t.wave === w);
        const disruptive = inWave.some(t => t.disruptive);
        const isOpen = openWave === w;
        const canConfirm = isOpen && d.status !== 'halted' && waveNeedsConfirm(inWave, w);
        const queued = openWave !== null && w > openWave;
        return (
          <div key={w}>
            <div className="wavehead">
              <span>
                wave {w} · {disruptive ? '破坏性 · 金丝雀，一台一波' : '不掉线，一波推完'}
              </span>
              <span className="rule" />
              {canConfirm && (
                <button
                  className="btn primary"
                  disabled={!publisher || confirm.isPending || confirmedWave === w}
                  onClick={() => setAsk({ kind: 'wave', wave: w })}
                >
                  {confirmedWave === w ? '已确认下发' : `确认第 ${w} 波`}
                </button>
              )}
              {isOpen && !canConfirm && d.status !== 'halted' && <span className="note">进行中 · 等待 agent 拉取</span>}
              {queued && <span className="note">等待前一波完成</span>}
            </div>
            <table className="tbl dp-wave">
              <tbody>
                {inWave.map(t => (
                  <TargetRow
                    key={t.node_id}
                    t={t}
                    publisher={publisher}
                    system={can(who.role, 'system') && !isolate.isPending}
                    retryPending={retry.isPending}
                    retrying={retry.isPending && retry.variables === t.node_id}
                    onRetry={() => retry.mutate(t.node_id)}
                    onIsolate={() => isolate.mutate(t)}
                  />
                ))}
              </tbody>
            </table>
          </div>
        );
      })}

      <div className="wavehead" style={{ marginTop: 14 }}>
        <span>产物变更 · 跟修订 {d.base_revision_id ?? '—'} 比</span>
        <span className="rule" />
      </div>
      <ArtifactChanges revision={d.revision_id} base={d.base_revision_id} targets={acting} />

      {(confirm.error ||
        halt.error ||
        cancel.error ||
        cancelRollback.error ||
        rollback.error ||
        retry.error ||
        isolate.error) && (
        <ErrorBox
          error={
            confirm.error ??
            halt.error ??
            cancel.error ??
            cancelRollback.error ??
            rollback.error ??
            retry.error ??
            isolate.error
          }
        />
      )}

      <div className="toolbar" style={{ marginTop: 12 }}>
        {OPEN_STATES.has(d.status) && (
          <button
            className="btn danger"
            disabled={!publisher || halt.isPending}
            title="停住后面的波次，已成功的保持成功"
            onClick={() => setAsk({ kind: 'halt' })}
          >
            熔断
          </button>
        )}
        {CANCELABLE.has(d.status) && (
          <button
            className="btn danger"
            disabled={!publisher || cancel.isPending}
            title="只停止这次发布：在途 target 转 canceled，之后能开新的发布"
            onClick={() => setAsk({ kind: 'cancel' })}
          >
            取消
          </button>
        )}
        {CANCELABLE.has(d.status) && (
          <button
            className="btn danger"
            disabled={!can(who.role, 'system') || cancelRollback.isPending}
            title="停止当前发布，并强制覆盖成它之前最近一次成功发布的快照"
            onClick={() => setAsk({ kind: 'cancelRollback' })}
          >
            取消并回滚
          </button>
        )}
        {d.status === 'succeeded' && (
          <button
            className="btn danger"
            disabled={!can(who.role, 'system') || rollback.isPending}
            title="恢复这次发布对应的模型快照，并创建强制同步工单"
            onClick={() => setAsk({ kind: 'rollback' })}
          >
            恢复到这次快照
          </button>
        )}
        <span className="sp" />
        <span className="note">{OPEN_STATES.has(d.status) ? '每 3 秒刷新' : '已完成'}</span>
      </div>

      {/* 危险操作确认。波次确认附带该波的中断影响摘要；回滚类操作需要输入「回滚」才能执行。 */}
      {ask?.kind === 'halt' && (
        <Confirm
          title="熔断这次发布？"
          body={<>停住后面的波次，已成功的保持成功。要彻底收尾再点「取消」。</>}
          confirmLabel="熔断"
          onConfirm={() => {
            setAsk(null);
            halt.mutate();
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'cancel' && (
        <Confirm
          title="取消这次发布？"
          body={
            <>
              只停止这次发布：在途 target 转 canceled。已经 dispatched / converging 但没回报的， 现场会被标成
              dirty——那台机器现在什么状态没人知道。
            </>
          }
          confirmLabel="取消发布"
          onConfirm={() => {
            setAsk(null);
            cancel.mutate();
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'cancelRollback' && (
        <Confirm
          title="取消并回滚？"
          body={
            <>
              system-admin 操作。取消当前发布，并强制覆盖成它之前最近一次成功发布的快照，
              创建强制同步工单——机器会按旧快照收敛。
            </>
          }
          confirmLabel="取消并回滚"
          requireWord="回滚"
          onConfirm={() => {
            setAsk(null);
            cancelRollback.mutate();
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'rollback' && (
        <Confirm
          title="恢复到这次快照？"
          body={<>system-admin 操作。恢复这次发布对应的模型快照，并创建强制同步工单—— 机器会回到发布前的样子。</>}
          confirmLabel="恢复快照"
          requireWord="回滚"
          onConfirm={() => {
            setAsk(null);
            rollback.mutate();
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'wave' && (
        <Confirm
          title={`确认第 ${ask.wave} 波？`}
          body={<WaveSummary targets={d.targets.filter(t => t.wave === ask.wave)} nameOf={nameOf} />}
          confirmLabel="确认并下发"
          onConfirm={() => {
            setAsk(null);
            confirm.mutate(ask.wave);
          }}
          onCancel={() => setAsk(null)}
        />
      )}
    </>
  );
}

// 确认波次前先展示该波的中断影响。数据全部在 Detail 中，只是平时需要自行读完表格——
// 确认弹窗中集中呈现。
/* 该波需要确认的是影响范围和是否中断，而非逐台核对。
   因此按动作聚合而非逐台列出——一波九十台时该表有九十行，
   逐行读完仍无法得出中断的机器数量，需要自行统计。
   聚合后可直接得出该结果。
   逐台明细在页面的波次表中已有，关闭该框即可查看。 */
function WaveSummary({ targets, nameOf }: { targets: DeploymentTargetDetail[]; nameOf: (id: string) => string }) {
  const actionCount = targets.reduce((n, t) => n + actionsOf(t).length, 0);
  const disruptive = targets.filter(t => t.disruptive);

  // 动作到机器数的映射。按机器数排序，影响最大的排在前面（重写 xray 的机器数通常最多）。
  const byAction = new Map<string, number>();
  for (const t of targets) for (const a of actionsOf(t)) byAction.set(a, (byAction.get(a) ?? 0) + 1);
  const actions = [...byAction.entries()].sort((a, b) => b[1] - a[1]);

  // 只显示部分机器名。显示若干个用于确认是否为预期的机器范围，而非用于逐一核对。
  const SHOW = 6;
  const shown = targets.slice(0, SHOW).map(t => nameOf(t.node_id));
  const rest = targets.length - shown.length;

  return (
    <>
      <p>
        这一波 <b>{targets.length}</b> 台机器、{actionCount} 个动作。
        {disruptive.length > 0 ? (
          <>
            其中 <b>{disruptive.length}</b> 台是破坏性的 —— 会断连接，确认后立刻执行。
          </>
        ) : (
          '不掉线，一波推完。'
        )}
      </p>
      {/* 机器数并入动作所在的格，不单独占一列：窄屏下 .tbl 的每一格会变为独立一行，
          单独的「90」占一行时无法与对应的动作关联。 */}
      <table className="tbl dp-wave">
        <tbody>
          {actions.map(([a, n]) => (
            <tr key={a}>
              <td>
                <span className="st" style={{ fontSize: '10.5px' }}>
                  {a}
                </span>{' '}
                <b className="mono">×{n}</b>
              </td>
              <td className="dim" style={{ fontSize: 12 }}>
                {(ACTION_NOTE as Record<string, string>)[a]}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="dim" style={{ fontSize: 12 }}>
        {shown.join('、')}
        {rest > 0 && ` 等 ${targets.length} 台`}
        {rest > 0 && <>（逐台明细在下面的波次表里）</>}
      </p>
    </>
  );
}

function TargetRow({
  t,
  publisher,
  system,
  retryPending,
  retrying,
  onRetry,
  onIsolate,
}: {
  t: DeploymentTargetDetail;
  publisher: boolean;
  system: boolean;
  retryPending: boolean;
  retrying: boolean;
  onRetry: () => void;
  onIsolate: () => void;
}) {
  const nameOf = useNodeNames();
  const guide = t.error ? guideFor(t.error) : undefined;
  return (
    <>
      <tr>
        <td title={t.node_id}>{nameOf(t.node_id)}</td>
        <td>
          <Status value={t.status} />
        </td>
        <td className="dim" style={{ fontSize: 12 }}>
          {t.error ? (
            <span style={{ color: 'var(--err)' }}>{t.error}</span>
          ) : t.dispatched_at ? (
            <>
              下发于 <Ago at={t.dispatched_at} />
            </>
          ) : (
            ''
          )}
        </td>
        <td>
          {t.status.startsWith('failed') && (
            <button className="btn" disabled={!publisher || retryPending} onClick={onRetry}>
              {retrying ? 'retrying…' : 'retry'}
            </button>
          )}
          {ISOLATABLE_TARGET.has(t.status) && (
            <button className="btn danger" disabled={!system} onClick={onIsolate}>
              隔离
            </button>
          )}
        </td>
      </tr>
      {t.error && guide && (
        <tr>
          <td colSpan={4} className="note" style={{ paddingTop: 2, paddingBottom: 6 }}>
            <span className="dim">处理方式：</span>
            {guide}
            {t.status === 'failed-dirty' ? ' 这台机器当前状态未知，请先登录确认。' : ''}
          </td>
        </tr>
      )}
    </>
  );
}
