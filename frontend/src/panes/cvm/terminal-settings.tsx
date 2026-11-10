import { useMutation, useQueryClient } from '@tanstack/react-query';
import { setCvmTerminalPolicy, type CvmHost, type CvmWorkspace } from '../../api';
import { ErrorBox } from '../../ui/bits';
import { QUERY, supportsGuestTerminal } from './queries';

export function HostTerminalPolicy({ host }: { host: CvmHost }) {
  const client = useQueryClient();
  const change = useMutation({
    mutationFn: (enabled: boolean) => setCvmTerminalPolicy(host.node_id, enabled, host.terminal_revision),
    onSuccess: next => {
      const updated = next.hosts.find(item => item.node_id === host.node_id);
      client.setQueryData<CvmWorkspace>(QUERY, current =>
        current
          ? {
              ...current,
              hosts: current.hosts.map(item =>
                item.node_id === host.node_id && updated && updated.terminal_revision >= item.terminal_revision
                  ? {
                      ...item,
                      terminal_enabled: updated.terminal_enabled,
                      terminal_revision: updated.terminal_revision,
                    }
                  : item,
              ),
            }
          : next,
      );
      void client.invalidateQueries({ queryKey: QUERY });
    },
    onError: () => {
      void client.invalidateQueries({ queryKey: QUERY });
    },
  });
  return (
    <div className="cvm-host-terminal">
      <div className="cvm-card-footer">
        <span>
          WebShell
          <small>
            {!supportsGuestTerminal(host.capabilities)
              ? '运行栈待升级'
              : host.terminal_enabled
                ? '允许连接实例终端'
                : '已关闭'}
          </small>
        </span>
        {host.can_manage_terminal ? (
          <label className="switch" title="关闭后，该宿主上的现有终端会话会结束">
            <input
              type="checkbox"
              role="switch"
              aria-label={`${host.name} WebShell`}
              aria-checked={host.terminal_enabled}
              checked={host.terminal_enabled}
              disabled={change.isPending}
              onChange={event => change.mutate(event.target.checked)}
            />
            <span className="switch-ui" aria-hidden="true">
              <span />
            </span>
          </label>
        ) : (
          <span className="note">{host.terminal_enabled ? '已开启' : '已关闭'}</span>
        )}
      </div>
      {change.error && <ErrorBox error={change.error} />}
      {change.isPending && (
        <p className="note" role="status">
          正在保存 WebShell 设置…
        </p>
      )}
    </div>
  );
}
