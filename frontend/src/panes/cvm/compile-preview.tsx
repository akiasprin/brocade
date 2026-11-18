import { useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { previewCvmCompile, type CvmCompilePreviewRequest, type CvmCompilePreview } from '../../api';
import { diffLines } from '../../forge/diff';
import { ErrorBox } from '../../ui/bits';

export function useCompilePreview(id: string, request: CvmCompilePreviewRequest | null) {
  const query = useQuery({
    queryKey: ['cvm-compile-preview', id, request],
    queryFn: () => {
      if (!request) throw new Error('请先填写配置');
      return previewCvmCompile(id, request);
    },
    enabled: false,
    retry: false,
    staleTime: Infinity,
  });
  return {
    query,
    inputSha256: query.data?.artifacts?.current?.input_sha256,
    ready: !!query.data && !query.error && !query.isFetching,
  };
}
export function CompilePreview({ preview }: { preview: ReturnType<typeof useCompilePreview> }) {
  const { query } = preview;
  return (
    <section className="cvm-compile-preview" aria-label="保存前编译预览">
      <header>
        <b>编译预览</b>
        <button type="button" className="btn small" disabled={query.isFetching} onClick={() => void query.refetch()}>
          {query.isFetching ? '正在编译…' : query.data ? '重新预览' : '预览产物差异'}
        </button>
      </header>
      {!query.data && !query.error && (
        <p className="note">
          先预览再保存。预览使用真实分配和编译流程，完成后回滚，不新增修订或占用资源。输入变化后须重新预览。
        </p>
      )}
      {query.error && <ErrorBox error={query.error} />}
      {query.data && <PreviewFiles data={query.data} />}
    </section>
  );
}
function PreviewFiles({ data }: { data: CvmCompilePreview }) {
  const [selected, setSelected] = useState('cube/attachments.json');
  const current = data.artifacts?.current;
  if (!current) return <p className="note">此执行器使用原规格路径，尚无原生编译产物。</p>;
  const previous = data.artifacts?.previous;
  const file = current.files.find(f => f.kind === selected) ?? current.files[0];
  const prior = previous?.files.find(f => f.kind === file?.kind);
  return (
    <>
      {data.no_op && <p role="status">配置没有变化，不会新增执行代或发布任务。</p>}
      <div className="cvm-artifact-files">
        {current.files.map(f => {
          const old = previous?.files.find(p => p.kind === f.kind);
          return (
            <button
              className={`cvm-artifact-file${f.kind === file?.kind ? ' active' : ''}`}
              type="button"
              key={f.kind}
              onClick={() => setSelected(f.kind)}
            >
              <code>{f.kind}</code>
              <span>{old?.sha256 === f.sha256 ? '内容不变' : old ? '内容变化' : '首次生成'}</span>
            </button>
          );
        })}
      </div>
      {file && (
        <pre className="cvm-artifact-content" aria-label="预览产物差异">
          {diffLines(prior?.content ?? '', file.content).map((line, i) => (
            <span key={i} className={line.t === '+' ? 'added' : line.t === '-' ? 'removed' : ''}>
              {line.t} {line.s}
            </span>
          ))}
        </pre>
      )}
      <details>
        <summary>冻结输入与镜像清单</summary>
        <pre className="cvm-artifact-content">{JSON.stringify(current.input, null, 2)}</pre>
      </details>
      <p className="note">
        保存时校验输入摘要；分配、镜像或能力变化会拒绝保存，须重新预览。发布审核使用同一份冻结产物。
      </p>
    </>
  );
}
