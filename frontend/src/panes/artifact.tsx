import { useQuery } from '@tanstack/react-query';
import { fetchArtifactContent, fetchArtifactIndex, fetchRevisions, type ArtifactIndexEntry } from '../api';
import { useSyncExternalStore } from 'react';
import { Empty, ErrorBox, Loading } from '../ui/bits';
import { artifactPanel } from '../ui/artifact-panel';
import { copyText } from '../ui/platform';
import { can, useSession } from '../session';

const FILE: Record<string, { file: string; lang: string }> = {
  phantun: { file: 'phantun.json', lang: 'json' },
  wireguard: { file: 'wg0.conf', lang: 'ini' },
  xray: { file: 'xray.json', lang: 'json' },
  hy2_port_hop: { file: 'hy2_port_hop.json', lang: 'json' },
  grants: { file: 'grants.json-rpc', lang: 'json-rpc' },
  uri: { file: 'uri.txt', lang: 'txt' },
  clash: { file: 'clash.yaml', lang: 'yaml' },
};
const fileOf = (kind: string) => FILE[kind] ?? { file: kind, lang: 'txt' };
const fmtSize = (n: number | null) => (n === null ? '—' : n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KB`);

// 产物面板：左侧目录树、右侧代码，整体沿用 playground 配置输出的形态。
// 它是工作台右侧的独立面板，既不是窗口也不属于画布——在两个台面上都存在，
// 显示的始终是当前网络结构的全部产物。
export function ArtifactPanel() {
  const { who } = useSession();
  const state = useSyncExternalStore(artifactPanel.subscribe, artifactPanel.snapshot);
  // 产物随当前修订变化：查询键包含修订号，模型变更后（角标处在轮询修订）
  // 此处自动获取新数据，写入方无需逐个调用 invalidate。
  const revisions = useQuery({
    queryKey: ['revisions'],
    queryFn: () => fetchRevisions(),
    refetchInterval: 10_000,
  });
  const revision = revisions.data?.current_revision;
  const index = useQuery({
    queryKey: ['artifacts', revision],
    queryFn: () => fetchArtifactIndex(),
    enabled: !!revision,
  });

  // 面板的展开状态会被存储，切换用户后仍然保留，因此该判断需要在此处而非仅在按钮上：
  // 前一用户在展开产物栏的状态下退出，下一个评审角色进入时会直接看到 403。
  if (!can(who.role, 'artifacts')) return null;
  if (!state.open) return null;

  const pick = (e: ArtifactIndexEntry) =>
    artifactPanel.select({
      targetKind: e.target_kind,
      targetId: e.target_id,
      artifactKind: e.artifact_kind,
    });

  const shell = (body: React.ReactNode) => (
    <aside id="artpanel" aria-label="产物">
      <div className="artpanel-head">
        <span className="fw-kind">产物</span>
        <span className="artpanel-title">当前网络结构的产物</span>
        <button title="收起面板" onClick={() => artifactPanel.close()}>
          ✕
        </button>
      </div>
      {body}
    </aside>
  );

  if (revisions.isPending || (!!revision && index.isPending)) return shell(<Loading />);
  if (revisions.error) return shell(<ErrorBox error={revisions.error} />);
  if (revision == null) return shell(<Empty>还没有可查看的修订。</Empty>);
  if (index.error) return shell(<ErrorBox error={index.error} />);
  if (!index.data) return shell(<ErrorBox error={new Error('产物索引没有返回结果')} />);

  const all = index.data.artifacts;
  if (all.length === 0) return shell(<Empty>当前修订还没有产物。先纳管一台机器。</Empty>);
  const sel = state.sel;

  const current =
    all.find(
      e =>
        sel && e.target_kind === sel.targetKind && e.target_id === sel.targetId && e.artifact_kind === sel.artifactKind,
    ) ?? all[0];

  // 空的分组同样保留并说明原因。直接过滤会使人认为控制台不具备该功能——
  // 订阅需要同时满足存在用户、存在授权、模型可发布三个条件，
  // 服务端的 artifact_index 才会生成它们。
  const groups = [
    {
      grp: '节点产物',
      items: all.filter(e => e.target_kind === 'node'),
      empty: '还没有节点产物。先纳管机器。',
    },
    {
      grp: '订阅',
      items: all.filter(e => e.target_kind === 'user'),
      empty: '还没有人拿到授权。去「用户与授权」勾上接入面。',
    },
  ];

  return shell(
    <div className="artwrap">
      <div className="cfg-tree">
        <div className="cfg-tree-hd">
          <span className="t">产物</span>
          <span className="n">{all.length}</span>
        </div>
        <div className="cfg-tree-scroll">
          {groups.map(g => {
            const owners = [...new Set(g.items.map(e => e.target_id))];
            return (
              <div key={g.grp}>
                <p className="cfg-grp">{g.grp}</p>
                {g.items.length === 0 && (
                  <p className="note" style={{ padding: '0 10px 8px' }}>
                    {g.empty}
                  </p>
                )}
                {owners.map(owner => (
                  <div key={owner}>
                    <div className="cfg-owner">
                      <span className="who">{owner}</span>
                    </div>
                    {g.items
                      .filter(e => e.target_id === owner)
                      .map(e => (
                        <button
                          key={`${e.target_id}/${e.artifact_kind}`}
                          className={`cfg-leaf${e.state === 'disabled' ? ' off' : ''}`}
                          aria-current={e === current}
                          title={`${e.state} · ${fmtSize(e.byte_len)}`}
                          onClick={() => pick(e)}
                        >
                          <span className="nm">{fileOf(e.artifact_kind).file}</span>
                          <span className="n">{e.state === 'present' ? fmtSize(e.byte_len) : e.state}</span>
                        </button>
                      ))}
                  </div>
                ))}
              </div>
            );
          })}
        </div>
      </div>
      <ArtifactBody entry={current} revision={revision} />
    </div>,
  );
}

function ArtifactBody({ entry, revision }: { entry: ArtifactIndexEntry; revision: number }) {
  const meta = fileOf(entry.artifact_kind);
  const content = useQuery({
    queryKey: ['artifact', revision, entry.target_kind, entry.target_id, entry.artifact_kind],
    queryFn: () => fetchArtifactContent(entry.target_kind, entry.target_id, entry.artifact_kind),
    enabled: entry.state === 'present',
  });

  const text = content.data?.content ?? '';
  const lines = text ? text.split('\n') : [];

  return (
    <div className="cfg-main">
      <div className="cfg-bar">
        <span className="cfg-path">
          {entry.target_kind === 'node' ? '节点产物' : '订阅'} / {entry.target_id} /&nbsp;
        </span>
        <span className="cfg-file">{meta.file}</span>
        <span className={`cfg-fmt ${meta.lang}`}>{meta.lang}</span>
        <span className="cfg-sp" />
        {text && (
          <button className="btn" onClick={() => void copyText(text)}>
            复制
          </button>
        )}
      </div>
      {entry.state !== 'present' ? (
        <div className="cfg-code">
          <pre className="cd">
            这份产物当前是 {entry.state}
            ——不是没算出来，是编译结果就说它该停用/不托管。
          </pre>
        </div>
      ) : content.isPending ? (
        <Loading />
      ) : content.error ? (
        <ErrorBox error={content.error} />
      ) : (
        <>
          <div className="cfg-code">
            <div className="lnum">{lines.map((_, i) => i + 1).join('\n')}</div>
            <pre className="cd">{text}</pre>
          </div>
          <div className="cfg-foot">
            <b>{lines.length} 行</b>
            <span>·</span>
            <span>{fmtSize(entry.byte_len)}</span>
            <span className="cfg-sp" />
            {content.data.redacted && <span className="st st-warn">私钥已由服务端打码</span>}
            <span className="mono dim">sha {entry.sha256?.slice(0, 12) ?? '—'}…</span>
          </div>
        </>
      )}
    </div>
  );
}
