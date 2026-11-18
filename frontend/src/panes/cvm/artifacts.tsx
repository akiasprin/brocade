import { useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { getCvmArtifacts, type CvmArtifactKind, type CvmInstance } from '../../api';
import { diffLines } from '../../forge/diff';
import { ErrorBox } from '../../ui/bits';
import { CopyButton } from '../../ui/copy-button';
import { Section } from './presentation';

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
      <Section title="产物">
        <p className="note">
          这一代没有编译产物。旧
          Agent、尚未上报镜像清单的历史代和裸机实例继续使用原规格路径；下一次符合条件的规格冻结才会生成产物。
        </p>
      </Section>
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
  return (
    <div className="cvm-artifacts">
      <Section title="冻结产物" meta={`第 ${generation} 代 · ${current.compiler}`}>
        <p className="note">规格、镜像清单和能力快照共同固定输入。代号记出处；文件摘要不随代号变化。</p>
        <div className="cvm-artifact-status" role="status">
          {receipt
            ? receipt.mismatches.length
              ? receipt.mode === 'enforce'
                ? `产物校验不一致：${receipt.mismatches.join('、')}。本轮已拒绝执行。`
                : `影子比对不一致：${receipt.mismatches.join('、')}。仍由原规格路径执行。`
              : `${receipt.mode === 'enforce' ? 'Console 产物执行' : '影子比对一致 · 原规格路径执行'}`
            : '尚无这一代的产物比对回执'}
        </div>
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
      </Section>
      <details className="cvm-artifact-input">
        <summary>冻结输入与镜像清单</summary>
        <p className="note">这是该代实际采用的能力、规格和完整镜像清单。后续宿主变化不会改写此快照。</p>
        <pre className="cvm-artifact-content">{JSON.stringify(current.input, null, 2)}</pre>
      </details>
      {file && (
        <Section
          title={file.kind}
          meta={selected.effect}
          aside={
            <div className="cvm-artifact-tools">
              <button className="btn" aria-pressed={view === 'diff'} onClick={() => setView('diff')}>
                差异
              </button>
              <button className="btn" aria-pressed={view === 'full'} onClick={() => setView('full')}>
                全文
              </button>
              <CopyButton className="btn" text={file.content} label="复制产物" />
            </div>
          }
        >
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
                <code>{applied?.effective_sha256 ?? '尚无已确认的生效摘要'}</code>
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
            <details>
              <summary>执行绑定值</summary>
              <pre className="cvm-artifact-content">{JSON.stringify(applied.bindings, null, 2)}</pre>
            </details>
          )}
        </Section>
      )}
    </div>
  );
}
