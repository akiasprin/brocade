import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import {
  changeCvmNicTransport,
  updateCvmNicVersion,
  type CvmInstance,
  type CvmNicCatalog,
  type CvmNicState,
  type CvmNicTransport,
  type CvmInterface,
} from '../../api';
import { ErrorBox } from '../../ui/bits';
import { useUnsavedChanges } from '../../ui/navigation-guard';
import { NicDialog, transportName, uncertainCvmResult, useNicRequestId } from './network-common';
import { invalidateCvmChanges } from './queries';

function useNetworkSaved(onClose: () => void) {
  const client = useQueryClient();
  return () => {
    invalidateCvmChanges(client);
    onClose();
  };
}
export function TransportDialog({
  instance,
  nic,
  state,
  onClose,
}: {
  instance: CvmInstance;
  nic: CvmInterface;
  state: CvmNicState;
  onClose: () => void;
}) {
  const [rebuild, setRebuild] = useState(false);
  const [generation] = useState(instance.desired.generation);
  const [kind, setKind] = useState<CvmNicTransport>(nic.allocated!.transport);
  const [initialKind] = useState(nic.allocated!.transport);
  const dirty = kind !== initialKind || rebuild;
  useUnsavedChanges(dirty, '更换连接方式');
  const available = state.interface_options[nic.id];
  const operationId = useNicRequestId();
  const onSaved = useNetworkSaved(onClose);
  const save = useMutation({
    mutationFn: () => {
      const body = { expected_generation: generation, transport: kind, rebuild };
      return changeCvmNicTransport(instance.desired.id, nic.id, { ...body, request_id: operationId(body) });
    },
    onSuccess: onSaved,
  });
  return (
    <NicDialog
      title="更换网卡连接方式"
      error={save.error}
      uncertain={uncertainCvmResult(save.error)}
      pending={save.isPending}
      onClose={onClose}
      onSubmit={() => save.mutate()}
      dirty={dirty}

      disabled={(kind === nic.allocated?.transport && !rebuild) || !available?.transports.includes(kind)}
    >
      <p>切换容器宿主与出口机器之间的连接方式，网卡、地址和路由保持不变。发布时现有连接可能短暂中断。</p>
      <label className="cvm-field">
        连接方式
        <select className="f" value={kind} onChange={e => setKind(e.target.value as CvmNicTransport)}>
          {(['xray', 'wireguard'] as const).map(kind => (
            <option key={kind} value={kind} disabled={!available?.transports.includes(kind)}>
              {transportName[kind]}
              {!available?.transports.includes(kind) ? ' · 当前不可用' : ''}
            </option>
          ))}
        </select>
      </label>
      <label className="cvm-check">
        <input type="checkbox" checked={rebuild} onChange={e => setRebuild(e.target.checked)} />
        重新建立连接并更换连接凭据，保留网卡与地址
      </label>
      {available?.error && <p className="cvm-notice">{available.error}</p>}
      {save.error && <ErrorBox error={save.error} />}
    </NicDialog>
  );
}

export function NicVersionDialog({
  instance,
  nic,
  catalog,
  onClose,
}: {
  instance: CvmInstance;
  nic: CvmInterface;
  catalog: CvmNicCatalog;
  onClose: () => void;
}) {
  const allocated = nic.allocated!;
  const [publication] = useState(() => catalog.publications.find(p => p.id === allocated.publication_id));
  const [generation] = useState(instance.desired.generation);
  const [reallocate, setReallocate] = useState(false);
  const [transport, setTransport] = useState<CvmNicTransport>(
    publication?.spec.transports.some(t => t.kind === allocated.transport)
      ? allocated.transport
      : (publication?.spec.transports[0]?.kind ?? allocated.transport),
  );
  const operationId = useNicRequestId();
  const onSaved = useNetworkSaved(onClose);
  const save = useMutation({
    mutationFn: () => {
      const body = {
        expected_generation: generation,
        publication_revision: publication!.revision,
        reallocate,
        transport,
      };
      return updateCvmNicVersion(instance.desired.id, nic.id, { ...body, request_id: operationId(body) });
    },
    onSuccess: onSaved,
  });
  return (
    <NicDialog
      title="应用网卡新规格"
      error={save.error}
      uncertain={uncertainCvmResult(save.error)}
      pending={save.isPending}
      onClose={onClose}
      onSubmit={() => save.mutate()}
      dirty={reallocate || transport !== allocated.transport}

      disabled={!publication || publication.revision === allocated.publication_revision}
    >
      <p>
        从版本 {allocated.publication_revision} 更新至版本 {publication?.revision ?? '不可用'}
        。保留网卡和路由表。保存后等待发布，地址变化可能中断现有连接。
      </p>
      {publication && (
        <dl className="cvm-facts">
          <div>
            <dt>新地址池</dt>
            <dd>{[publication.spec.ipv4_pool, publication.spec.ipv6_pool].filter(Boolean).join(' / ')}</dd>
          </div>
          <div>
            <dt>新可访问网络</dt>
            <dd>{publication.spec.destinations.join('、') || '仅入站'}</dd>
          </div>
          <div>
            <dt>支持的连接方式</dt>
            <dd>{publication.spec.transports.map(t => transportName[t.kind]).join(' / ')}</dd>
          </div>
        </dl>
      )}
      {publication && (
        <label className="cvm-field">
          更新后使用的连接方式
          <select className="f" value={transport} onChange={e => setTransport(e.target.value as CvmNicTransport)}>
            {publication.spec.transports.map(offer => (
              <option key={offer.kind} value={offer.kind}>
                {transportName[offer.kind]}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className="cvm-check">
        <input type="checkbox" checked={reallocate} onChange={e => setReallocate(e.target.checked)} />
        {instance.desired.spec.execution.kind === 'baremetal'
          ? '明确允许运行中重新分配业务地址（现有业务连接会中断，管理联络保留）'
          : '如需改变地址，明确允许重新分配（需先关机）'}
      </label>
      {save.error && <ErrorBox error={save.error} />}
    </NicDialog>
  );
}
