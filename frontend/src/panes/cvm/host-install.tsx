import { useQuery } from '@tanstack/react-query';
import { useState } from 'react';
import { fetchCvm, fetchCvmHostInstallCommand } from '../../api';
import { ErrorBox } from '../../ui/bits';
import { CopyButton } from '../../ui/copy-button';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { QUERY } from './queries';

const actions = [
  { label: '启用网络提供', flags: '--agent-flavor cvm' },
  { label: '安装组合执行栈', flags: '--enable-cvm' },
] as const;

export function HostInstallDialog({ node, name, onClose }: { node: string; name: string; onClose: () => void }) {
  const [flags, setFlags] = useState('');
  const [mode, setMode] = useState('');
  const workspace = useQuery({ queryKey: QUERY, queryFn: fetchCvm });
  const installer = useQuery({ queryKey: ['cvm-host-install-command'], queryFn: fetchCvmHostInstallCommand });
  const host = [...(workspace.data?.hosts ?? []), ...(workspace.data?.available_hosts ?? [])].find(
    item => item.node_id === node,
  );
  const cubeSelected = flags.split(' ').includes('--enable-cvm');
  const command =
    flags && installer.data
      ? [installer.data.install_command, flags, cubeSelected && mode ? '--cvm-mode ' + mode : '']
          .filter(Boolean)
          .join(' ')
      : '';
  return (
    <DialogLayer label="配置机器能力" onClose={onClose}>
      <section className="dialog-surface tunnel-dialog cvm-dialog">
        <header>
          <div>
            <b>配置机器能力</b>
            <p className="note">{name}</p>
          </div>
          <DialogClose className="btn">关闭</DialogClose>
        </header>
        <div className="tunnel-dialog-body cvm-dialog-body">
          <p className="note">在目标机器执行命令，沿用现有纳管凭据。Console 会显示 Agent 实际上报的能力。</p>
          {host?.capabilities.baremetal && (
            <p className="cvm-notice">
              这台机器仍在旧裸机独占模式。安装组合执行栈会保留宿主配置并迁移到 Cube 共存模式。
            </p>
          )}
          <div className="pv-host-config-options">
            <label>
              操作
              <select className="f" value={flags} onChange={event => setFlags(event.target.value)}>
                <option value="">请选择操作</option>
                {actions.map(action => (
                  <option key={action.flags} value={action.flags}>
                    {action.label}
                  </option>
                ))}
              </select>
            </label>
            {cubeSelected && (
              <label>
                虚拟化方式
                <select className="f" value={mode} onChange={event => setMode(event.target.value)}>
                  <option value="">自动 / 保留已安装方式</option>
                  <option value="kvm">KVM</option>
                  <option value="pvm">PVM</option>
                </select>
              </label>
            )}
          </div>
          {installer.isPending && <p className="note">正在获取安装命令…</p>}
          {installer.error && <ErrorBox error={installer.error} />}
          {command && (
            <div className="pv-cmd">
              <pre className="code" aria-label="机器配置命令">
                {command}
              </pre>
            </div>
          )}
        </div>
        <footer>{command && <CopyButton className="btn primary" text={command} label="复制命令" />}</footer>
      </section>
    </DialogLayer>
  );
}
