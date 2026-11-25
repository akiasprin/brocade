import { useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { getCvmArtifacts, type CvmArtifactKind, type CvmInstance } from '../../api';
import { diffLines } from '../../forge/diff';
import { ErrorBox, SegmentedControl } from '../../ui/bits';
import { CopyButton } from '../../ui/copy-button';
import { PanelTitle } from '../../ui/icons';

const kinds: { kind: CvmArtifactKind; name: string; effect: string; bindings: string }[] = [
  { kind: 'cube/sandbox.json', name: '沙箱', effect: '关机改配后创建', bindings: '原盘对象 · Shim 版本' },
  { kind: 'cube/egress.json', name: '出网策略', effect: '运行中热更新', bindings: '无' },
  { kind: 'cube/attachments.json', name: '网卡附件', effect: '运行中热收敛', bindings: '运行时 ID' },
  { kind: 'guest/init.json', name: 'Guest 初始化', effect: '启动身份 · 创建后执行', bindings: '无' },
];
export function CvmArtifacts({ instance }: { instance: CvmInstance }) {
  const [kind, setKind] = useState<CvmArtifactKind>('cube/sandbox.json');
  const [view, setView] = useState<'diff' | 'full'>('diff');
  const generation = instance.desired.generation;
  const query = useQuery({
    queryKey: ['cvm-artifacts', instance.desired.id, generation],
    queryFn: () => getCvmArtifacts(instance.desired.id, generation),
    staleTime: Infinity,
  });
  const currentFiles = query.data?.current?.files;
  const previousFiles = query.data?.previous?.files;
  const diff = useMemo(() => {
    const current = currentFiles?.find(file => file.kind === kind);
    const previous = previousFiles?.find(file => file.kind === kind);
    return current ? diffLines(previous?.content ?? '', current.content) : [];
  }, [currentFiles, previousFiles, kind]);
  if (query.isPending) return <p role="status">正在读取冻结产物…</p>;
  if (query.error)
    return (
      <div>
        <ErrorBox error={query.error} />
        <button className="btn" onClick={() => void query.refetch()}>
          重试读取
        </button>
      </div>
    );
  const { current, previous } = query.data;
  if (!current)
    return (
      <section className="panel titled cvm-net-panel" aria-label="产物">
        <header>
          <PanelTitle of="artifacts">产物</PanelTitle>
        </header>
        <p className="note cvm-net-empty">
          这一代没有编译产物。裸机实例不使用 Cube 产物；容器运行操作需要完整产物，缺失时不会执行。
        </p>
      </section>
    );
  const file = current.files.find(f => f.kind === kind);
  const report = instance.report;
  const receipt =
    report?.generation === generation &&
    report.operation_id === instance.desired.operation_id &&
    report.artifacts?.input_sha256 === current.input_sha256
      ? report.artifacts
      : undefined;
  const applied = receipt?.files.find(f => f.kind === kind);
  const selected = kinds.find(item => item.kind === kind)!;
  const status = receipt
    ? receipt.mismatches.length
      ? {
          tone: 'st-halted',
          text: '校验不一致',
          detail: `产物校验不一致：${receipt.mismatches.join('、')}。本轮已拒绝执行。`,
        }
      : report?.status === 'applied'
        ? { tone: 'st-succeeded', text: '已执行', detail: '产物校验通过，已按这一代产物执行' }
        : { tone: 'st-pending', text: '校验通过', detail: '产物校验通过，尚未确认执行完成' }
    : { tone: 'st-pending', text: '尚无回执', detail: '尚无这一代的产物校验回执' };
  return (
    <div className="cvm-artifacts">
      <section className="panel titled cvm-net-panel" aria-label="冻结产物">
        <header>
          <PanelTitle of="artifacts">冻结产物</PanelTitle>
          <span className="hint">
            第 {generation} 代 · {current.compiler}
          </span>
          <span className="cvm-panel-tools">
            <span className={`st ${status.tone}`} aria-hidden="true">
              {status.text}
            </span>
          </span>
        </header>
        <div className="cvm-artifact-body">
          <p className="cvm-artifact-status" data-tone={status.tone} role="status">
            {status.detail}
          </p>
          <p className="note">规格、镜像清单和能力快照共同固定输入。代号记出处；文件摘要不随代号变化。</p>
          <dl className="cvm-artifact-meta">
            <div>
              <dt>输入 SHA-256</dt>
              <dd>
                <code>{current.input_sha256}</code>
              </dd>
            </div>
            <div>
              <dt>规格 SHA-256</dt>
              <dd>
                <code>{current.spec_sha256}</code>
              </dd>
            </div>
          </dl>
          <div className="cvm-artifact-files" aria-label="产物文件">
            {kinds.map(item => {
              const f = current.files.find(f => f.kind === item.kind);
              const old = previous?.files.find(f => f.kind === item.kind);
              return (
                <button
                  type="button"
                  key={item.kind}
                  className={`cvm-artifact-file${kind === item.kind ? ' active' : ''}`}
                  aria-pressed={kind === item.kind}
                  onClick={() => setKind(item.kind)}
                >
                  <strong>{item.name}</strong>
                  <code>{item.kind}</code>
                  <span>
                    {!previous ? '首次生成' : f?.sha256 === old?.sha256 ? '内容不变' : '内容变化'} · {item.effect}
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      </section>
      {file && (
        <section className="panel titled cvm-net-panel" aria-label={file.kind}>
          <header>
            <h4 className="panel-title cvm-artifact-name">{file.kind}</h4>
            <span className="hint">{selected.effect}</span>
            <span className="cvm-panel-tools">
              <SegmentedControl
                ariaLabel="产物视图"
                value={view}
                options={[
                  { value: 'diff', label: '差异' },
                  { value: 'full', label: '全文' },
                ]}
                onChange={setView}
              />
              <CopyButton className="btn sm" text={file.content} label="复制产物" />
            </span>
          </header>
          <div className="cvm-artifact-body">
            <dl className="cvm-artifact-meta">
              <div>
                <dt>编译摘要</dt>
                <dd>
                  <code>{file.sha256}</code>
                </dd>
              </div>
              <div>
                <dt>执行方式 / 本机绑定</dt>
                <dd>
                  {selected.effect} / {selected.bindings}
                </dd>
              </div>
              <div>
                <dt>生效摘要</dt>
                <dd>
                  {applied?.effective_sha256 ? (
                    <code>{applied.effective_sha256}</code>
                  ) : (
                    <span className="dim">尚无已确认的生效摘要</span>
                  )}
                </dd>
              </div>
            </dl>
            <p className="note">
              生效摘要只在对应执行器确认后上报。沙箱的生效记录属于实际创建时的内容，热更新不会伪造新的创建回执。
            </p>
            {view === 'diff' && diff.length > 2000 && (
              <p className="note">差异较长，先显示前 2000 行。切换「全文」或复制可获取完整产物。</p>
            )}
            <pre className="cvm-artifact-content" aria-label={view === 'full' ? '产物全文' : '产物差异'}>
              {view === 'full'
                ? file.content
                : diff.slice(0, 2000).map((line, i) => (
                    <span key={i} className={line.t === '+' ? 'added' : line.t === '-' ? 'removed' : ''}>
                      {line.t} {line.s}
                    </span>
                  ))}
            </pre>
            {applied && (
              <details className="cvm-artifact-more">
                <summary>执行绑定值</summary>
                <pre className="cvm-artifact-content">{JSON.stringify(applied.bindings, null, 2)}</pre>
              </details>
            )}
          </div>
        </section>
      )}
      <details className="panel titled cvm-net-panel cvm-artifact-input">
        <summary>
          <h4 className="panel-title">冻结输入与镜像清单</h4>
          <span className="hint">该代实际采用的能力、规格和完整镜像清单，后续宿主变化不会改写</span>
        </summary>
        <div className="cvm-artifact-body">
          <pre className="cvm-artifact-content">{JSON.stringify(current.input, null, 2)}</pre>
        </div>
      </details>
    </div>
  );
}
