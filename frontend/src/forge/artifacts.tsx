// 右列：编译产生的产物。高度贯通到顶，与浏览器顶边对齐。
//
// 与演示版本的主要差异：产物由服务端生成，浏览器不执行编译。
// 版本间的差异也不在本地重新计算——`GET /artifacts/index?revision=` 的每条记录
// 都包含 sha256，比较两个版本的索引即可确定哪些产物发生变化，无需拉取内容。
// 内容只在展开某一份时获取，且获取两份（当前版本和上一版本）用于行级 diff。
//
// 结构是一棵 `.cfg-tree`：组标题 → owner 行（带角色标签）→ 缩进的叶节点，
// 全部展开不折叠。产物总数在十余份，折叠会为查看变更增加两次点击。
// 原有的横向 tab 已移除：19 份产物对应 19 个 tab，查找某一份需要横向滚动。

import { useEffect, useMemo, useRef, useSyncExternalStore } from 'react';
import { draft } from '../draft';
import { useQuery } from '@tanstack/react-query';
import {
  fetchArtifactContent,
  fetchArtifactContentView,
  fetchArtifactIndex,
  fetchArtifactIndexView,
  type ArtifactIndexEntry,
  type SnapshotApp,
} from '../api';
import { artifactPanel } from '../ui/artifact-panel';
import { Empty, ErrorBox, Loading } from '../ui/bits';
import { artifactFile, artifactFmt, countChanges, diffLines, fmtBytes, highlight } from './diff';

export const entryId = (a: ArtifactIndexEntry) => `${a.target_kind}/${a.target_id}/${a.artifact_kind}`;

/** 通过比较 sha256 得出发生变化的产物 id——索引本身包含 sha256，无需拉取内容。 */
function changedBetween(here: ArtifactIndexEntry[], there: ArtifactIndexEntry[]): Set<string> {
  const before = new Map(there.map(a => [entryId(a), a.sha256]));
  const changed = new Set<string>();
  for (const a of here) {
    const was = before.get(entryId(a));
    if (was === undefined || was !== a.sha256) changed.add(entryId(a));
  }
  return changed;
}

/** 两个已提交修订之间的产物差异。
 *
 * 与 `useChangedArtifacts` 的差异在于不包含草稿。包含草稿时，
 * 在编辑过程中查看发布历史，历史内容会随当前草稿变化。 */
export function useRevisionDiff(revision: number | undefined, base: number | null | undefined) {
  const here = useQuery({
    queryKey: ['artifact-index', revision],
    queryFn: () => fetchArtifactIndex(revision),
    enabled: revision != null,
  });
  const there = useQuery({
    queryKey: ['artifact-index', base],
    queryFn: () => fetchArtifactIndex(base ?? undefined),
    enabled: base != null,
  });

  return useMemo(() => {
    const list = here.data?.artifacts ?? [];
    /* 没有基线时不判定为已变更：将全部产物标为变更不提供信息。 */
    if (base == null || !there.data) return { list, changed: new Set<string>(), known: false };
    return { list, changed: changedBetween(list, there.data.artifacts), known: true };
  }, [here.data, there.data, base]);
}

/** 当前版本相对基线发生变化的产物，返回产物 id 的集合。
 *
 * 基线的选取取决于是否存在草稿。存在草稿时基线是当前已提交的修订——此时需要了解的是
 * 未提交的改动会将产物修改为什么内容。不存在草稿时默认不比较；
 * 只有调用方确认当前修订尚未收敛时，才以上一修订为基线用于发布前查看。 */
export function useChangedArtifacts(
  revision: number | undefined,
  prev: number | undefined,
  options: { compareClean?: boolean } = {},
) {
  const draftVer = useSyncExternalStore(draft.subscribe, draft.version);
  const dirty = !draft.isEmpty();
  const base = dirty ? revision : options.compareClean ? prev : undefined;

  const here = useQuery({
    queryKey: ['artifact-index', 'view', revision, draftVer],
    queryFn: () => fetchArtifactIndexView(revision),
    enabled: revision != null,
  });
  const there = useQuery({
    queryKey: ['artifact-index', base],
    queryFn: () => fetchArtifactIndex(base),
    enabled: base != null,
  });

  return useMemo(() => {
    const list = here.data?.artifacts ?? [];
    // 没有基线（首次编译）时不判定为已变更——那会将全部产物标为变更，不提供信息。
    if (base == null || !there.data) return { list, changed: new Set<string>(), known: false, dirty };
    return { list, changed: changedBetween(list, there.data.artifacts), known: true, dirty };
  }, [here.data, there.data, base, dirty]);
}

/** 影响范围：发生变化的 node 类产物涉及的机器数量。 */
export function blastRadius(list: ArtifactIndexEntry[], changed: Set<string>): Set<string> {
  const nodes = new Set<string>();
  for (const a of list) {
    if (a.target_kind !== 'node') continue;
    if (changed.has(entryId(a))) nodes.add(a.target_id);
  }
  return nodes;
}

/** 该机器在所有项目中的角色。读取快照的 ingresses 和 steps，不另行计算。 */
function nodeRoles(apps: SnapshotApp[], nodeId: string): string[] {
  const r = new Set<string>();
  for (const a of apps) {
    if (a.ingresses.some(i => i.node === nodeId)) r.add('入口');
    for (const s of a.steps) {
      for (const rule of s.rules) {
        if (s.node === nodeId && rule.a.t === 'egress') r.add('落地');
        if (s.node === nodeId && rule.a.t === 'proxy') r.add('外部代理');
        if (rule.a.t === 'forward' && (s.node === nodeId || rule.a.to === nodeId)) r.add('中转');
      }
    }
  }
  return ['入口', '中转', '外部代理', '落地'].filter(x => r.has(x));
}

const GROUPS: { kind: string; label: string }[] = [
  { kind: 'node', label: '机器产物' },
  { kind: 'user', label: '订阅' },
];

export function ArtifactRail({
  revision,
  prev,
  compareClean,
  apps,
  onClose,
}: {
  revision: number | undefined;
  prev: number | undefined;
  compareClean: boolean;
  apps: SnapshotApp[];
  onClose: () => void;
}) {
  const panel = useSyncExternalStore(artifactPanel.subscribe, artifactPanel.snapshot);
  const { list, changed, dirty } = useChangedArtifacts(revision, prev, { compareClean });
  const baseRevision = dirty ? revision : compareClean ? prev : undefined;
  const rail = useRef<HTMLElement>(null);

  const cur =
    list.find(
      a =>
        panel.sel &&
        a.target_kind === panel.sel.targetKind &&
        a.target_id === panel.sel.targetId &&
        a.artifact_kind === panel.sel.artifactKind,
    ) ??
    list.find(a => changed.has(entryId(a))) ??
    list[0];

  // 将选中的叶节点滚动到可见区域：从其他位置跳转进入（如机器详情）时它可能位于树的下方。
  // 依赖使用 id 而非对象：列表每次重新获取都是新对象，以对象为依赖会导致每次刷新都滚动。
  const curId = cur ? entryId(cur) : null;
  useEffect(() => {
    rail.current?.querySelector('.cfg-leaf[aria-current="true"]')?.scrollIntoView({ block: 'nearest' });
  }, [curId]);

  /* 拖动调整右列宽度。释放时才写回样式变量，拖动过程中只修改 --rail，不触发 React 重渲染。 */
  const onDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);
    handle.classList.add('on');
    document.body.classList.add('fg-dragging');
    const move = (ev: PointerEvent) => {
      const w = Math.min(Math.max(window.innerWidth - ev.clientX, 360), window.innerWidth * 0.72);
      document.documentElement.style.setProperty('--rail', `${Math.round(w)}px`);
    };
    const up = () => {
      handle.classList.remove('on');
      document.body.classList.remove('fg-dragging');
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', up);
      handle.removeEventListener('pointercancel', up);
    };
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', up);
    handle.addEventListener('pointercancel', up);
  };

  return (
    <aside className="fg-rail" ref={rail}>
      <div className="fg-hs" title="拖动改宽度" onPointerDown={onDrag} />
      <div className="fg-rsh">
        产物 · {dirty ? '草稿预览' : '当前修订'}
        <span className="n">
          {list.length} 份{baseRevision != null && changed.size ? ` · 比修订 ${baseRevision} ${changed.size} 变` : ''}
        </span>
        <button className="x" title="关闭产物栏" aria-label="关闭产物栏" onClick={onClose}>
          ×
        </button>
      </div>

      <div className="fg-artbody">
        <div className="fg-arttree">
          {GROUPS.map(g => {
            const mine = list.filter(a => a.target_kind === g.kind);
            if (!mine.length) return null;
            const targets = [...new Set(mine.map(a => a.target_id))];
            return (
              <div key={g.kind}>
                <p className="cfg-grp">{g.label}</p>
                {targets.map(t => {
                  const roles = g.kind === 'node' ? nodeRoles(apps, t) : [];
                  return (
                    <div key={t}>
                      <div className="cfg-owner">
                        <span className="who">{t}</span>
                        {roles.length > 0 && (
                          <span
                            className={`role${roles[0] === '入口' ? ' r-in' : roles.includes('落地') ? ' r-eg' : ''}`}
                          >
                            {roles.join('·')}
                          </span>
                        )}
                      </div>
                      {mine
                        .filter(a => a.target_id === t)
                        .map(a => {
                          const id = entryId(a);
                          return (
                            <button
                              key={id}
                              className="cfg-leaf"
                              aria-current={cur && entryId(cur) === id ? 'true' : 'false'}
                              title={`${artifactFile(a.artifact_kind)} · ${fmtBytes(a.byte_len)}`}
                              onClick={() =>
                                artifactPanel.select({
                                  targetKind: a.target_kind,
                                  targetId: a.target_id,
                                  artifactKind: a.artifact_kind,
                                })
                              }
                            >
                              <span className="nm">{artifactFile(a.artifact_kind)}</span>
                              {changed.has(id) && (
                                <span
                                  className="chg"
                                  title={dirty ? '草稿会改动这份产物' : `本修订相对修订 ${baseRevision} 有变化`}
                                />
                              )}
                              <span className="n">{fmtBytes(a.byte_len)}</span>
                            </button>
                          );
                        })}
                    </div>
                  );
                })}
              </div>
            );
          })}
          {/* 为空有两种原因，该栏无法区分：模型中确实没有可编译的内容，
              或编译因错误而中止（产物只在 can_publish 为真时产生）。
              后一种更常见，此前的「该版本没有产物」会引导向前一种理解，
              而实际需要查看的是诊断。因此两种原因都说明。 */}
          {list.length === 0 && <Empty>当前无产物或编译检测出无法解决的问题。</Empty>}
        </div>

        <div className="fg-artpane">
          {cur ? (
            <ArtifactBody
              entry={cur}
              revision={revision}
              baseRevision={changed.has(entryId(cur)) ? baseRevision : undefined}
            />
          ) : (
            <div className="fg-code" />
          )}
        </div>
      </div>
    </aside>
  );
}

function ArtifactBody({
  entry,
  revision,
  baseRevision,
}: {
  entry: ArtifactIndexEntry;
  revision: number | undefined;
  baseRevision: number | undefined;
}) {
  const draftVer = useSyncExternalStore(draft.subscribe, draft.version);

  const here = useQuery({
    queryKey: ['artifact', 'view', revision, draftVer, entry.target_kind, entry.target_id, entry.artifact_kind],
    queryFn: () => fetchArtifactContentView(entry.target_kind, entry.target_id, entry.artifact_kind, revision),
  });
  const there = useQuery({
    queryKey: ['artifact', baseRevision, entry.target_kind, entry.target_id, entry.artifact_kind],
    queryFn: () => fetchArtifactContent(entry.target_kind, entry.target_id, entry.artifact_kind, baseRevision),
    enabled: baseRevision != null,
  });

  const fmt = artifactFmt(entry.artifact_kind);
  const bar = (
    <div className="cfg-bar">
      <span className="cfg-path">
        {entry.target_kind === 'node' ? '机器产物' : '订阅'} / {entry.target_id} /
      </span>
      <span className="cfg-file">{artifactFile(entry.artifact_kind)}</span>
      <span className="cfg-sp" />
      <span className={`cfg-fmt ${fmt}`}>{fmt}</span>
    </div>
  );

  if (here.isPending)
    return (
      <>
        {bar}
        <div className="fg-code">
          <Loading />
        </div>
      </>
    );
  if (here.error)
    return (
      <>
        {bar}
        <div className="fg-code">
          <ErrorBox error={here.error} />
        </div>
      </>
    );

  const text = here.data.content ?? '';
  const before = baseRevision != null ? (there.data?.content ?? null) : null;
  const ops = before != null && before !== text ? diffLines(before, text) : null;
  const counts = ops ? countChanges(ops) : null;

  return (
    <>
      {bar}
      <div className="fg-code">
        <table>
          <tbody>
            {ops
              ? ops.map((op, i) => (
                  <tr key={i} className={op.t === '+' ? 'add' : op.t === '-' ? 'del' : undefined}>
                    <td className="ln">{op.n ?? ''}</td>
                    <td className="src" dangerouslySetInnerHTML={{ __html: highlight(op.s, fmt) || '&nbsp;' }} />
                  </tr>
                ))
              : text.split('\n').map((line, i) => (
                  <tr key={i}>
                    <td className="ln">{i + 1}</td>
                    <td className="src" dangerouslySetInnerHTML={{ __html: highlight(line, fmt) || '&nbsp;' }} />
                  </tr>
                ))}
          </tbody>
        </table>
      </div>
      <div className="fg-afoot">
        {here.data.redacted && <span className="st st-warn">已打码</span>}
        <span className="sp" />
        {text ? `${text.split('\n').length} 行 · ` : ''}
        {fmtBytes(entry.byte_len)}
        {counts ? (
          <span className="fg-delta">
            {' · '}
            <span className="add">+{counts.add}</span> <span className="del">−{counts.del}</span> 跟修订 {baseRevision}{' '}
            比
          </span>
        ) : baseRevision != null ? (
          ` · 与修订 ${baseRevision} 相同`
        ) : null}
      </div>
    </>
  );
}
