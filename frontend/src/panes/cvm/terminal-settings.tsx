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
  const supported = supportsGuestTerminal(host.capabilities);
  const status = !supported
    ? '运行栈待升级'
    : !host.terminal_enabled
      ? '已关闭'
      : !host.can_manage_terminal
        ? '已开启'
        : host.terminal_sessions
          ? `会话 ${host.terminal_sessions.used} / ${host.terminal_sessions.limit}`
          : '会话数未上报';
  return (
    <div className="cvm-host-terminal">
      {host.can_manage_terminal && (
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
      )}
      <small title={host.terminal_sessions ? 'Agent 最近上报的宿主 WebShell 会话占用' : undefined} className="mono">
        {status}
      </small>
      {change.error && <ErrorBox error={change.error} />}
      {change.isPending && (
        <p className="note" role="status">
          正在保存 WebShell 设置…
        </p>
      )}
    </div>
  );
}
