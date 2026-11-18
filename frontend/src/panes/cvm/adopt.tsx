import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useRef, useState } from 'react';
import { adoptCvmHost, ApiError, type CvmAdopt, type CvmHost, type CvmWorkspace } from '../../api';
import { navigate } from '../../forge/route';
import { EmptyState, ErrorBox } from '../../ui/bits';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { capacity, ownerKey, QUERY, requestId } from './queries';

function adoptionBlock(host: CvmHost, workspace: CvmWorkspace) {
  if (workspace.instances.some(item => item.node_id === host.node_id && item.reserved)) return '裸机已纳管';
  if (host.error) return '执行能力不可用';
  if (!host.fresh) return '等待机器重新上报';
  return null;
}

export function AdoptHostAction({ workspace, onViewHosts }: { workspace: CvmWorkspace; onViewHosts: () => void }) {
  const [open, setOpen] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  if (!workspace.can_manage_quotas) return null;
  const host = workspace.hosts.find(item => item.node_id === selected && item.capabilities.baremetal);
  const close = () => {
    setOpen(false);
    setSelected(null);
  };
  return (
    <>
      <button className="btn" onClick={() => setOpen(true)}>
        纳管裸机
      </button>
      {open &&
        (host ? (
          <AdoptDialog host={host} workspace={workspace} onClose={close} />
        ) : (
          <HostPicker
            workspace={workspace}
            onClose={close}
            onSelect={setSelected}
            onViewHosts={() => {
              close();
              onViewHosts();
            }}
          />
        ))}
    </>
  );
}

function HostPicker({
  workspace,
  onClose,
  onSelect,
  onViewHosts,
}: {
  workspace: CvmWorkspace;
  onClose: () => void;
  onSelect: (node: string) => void;
  onViewHosts: () => void;
}) {
  const hosts = workspace.hosts.filter(host => host.capabilities.baremetal);
  const [node, setNode] = useState(hosts.find(host => !adoptionBlock(host, workspace))?.node_id ?? '');
  const selected = hosts.find(host => host.node_id === node);
  const blocked = selected ? adoptionBlock(selected, workspace) : null;
  return (
    <DialogLayer label="选择裸机" onClose={onClose}>
      <form
        className="dialog-surface tunnel-dialog cvm-dialog"
        onSubmit={event => {
          event.preventDefault();
          if (selected && !blocked) onSelect(selected.node_id);
        }}
      >
        <header>
          <div>
            <b>纳管裸机</b>
            <p className="note">沿用机器当前系统，将整机分配给用户。</p>
          </div>
          <DialogClose className="btn">关闭</DialogClose>
        </header>
        <div className="tunnel-dialog-body cvm-dialog-body">
          {hosts.length ? (
            <>
              <label className="cvm-field">
                选择裸机
                <select className="f" value={node} onChange={event => setNode(event.target.value)}>
                  <option value="">请选择机器</option>
                  {hosts.map(host => {
                    const reason = adoptionBlock(host, workspace);
                    return (
                      <option key={host.node_id} value={host.node_id} disabled={!!reason}>
                        {host.name}
                        {reason ? ` · ${reason}` : ''}
                      </option>
                    );
                  })}
                </select>
              </label>
              {selected && <p className="note">{selected.capabilities.baremetal!.os} · 整机独占 · 无容器隔离</p>}
              {blocked && <p className="note">{blocked}</p>}
              {!hosts.some(host => !adoptionBlock(host, workspace)) && (
                <p className="note">当前裸机均不可纳管。请在宿主页查看机器状态。</p>
              )}
            </>
          ) : (
            <EmptyState icon="nodes" title="尚无可纳管的裸机">
              {workspace.hosts.some(host => host.capabilities.backend.startsWith('cube'))
                ? '当前宿主以容器模式运行。目标机器启用裸机模式并上报后，会出现在这里。'
                : '目标机器启用裸机模式并添加为宿主后，会出现在这里。'}
            </EmptyState>
          )}
        </div>
        <footer>
          <button type="button" className="btn" onClick={onViewHosts}>
            查看宿主
          </button>
          {!!hosts.length && (
            <button type="submit" className="btn primary" disabled={!selected || !!blocked}>
              下一步
            </button>
          )}
        </footer>
      </form>
    </DialogLayer>
  );
}

export function AdoptHost({ host, workspace }: { host: CvmHost; workspace: CvmWorkspace }) {
  const [open, setOpen] = useState(false);
  const native = host.capabilities.baremetal;
  if (!native || !workspace.can_manage_quotas) return null;
  const blocked = adoptionBlock(host, workspace);
  return (
    <>
      <div className="cvm-card-footer">
        <button className="btn primary" disabled={!!blocked} onClick={() => setOpen(true)}>
          {blocked === '裸机已纳管' ? blocked : '纳管裸机'}
        </button>
      </div>
      {open && <AdoptDialog host={host} workspace={workspace} onClose={() => setOpen(false)} />}
    </>
  );
}

function AdoptDialog({ host, workspace, onClose }: { host: CvmHost; workspace: CvmWorkspace; onClose: () => void }) {
  const query = useQueryClient();
  const eligible = workspace.quotas.filter(q => q.enabled && q.allowed_nodes.includes(host.node_id));
  const [owner, setOwner] = useState(eligible[0] ? ownerKey(eligible[0].tenant_id, eligible[0].user_id) : '');
  const [name, setName] = useState(host.name);
  const [accepted, setAccepted] = useState(false);
  const attempt = useRef<CvmAdopt | null>(null);
  const native = host.capabilities.baremetal!;
  const blocked = adoptionBlock(host, workspace);
  const mutation = useMutation({
    mutationFn: async () => {
      if (blocked) throw new Error(blocked);
      const selected = eligible.find(q => ownerKey(q.tenant_id, q.user_id) === owner);
      if (!selected || !name.trim() || !accepted) throw new Error('请填写名称、选择已授权用户，并确认整机纳管。');
      const next = {
        request_id: attempt.current?.request_id ?? requestId(),
        node_id: host.node_id,
        tenant_id: selected.tenant_id,
        user_id: selected.user_id,
        name: name.trim(),
        identity: native.identity,
      };
      if (attempt.current && JSON.stringify(attempt.current) !== JSON.stringify(next))
        throw new Error('上一项纳管结果尚未确认，请先重试原请求。');
      attempt.current = next;
      return adoptCvmHost(next);
    },
    onError: error => {
      if (error instanceof ApiError && [400, 403, 409, 422].includes(error.status)) attempt.current = null;
      void query.invalidateQueries({ queryKey: QUERY });
    },
    onSuccess: instance => {
      query.setQueryData<CvmWorkspace>(QUERY, old =>
        old
          ? { ...old, instances: [...old.instances.filter(i => i.desired.id !== instance.desired.id), instance] }
          : old,
      );
      void query.invalidateQueries({ queryKey: QUERY });
      onClose();
      navigate('cvm', { p: 'instance', id: instance.desired.id });
    },
  });
  return (
    <DialogLayer label="纳管裸机" onClose={onClose} canClose={() => !mutation.isPending}>
      <form
        className="dialog-surface tunnel-dialog cvm-dialog"
        onSubmit={event => {
          event.preventDefault();
          mutation.mutate();
        }}
      >
        <header>
          <div>
            <b>纳管裸机</b>
            <p className="note">
              {host.name} · {native.os}
            </p>
          </div>
          <DialogClose className="btn" disabled={mutation.isPending}>
            关闭
          </DialogClose>
        </header>
        <div className="tunnel-dialog-body cvm-dialog-body">
          <label className="cvm-field">
            实例名称
            <input
              className="f"
              maxLength={80}
              value={name}
              onChange={e => setName(e.target.value)}
              disabled={mutation.isPending}
            />
          </label>
          <label className="cvm-field">
            使用用户
            <select className="f" value={owner} onChange={e => setOwner(e.target.value)} disabled={mutation.isPending}>
              {!eligible.length && <option value="">请先为用户授权此机器及整机额度</option>}
              {eligible.map(q => (
                <option key={ownerKey(q.tenant_id, q.user_id)} value={ownerKey(q.tenant_id, q.user_id)}>
                  {q.tenant_id} / {q.user_id}
                </option>
              ))}
            </select>
          </label>
          <p className="note">
            整机占用：{host.capabilities.allocatable_vcpus} 核 · {capacity(host.capabilities.allocatable_memory_bytes)}{' '}
            内存 · {capacity(host.capabilities.allocatable_disk_bytes)} 根文件系统。
          </p>
          <p className="note">
            沿用当前系统与数据。终端登录宿主账号 {native.terminal_user}；网络变更作用于整机。解除纳管保留系统和磁盘。
          </p>
          <label className="check">
            <input
              type="checkbox"
              checked={accepted}
              onChange={e => setAccepted(e.target.checked)}
              disabled={mutation.isPending}
            />
            确认此用户独占整台机器，业务直接运行于宿主系统，没有容器隔离
          </label>
          {mutation.error && <ErrorBox error={mutation.error} />}
          {blocked && <p className="note">{blocked}</p>}
        </div>
        <footer>
          <button
            type="submit"
            className="btn primary"
            disabled={mutation.isPending || !!blocked || !accepted || !owner || !name.trim()}
          >
            {mutation.isPending ? '提交中…' : '确认纳管'}
          </button>
        </footer>
      </form>
    </DialogLayer>
  );
}
