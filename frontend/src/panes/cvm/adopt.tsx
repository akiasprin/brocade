import './hosts.css';
import { Section } from './presentation';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { adoptCvmHost, type CvmAdopt, type CvmHost, type CvmWorkspace } from '../../api';
import { navigate } from '../../forge/route';
import { EmptyState, ErrorBox } from '../../ui/bits';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { uncertainCvmResult, useCvmErrorNotice } from './network-common';
import { capacity, invalidateCvmChanges, ownerKey, QUERY, requestId } from './queries';

const baremetalHosts = (workspace: CvmWorkspace) =>
  workspace.hosts.filter(
    host =>
      host.capabilities.host_system?.identity &&
      host.capabilities.host_system.terminal_user &&
      host.capabilities.host_system.resources,
  );

function adoptionBlock(host: CvmHost, workspace: CvmWorkspace) {
  const caps = host.capabilities;
  if (!host.can_manage_host) return '当前账号无权纳管此机器';
  if (
    workspace.instances.some(
      item => item.node_id === host.node_id && item.reserved && item.desired.spec.execution.kind === 'baremetal',
    )
  )
    return '裸机已纳管';
  if (host.error) return '执行能力不可用';
  if (!host.fresh) return '等待机器重新上报';
  if (
    !caps.host_system?.resources?.vcpus ||
    !caps.host_system.resources.memory_bytes ||
    !caps.host_system.resources.root_disk_bytes
  )
    return '裸机容量尚未核验';
  return null;
}

export function AdoptForm({ workspace, onViewHosts }: { workspace: CvmWorkspace; onViewHosts: () => void }) {
  return workspace.can_manage_network ? (
    <AdoptionEditor workspace={workspace} onViewHosts={onViewHosts} />
  ) : (
    <EmptyState icon="nodes" title="当前账号无权纳管裸机">
      请联系系统管理员。
    </EmptyState>
  );
}

function BaremetalPicker({
  workspace,
  node,
  disabled,
  onSelect,
}: {
  workspace: CvmWorkspace;
  node: string;
  disabled?: boolean;
  onSelect: (node: string) => void;
}) {
  const hosts = baremetalHosts(workspace);
  if (!hosts.length)
    return (
      <EmptyState icon="nodes" title="尚无可纳管的裸机">
        目标机器安装或升级组合执行栈并上报后，会出现在这里。
      </EmptyState>
    );
  return (
    <>
      <label className="cvm-field">
        选择裸机
        <select className="f" value={node} onChange={event => onSelect(event.target.value)} disabled={disabled}>
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
      {!hosts.some(host => !adoptionBlock(host, workspace)) && (
        <p className="note">当前裸机均不可纳管。请在宿主页查看机器状态。</p>
      )}
    </>
  );
}

export function AdoptHost({ host, workspace }: { host: CvmHost; workspace: CvmWorkspace }) {
  const [open, setOpen] = useState(false);
  if (
    !baremetalHosts(workspace).some(item => item.node_id === host.node_id) ||
    !workspace.can_manage_network ||
    !host.can_manage_host
  )
    return null;
  const blocked = adoptionBlock(host, workspace);
  return (
    <>
      <button
        type="button"
        className="btn sm"
        title={blocked ?? undefined}
        disabled={!!blocked}
        onClick={() => setOpen(true)}
      >
        {blocked === '裸机已纳管' ? blocked : '纳管裸机'}
      </button>
      {open && <AdoptionEditor initialHost={host.node_id} workspace={workspace} onClose={() => setOpen(false)} />}
    </>
  );
}

function AdoptionEditor({
  workspace,
  initialHost,
  onClose,
  onViewHosts,
}: {
  workspace: CvmWorkspace;
  initialHost?: string;
  onClose?: () => void;
  onViewHosts?: () => void;
}) {
  const query = useQueryClient();
  const hosts = baremetalHosts(workspace);
  const owners = workspace.owners;
  const [node, setNode] = useState(
    () => initialHost ?? hosts.find(host => !adoptionBlock(host, workspace))?.node_id ?? '',
  );
  const [draftName, setDraftName] = useState<string>();
  const [owner, setOwner] = useState<string>();
  const [confirmed, setConfirmed] = useState<string | null>(null);
  const host = hosts.find(item => item.node_id === node);
  const native = host?.capabilities.host_system;
  const name = draftName ?? host?.name ?? '';
  const selectedOwner =
    owner == null ? owners[0] : owners.find(item => ownerKey(item.tenant_id, item.user_id) === owner);
  const ownerValue = selectedOwner ? ownerKey(selectedOwner.tenant_id, selectedOwner.user_id) : '';
  const confirmation = JSON.stringify([node, native?.identity, ownerValue]);
  const accepted = confirmed === confirmation;
  const blocked = host ? adoptionBlock(host, workspace) : node && '所选机器已不可纳管，请查看宿主状态。';
  const mutation = useMutation({
    mutationFn: adoptCvmHost,
    onError: () => {
      void query.invalidateQueries({ queryKey: QUERY });
    },
    onSuccess: instance => {
      query.setQueryData<CvmWorkspace>(QUERY, old =>
        old
          ? { ...old, instances: [...old.instances.filter(i => i.desired.id !== instance.desired.id), instance] }
          : old,
      );
      invalidateCvmChanges(query);
      onClose?.();
      navigate('cvm', { p: 'instance', id: instance.desired.id });
    },
  });
  const replay = uncertainCvmResult(mutation.error) ? mutation.variables : undefined;
  const locked = mutation.isPending || !!replay || mutation.isSuccess;
  const canSubmit =
    workspace.can_manage_network &&
    !mutation.isPending &&
    !mutation.isSuccess &&
    (!!replay || (!!native && !blocked && !!selectedOwner && !!name.trim() && accepted));
  const body = useCvmErrorNotice(mutation.error);
  const submit = () => {
    if (!canSubmit) return;
    if (replay) {
      // The server checks request identity before host eligibility. A refresh can already show the accepted request.
      mutation.mutate(replay);
      return;
    }
    if (!native || !selectedOwner) return;
    const request: CvmAdopt = {
      request_id: requestId(),
      node_id: node,
      tenant_id: selectedOwner.tenant_id,
      user_id: selectedOwner.user_id,
      name: name.trim(),
      identity: native.identity!,
    };
    mutation.mutate(request);
  };
  const fields = (
    <>
      <label className="cvm-field">
        实例名称
        <input
          className="f"
          maxLength={80}
          value={name}
          onChange={e => setDraftName(e.target.value)}
          disabled={locked}
        />
      </label>
      <label className="cvm-field">
        使用用户
        <select
          className="f"
          value={ownerValue}
          onChange={e => setOwner(e.target.value)}
          disabled={locked || !owners.length}
        >
          {!selectedOwner && <option value="">{owners.length ? '请选择使用用户' : '请先创建使用用户'}</option>}
          {owners.map(item => {
            const key = ownerKey(item.tenant_id, item.user_id);
            return (
              <option key={key} value={key}>
                {item.tenant_id} / {item.user_id}
              </option>
            );
          })}
        </select>
      </label>
    </>
  );
  const machine = host && native && (
    <>
      <dl className="cvm-adopt-facts">
        <dt>系统</dt>
        <dd>{native.os}</dd>
        <dt>管理网卡</dt>
        <dd>{native.primary_device}</dd>
        <dt>整机资源</dt>
        <dd>
          {native.resources?.vcpus} 核 · {capacity(native.resources?.memory_bytes ?? 0)} 内存 ·{' '}
          {capacity(native.resources?.root_disk_bytes ?? 0)} 根文件系统
        </dd>
        <dt>终端账号</dt>
        <dd>{native.terminal_user}</dd>
      </dl>
      <p className="note">沿用当前系统与数据。终端和网络操作作用于整机。解除纳管保留系统和磁盘。</p>
      <label className="check cvm-adopt-confirmation">
        <input
          type="checkbox"
          checked={accepted}
          disabled={locked || !!blocked || !selectedOwner}
          onChange={e => setConfirmed(e.target.checked ? confirmation : null)}
        />
        确认此用户可管理本物理系统；宿主业务没有容器隔离，Cube 容器保持独立运行
      </label>
    </>
  );
  const feedback = (
    <>
      {replay && (
        <p className="cvm-notice" role="status">
          上次纳管结果尚未确认。请重试原请求，或返回宿主清单核查结果。
        </p>
      )}
      {mutation.error && (
        <div role="alert">
          <ErrorBox error={mutation.error} />
        </div>
      )}
      {!replay && blocked && (
        <p className="note" role="status">
          {blocked}
        </p>
      )}
      {mutation.isSuccess && <p role="status">已保存纳管变更</p>}
    </>
  );
  const submitButton = (
    <button type="submit" className="btn primary" disabled={!canSubmit}>
      {mutation.isPending ? '提交中…' : replay ? '重试原请求' : '确认纳管'}
    </button>
  );
  const onSubmit: React.FormEventHandler<HTMLFormElement> = event => {
    event.preventDefault();
    submit();
  };

  if (onClose)
    return (
      <DialogLayer label="纳管裸机" onClose={onClose} canClose={() => !mutation.isPending}>
        <form className="dialog-surface tunnel-dialog cvm-dialog" onSubmit={onSubmit}>
          <header>
            <div>
              <b>纳管裸机</b>
              {host && <p className="note">{host.name}</p>}
            </div>
            <DialogClose className="btn" disabled={mutation.isPending}>
              关闭
            </DialogClose>
          </header>
          <div className="tunnel-dialog-body cvm-dialog-body" ref={body}>
            {fields}
            {machine}
            {feedback}
          </div>
          <footer>{submitButton}</footer>
        </form>
      </DialogLayer>
    );

  return (
    <form className="cvm-adopt" aria-label="纳管裸机" onSubmit={onSubmit}>
      <div className="cvm-adopt-layout" ref={body}>
        <Section className="cvm-panel cvm-adopt-section" title="裸机与使用者" bare>
          <BaremetalPicker
            workspace={workspace}
            node={node}
            disabled={locked}
            onSelect={value => {
              setNode(value);
              setDraftName(undefined);
              setConfirmed(null);
            }}
          />
          {(host || replay) && fields}
        </Section>
        {(host || replay || mutation.error) && (
          <Section className="cvm-panel cvm-adopt-section" title="整机纳管确认" bare>
            {machine}
            {feedback}
          </Section>
        )}
      </div>
      {!host && !replay && !mutation.error && feedback}
      <footer className="cvm-adopt-actions">
        <button type="button" className="btn" onClick={onViewHosts} disabled={mutation.isPending}>
          查看宿主
        </button>
        {(host || replay) && submitButton}
      </footer>
    </form>
  );
}
