import { useState, type FormEvent } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchSourceAccess,
  saveSourceAccess,
  sourceAccessPath,
  type SourceAccessNode,
  type SourceAccessScope,
  type SourceAccessView,
} from '../api';
import { ErrorBox } from '../ui/bits';
import { PanelTitle } from '../ui/icons';
import { FieldLoading } from '../ui/loading';
import { DialogClose, DialogLayer } from '../ui/dialog';
import './source-access.css';

export const SOURCE_ACCESS_PHASE_TEXT: Record<SourceAccessNode['phase'], string> = {
  pending: '待同步',
  applied: '已生效',
  unsupported: '待升级',
  error: '执行异常',
};

/**
 * One denial list per scope. A change is a function of the list it was made against and is applied to the cached
 * copy together with that copy's revision, so a concurrent edit is refused by the Console instead of silently undone
 * here. While a machine has yet to confirm the current list it is read every 3 s, about as often as Agents pull it;
 * otherwise every 30 s.
 */
export function useSourceAccess(scope: SourceAccessScope) {
  const qc = useQueryClient();
  const queryKey = ['source-access', sourceAccessPath(scope)];
  const query = useQuery({
    queryKey,
    queryFn: () => fetchSourceAccess(scope),
    refetchInterval: current => (current.state.data?.nodes.some(node => node.phase === 'pending') ? 3_000 : 30_000),
  });
  const save = useMutation({
    mutationFn: (change: (ips: string[]) => string[]) => {
      const current = qc.getQueryData<SourceAccessView>(queryKey);
      if (!current) throw new Error('请先读取封禁名单');
      return saveSourceAccess(scope, current.revision, change(current.ips));
    },
    onSuccess: view => qc.setQueryData(queryKey, view),
    onError: () => void qc.invalidateQueries({ queryKey }),
  });
  return { query, save };
}

/** 机器详情「配置」里的节点名单。用户名单在用户详情的「在线设备」里，见 user-source-access.tsx。 */
export function SourceAccessPanel({ nodeId, editable }: { nodeId: string; editable: boolean }) {
  const { query, save } = useSourceAccess({ nodeId });
  const [input, setInput] = useState('');
  const [confirm, setConfirm] = useState<string | null>(null);
  const [notice, setNotice] = useState('');
  const [expanded, setExpanded] = useState(false);
  const commit = (change: (ips: string[]) => string[], added = false) => {
    setNotice('');
    save.mutate(change, {
      onSuccess: () => {
        if (added) setInput('');
        setConfirm(null);
        setNotice('名单已保存。');
      },
    });
  };
  const add = (event: FormEvent) => {
    event.preventDefault();
    setNotice('');
    save.reset();
    if (input.trim()) setConfirm(input.trim());
  };
  const applied = query.data?.nodes.filter(item => item.phase === 'applied').length ?? 0;
  const nodes = query.data?.nodes ?? [];
  return (
    <details
      className="panel config-panel config-disclosure source-access-panel"
      aria-label="来源 IP 封禁"
      open={expanded}
      onToggle={event => setExpanded(event.currentTarget.open)}
    >
      <summary>
        <PanelTitle of="security">来源 IP 封禁</PanelTitle>
        {query.data && <span className="config-disclosure-summary">{query.data.ips.length} 个地址</span>}
        <span className="sp" />
        <span className="config-disclosure-toggle">{expanded ? '收起' : '展开'}</span>
      </summary>
      <div className="source-access-body">
        <p className="note">禁止这些来源接入此机器的代理服务。生效后关闭命中的会话。</p>
        {query.isPending && <FieldLoading label="读取封禁名单" />}
        {query.error && <ErrorBox error={query.error} />}
        {query.data && (
          <>
            <p
              className="source-access-status"
              role="status"
              title={nodes
                .map(
                  item =>
                    `${item.node_id}：${SOURCE_ACCESS_PHASE_TEXT[item.phase]}${item.error ? `（${item.error}）` : ''}`,
                )
                .join('\n')}
            >
              {nodes.length ? `已生效 ${applied}/${nodes.length} 台` : '暂无相关节点，接入授权后自动执行'}
              {nodes.some(item => item.phase === 'unsupported') && ' · 部分节点需要升级 Agent 和 Xray'}
              {nodes.some(item => item.phase === 'error') && ' · 部分节点执行异常'}
              {nodes.some(item => item.phase === 'pending') && ' · 等待节点同步'}
            </p>
            {nodes.some(item => item.error) && (
              <ul className="note" aria-label="节点执行说明">
                {nodes
                  .filter(item => item.error)
                  .map(item => (
                    <li key={item.node_id}>
                      {item.node_id}：{item.error}
                    </li>
                  ))}
              </ul>
            )}
            {query.data.ips.length ? (
              <ul className="source-access-list" aria-label="已封禁的来源 IP">
                {query.data.ips.map(ip => (
                  <li key={ip}>
                    <code>{ip}</code>
                    {editable && (
                      <button
                        className="btn small"
                        disabled={save.isPending}
                        onClick={() => commit(ips => ips.filter(value => value !== ip))}
                        aria-label={`解除封禁 ${ip}`}
                      >
                        解除
                      </button>
                    )}
                  </li>
                ))}
              </ul>
            ) : (
              <p className="dim">暂无封禁地址</p>
            )}
            {editable && (
              <form className="source-access-add" onSubmit={add}>
                <label>
                  <span>来源 IP</span>
                  <input
                    className="f mono"
                    value={input}
                    onChange={event => setInput(event.target.value)}
                    placeholder="IPv4 或 IPv6 地址"
                    autoComplete="off"
                    disabled={save.isPending}
                    maxLength={45}
                  />
                </label>
                <button className="btn" disabled={!input.trim() || save.isPending || query.data.ips.length >= 256}>
                  封禁 IP
                </button>
              </form>
            )}
            <small className="note">
              仅限指定来源，其他来源不受影响。经反向代理时按代理 IP 判定，用户与节点名单同时有效。
            </small>
          </>
        )}
        {notice && (
          <p className="note" role="status">
            {notice}
          </p>
        )}
        {save.error && <ErrorBox error={save.error} />}
      </div>
      {confirm && query.data && (
        <DialogLayer label="封禁来源 IP" onClose={() => setConfirm(null)} canClose={() => !save.isPending}>
          <div className="dialog-surface confirm-card source-access-dialog">
            <div className="confirm-title">封禁来源 IP</div>
            <div className="confirm-body">
              <p>
                封禁 <code>{confirm}</code> 后，将关闭此机器上该来源的代理会话，并拒绝重新接入。
              </p>
              {save.error && <ErrorBox error={save.error} />}
            </div>
            <div className="confirm-actions">
              <DialogClose className="btn" disabled={save.isPending}>
                取消
              </DialogClose>
              <button
                className="btn danger"
                disabled={save.isPending}
                onClick={() => commit(ips => [...ips, confirm], true)}
              >
                {save.isPending ? '保存中…' : '确认封禁'}
              </button>
            </div>
          </div>
        </DialogLayer>
      )}
    </details>
  );
}
