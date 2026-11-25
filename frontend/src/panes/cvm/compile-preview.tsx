import { useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { previewCvmCompile, type CvmCompilePreviewRequest, type CvmCompilePreview } from '../../api';
import { diffLines } from '../../forge/diff';
import { ErrorBox } from '../../ui/bits';
import { uncertainCvmResult } from './network-common';

export function useCompilePreview(id: string, request: CvmCompilePreviewRequest | null) {
  const attempt = useRef<{ key: string; inputSha256: string | undefined } | null>(null);
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
    save: async <T,>(apply: (inputSha256: string | undefined) => Promise<T>) => {
      const key = JSON.stringify({ id, request });
      if (attempt.current?.key !== key) {
        // An optional preview may be stale. Check again before a new save.
        const result = await query.refetch({ throwOnError: true });
        attempt.current = { key, inputSha256: result.data?.artifacts?.current?.input_sha256 };
      }
      try {
        const result = await apply(attempt.current.inputSha256);
        attempt.current = null;
        return result;
      } catch (error) {
        // An uncertain response must retry the same reviewed request: the first
        // save may have succeeded. Definite rejection allows a fresh check.
        if (!uncertainCvmResult(error)) attempt.current = null;
        throw error;
      }
    },
  };
}
export function CompilePreview({ preview }: { preview: ReturnType<typeof useCompilePreview> }) {
  const { query } = preview;
  return (
    <section className="cvm-compile-preview" aria-label="配置检查">
      <header>
        <b>配置检查</b>
        <button type="button" className="btn small" disabled={query.isFetching} onClick={() => void query.refetch()}>
          {query.isFetching ? '正在检查…' : query.data ? '重新检查' : '查看配置差异'}
        </button>
      </header>
      {!query.data && !query.error && (
        <p className="note">保存时会自动检查配置。也可先查看差异，检查不会保存变更或占用资源。</p>
      )}
      {query.error && <ErrorBox error={query.error} />}
      {query.data && <PreviewFiles data={query.data} />}
    </section>
  );
}
function PreviewFiles({ data }: { data: CvmCompilePreview }) {
  const [selected, setSelected] = useState('cube/attachments.json');
  const current = data.artifacts?.current;
  if (!current) return <p className="note">配置检查通过。当前宿主不提供文件差异预览。</p>;
  const previous = data.artifacts?.previous;
  const file = current.files.find(f => f.kind === selected) ?? current.files[0];
  const prior = previous?.files.find(f => f.kind === file?.kind);
  return (
    <>
      <p role="status">{data.no_op ? '配置没有变化，无需重复保存。' : '配置检查通过，保存后等待发布。'}</p>
      <details>
        <summary>查看生成文件</summary>
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
          <summary>配置与镜像详情</summary>
          <pre className="cvm-artifact-content">{JSON.stringify(current.input, null, 2)}</pre>
        </details>
      </details>
    </>
  );
}
