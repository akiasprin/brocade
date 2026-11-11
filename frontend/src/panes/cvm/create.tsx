import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useId, useMemo, useRef, useState } from 'react';
import { ApiError, createCvm, type CvmCreate, type CvmResources, type CvmWorkspace } from '../../api';
import { navigate } from '../../forge/route';
import { useSession } from '../../session';
import { ErrorBox } from '../../ui/bits';
import { useUnsavedChanges } from '../../ui/navigation-guard';

import { CapacityMeter, quotaRows } from './cards';
import { initialInterfaces, interfaceProblem } from './interfaces';
import { Section } from './presentation';
import { capacity, GIB, MIB, ownerKey, QUERY, requestId, stackHosts, supportsCreate } from './queries';

export function CreateForm({ workspace }: { workspace: CvmWorkspace }) {
  const qc = useQueryClient();
  const usable = workspace.hosts.filter(supportsCreate);
  const [hostId, setHostId] = useState(usable[0]?.node_id ?? '');
  const host = usable.find(item => item.node_id === hostId);
  const { who } = useSession();
  const defaultOwner = who.self_user ?? workspace.owners[0];
  const [owner, setOwner] = useState(defaultOwner ? ownerKey(defaultOwner.tenant_id, defaultOwner.user_id) : '');
  const [name, setName] = useState('');
  const [changed, setChanged] = useState(false);
  const [imageId, setImageId] = useState(host?.capabilities.images[0]?.id ?? '');
  const [resourceInputs, setResourceInputs] = useState(() => ({
    vcpus: String(host?.capabilities.resource_limits?.min_vcpus ?? 1),
    memoryMiB: String((host?.capabilities.resource_limits?.min_memory_bytes ?? 128 * MIB) / MIB),
    diskGiB: String((host?.capabilities.resource_limits?.min_root_disk_bytes ?? 3 * GIB) / GIB),
  }));
  const resources: CvmResources = {
    vcpus: Number(resourceInputs.vcpus),
    memory_bytes: Number(resourceInputs.memoryMiB) * MIB,
    root_disk_bytes: Number(resourceInputs.diskGiB) * GIB,
  };
  const [publicKey, setPublicKey] = useState('');
  const [interfaces, setInterfaces] = useState(() =>
    initialInterfaces(host).map(connection => ({ key: requestId(), connection })),
  );
  const [labAccepted, setLabAccepted] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);
  const attempted = useRef<CvmCreate | null>(null);
  const clearUnsaved = useUnsavedChanges(changed || !!name || !!publicKey, '新建 容器', 'cvm:new');
  const image = host?.capabilities.images.find(item => item.id === imageId);
  const limits = host?.capabilities.resource_limits;
  const create = useMutation({
    mutationFn: createCvm,
    onSuccess: result => {
      clearUnsaved();
      qc.setQueryData<CvmWorkspace>(QUERY, old =>
        old
          ? { ...old, instances: [result, ...old.instances.filter(item => item.desired.id !== result.desired.id)] }
          : old,
      );
      void qc.invalidateQueries({ queryKey: QUERY });
      navigate('cvm', { p: 'instance', id: result.desired.id });
    },
  });
  const quota = useMemo(
    () => workspace.quotas.find(item => ownerKey(item.tenant_id, item.user_id) === owner),
    [owner, workspace.quotas],
  );
  const planned = [1, resources.vcpus, resources.memory_bytes, resources.root_disk_bytes, interfaces.length];
  const rows = quota ? quotaRows(workspace, quota).map((row, index) => ({ ...row, add: planned[index] })) : [];
  const usageReady = rows.length === 5;
  const overQuota = rows.some(row => !Number.isFinite(row.add) || row.used + row.add > row.total);
  const changeHost = (value: string) => {
    const next = usable.find(item => item.node_id === value);
    setHostId(value);
    setImageId(next?.capabilities.images[0]?.id ?? '');
    setResourceInputs({
      vcpus: String(next?.capabilities.resource_limits?.min_vcpus ?? 1),
      memoryMiB: String((next?.capabilities.resource_limits?.min_memory_bytes ?? 128 * MIB) / MIB),
      diskGiB: String((next?.capabilities.resource_limits?.min_root_disk_bytes ?? 3 * GIB) / GIB),
    });
    setInterfaces(initialInterfaces(next).map(connection => ({ key: requestId(), connection })));
    setLabAccepted(false);
  };
  const submit = () => {
    setLocalError(null);
    const problem = interfaceProblem(
      interfaces.map(row => row.connection),
      host,
    );
    if (!limits) {
      setLocalError('这台机器尚未开放 CPU、内存独立配置，请更新执行适配器。');
      return;
    }
    if (
      ![resources.vcpus, resources.memory_bytes, resources.root_disk_bytes].every(Number.isSafeInteger) ||
      resources.vcpus < limits.min_vcpus ||
      resources.vcpus > limits.max_vcpus ||
      resources.memory_bytes < limits.min_memory_bytes ||
      resources.memory_bytes > limits.max_memory_bytes ||
      resources.memory_bytes % limits.memory_step_bytes !== 0 ||
      resources.root_disk_bytes < limits.min_root_disk_bytes ||
      resources.root_disk_bytes > limits.max_root_disk_bytes ||
      resources.root_disk_bytes % limits.root_disk_step_bytes !== 0
    ) {
      setLocalError('CPU、内存或系统盘超出机器支持范围。');
      return;
    }
    if (problem || !name.trim() || !image || !owner || !publicKey.trim()) {
      setLocalError(problem ?? '请填写实例名称、系统镜像、规格、归属和 SSH 公钥。');
      return;
    }
    if (!quota?.enabled || !quota.allowed_nodes.includes(hostId)) {
      setLocalError('当前用户没有在这台机器创建 容器 的额度。');
      return;
    }
    if (!usageReady) {
      setLocalError('额度占用尚未就绪，请刷新后重试。');
      return;
    }
    if (overQuota) {
      setLocalError('创建后的资源占用超出用户额度。');
      return;
    }
    if (!host?.capabilities.stable_storage && !labAccepted) {
      setLocalError('请先确认这台机器尚未启用原盘恢复。');
      return;
    }
    const [tenant, user] = JSON.parse(owner) as [string, string];
    const payload: CvmCreate = {
      request_id: attempted.current?.request_id ?? requestId(),
      tenant_id: tenant,
      user_id: user,
      node_id: hostId,
      name: name.trim(),
      image_id: image.id,
      resources,
      ssh_public_key: publicKey.trim(),
      interfaces: interfaces.map(row => row.connection),
    };
    if (
      attempted.current &&
      JSON.stringify(attempted.current) !== JSON.stringify(payload) &&
      !(create.error instanceof ApiError && [400, 403, 409, 422].includes(create.error.status))
    ) {
      setLocalError('上次创建结果尚未确认，请先返回列表核查，或沿原内容重试，避免重复创建。');
      return;
    }
    if (attempted.current && JSON.stringify(attempted.current) !== JSON.stringify(payload))
      payload.request_id = requestId();
    attempted.current = payload;
    create.mutate(payload);
  };
  return (
    <form
      onChange={() => setChanged(true)}
      className="cvm-create"
      onSubmit={event => {
        event.preventDefault();
        submit();
      }}
    >
      <div className="cvm-config-layout">
        <fieldset disabled={create.isPending || !workspace.can_write} className="cvm-form-fieldset">
          <Section title="基本信息">
            <div className="cvm-form-grid">
              <label>
                实例名称
                <input
                  className="f"
                  value={name}
                  maxLength={80}
                  onChange={event => setName(event.target.value)}
                  placeholder="例如 my-cvm"
                />
              </label>
              <label>
                使用用户
                <select
                  className="f"
                  value={owner}
                  onChange={event => setOwner(event.target.value)}
                  disabled={who.role === 'user'}
                >
                  {workspace.owners.map(item => (
                    <option key={ownerKey(item.tenant_id, item.user_id)} value={ownerKey(item.tenant_id, item.user_id)}>
                      {item.name}
                    </option>
                  ))}
                </select>
              </label>
              <div className="cvm-field-note">
                {quota?.enabled
                  ? `额度上限：${quota.max_instances} 个实例 · ${capacity(quota.max_memory_bytes)} 内存`
                  : '尚未分配 容器 创建额度'}
              </div>
            </div>
          </Section>
          <Section title="宿主">
            <div className="cvm-pick-grid">
              {stackHosts(workspace).map(item => {
                const blocked = !supportsCreate(item)
                  ? !item.fresh
                    ? '报告过期'
                    : '未开放创建'
                  : !quota?.enabled || !quota.allowed_nodes.includes(item.node_id)
                    ? '当前用户未获授权'
                    : null;
                return (
                  <button
                    type="button"
                    className={`cvm-pick ${hostId === item.node_id ? 'on' : ''}`}
                    key={item.node_id}
                    aria-pressed={hostId === item.node_id}
                    disabled={!!blocked}
                    onClick={() => {
                      setChanged(true);
                      changeHost(item.node_id);
                    }}
                  >
                    <b>{item.name}</b>
                    <small>
                      {blocked ??
                        `${item.capabilities.allocatable_vcpus} vCPU · ${capacity(item.capabilities.allocatable_memory_bytes)} · ${capacity(item.capabilities.allocatable_disk_bytes)}`}
                    </small>
                  </button>
                );
              })}
            </div>
          </Section>
          <Section title="系统镜像">
            <div className="cvm-pick-grid">
              {host?.capabilities.images.map(item => (
                <button
                  type="button"
                  className={`cvm-pick image ${imageId === item.id ? 'on' : ''}`}
                  key={item.id}
                  aria-label={`${item.name} ${item.version} · ${item.architecture}`}
                  aria-pressed={imageId === item.id}
                  onClick={() => {
                    setChanged(true);
                    setImageId(item.id);
                  }}
                >
                  <span className="cvm-os">{item.name[0]}</span>
                  <span>
                    <b>
                      {item.name} {item.version}
                    </b>
                    <small>{item.architecture}</small>
                  </span>
                </button>
              ))}
            </div>
            {image && (
              <p className="note">
                {image.boot_mode === 'cube-oci' ? '独立 Guest 内核运行 OCI 系统环境。' : `启动方式：${image.boot_mode}`}{' '}
                系统版本已固定。
              </p>
            )}
          </Section>
          <Section title="实例规格">
            {limits ? (
              <div className="cvm-form-grid">
                <ResourceField
                  label="CPU（核）"
                  value={resourceInputs.vcpus}
                  min={limits.min_vcpus}
                  max={limits.max_vcpus}
                  step={1}
                  onChange={value => {
                    setChanged(true);
                    setResourceInputs(old => ({ ...old, vcpus: value }));
                  }}
                />
                <ResourceField
                  label="内存（MiB）"
                  value={resourceInputs.memoryMiB}
                  min={limits.min_memory_bytes / MIB}
                  max={limits.max_memory_bytes / MIB}
                  step={limits.memory_step_bytes / MIB}
                  onChange={value => {
                    setChanged(true);
                    setResourceInputs(old => ({ ...old, memoryMiB: value }));
                  }}
                />
              </div>
            ) : (
              <p className="note">机器尚未上报独立配置能力，请更新执行适配器。</p>
            )}
          </Section>
          <Section title="系统盘">
            {limits && (
              <ResourceField
                label="容量（GiB）"
                value={resourceInputs.diskGiB}
                min={limits.min_root_disk_bytes / GIB}
                max={limits.max_root_disk_bytes / GIB}
                step={limits.root_disk_step_bytes / GIB}
                onChange={value => {
                  setChanged(true);
                  setResourceInputs(old => ({ ...old, diskGiB: value }));
                }}
              />
            )}
            <p className="note">
              系统盘独立分配。
              {host?.capabilities.stable_storage ? '支持原盘恢复。' : '当前机器尚未启用原盘恢复。'}
            </p>
          </Section>
          <Section title="网络">
            <p>
              创建时生成主网卡，通过宿主机 NAT 访问公网，优先级最低。创建后可挂载已授权的业务网卡，并单独配置优先路由。
            </p>
            <p className="note">当前宿主开放 {host?.capabilities.max_interfaces ?? 0} 张网卡，含管理主网卡。</p>
          </Section>
          <Section title="访问与确认">
            <label className="cvm-field">
              SSH 公钥
              <textarea
                className="f mono"
                rows={3}
                value={publicKey}
                onChange={event => setPublicKey(event.target.value)}
                placeholder="ssh-ed25519 …"
              />
            </label>
            <p className="note">仅填写公钥。每个实例独立初始化访问身份。</p>
            {host && !host.capabilities.stable_storage && (
              <label className="cvm-check">
                <input type="checkbox" checked={labAccepted} onChange={event => setLabAccepted(event.target.checked)} />
                我了解此机器尚未启用原盘恢复，关机、重启和改配暂不可用。
              </label>
            )}
          </Section>
        </fieldset>
        <aside className="cvm-config-summary" aria-label="创建汇总">
          <h2>{name || '新实例'}</h2>
          <p>
            {host?.name ?? '未选择宿主'} · {image ? `${image.name} ${image.version}` : '未选择系统'}
          </p>
          <div className="cvm-summary-figures">
            <span>
              <b>{resourceInputs.vcpus || '—'}</b>
              <small>vCPU</small>
            </span>
            <span>
              <b>{resourceInputs.memoryMiB || '—'}</b>
              <small>MiB 内存</small>
            </span>
            <span>
              <b>{resourceInputs.diskGiB || '—'}</b>
              <small>GiB 系统盘</small>
            </span>
          </div>
          <h3>创建后的额度占用</h3>
          {rows.map(row => (
            <div className={`cvm-capacity-row ${row.used + row.add > row.total ? 'error' : ''}`} key={row.label}>
              <span>{row.label}</span>
              <CapacityMeter used={row.used} total={row.total} add={row.add} />
              <b>
                {Number.isFinite(row.add) ? row.format(row.used + row.add) : '—'} / {row.format(row.total)}
              </b>
            </div>
          ))}
          {!usageReady && <p className="note">额度占用尚未就绪，请刷新后重试。</p>}
          {!quota?.enabled && <p className="note">尚未分配创建额度</p>}
          {overQuota && (
            <p className="error" role="alert">
              创建后的资源占用超出用户额度。
            </p>
          )}
          <div className="cvm-submit">
            {localError && (
              <p className="error" role="alert">
                {localError}
              </p>
            )}
            {create.error && <ErrorBox error={create.error} />}
            <button
              className="btn primary"
              type="submit"
              disabled={create.isPending || !workspace.can_write || overQuota || !usageReady}
            >
              {create.isPending ? '创建中…' : '创建实例'}
            </button>
            <span className="note">创建进度会显示在实例详情中。</span>
          </div>
        </aside>
      </div>
    </form>
  );
}

function ResourceField({
  label,
  value,
  min,
  max,
  step,
  onChange,
}: {
  label: string;
  value: string;
  min: number;
  max: number;
  step: number;
  onChange: (value: string) => void;
}) {
  const id = useId();
  const number = value === '' ? min : Number(value);
  const move = (direction: number) => onChange(String(Math.min(max, Math.max(min, number + direction * step))));
  return (
    <div className="cvm-resource-field">
      <label htmlFor={id}>
        {label}
        <small id={`${id}-hint`}>
          {min}–{max} · 步长 {step}
        </small>
      </label>
      <div className="cvm-stepper">
        <button
          type="button"
          aria-label={`减少${label}`}
          disabled={!Number.isFinite(number) || number <= min}
          onClick={() => move(-1)}
        >
          −
        </button>
        <input
          id={id}
          aria-label={label}
          aria-describedby={`${id}-hint`}
          className="f"
          type="number"
          min={min}
          max={max}
          step={step}
          required
          value={value}
          onChange={event => onChange(event.target.value)}
        />
        <button
          type="button"
          aria-label={`增加${label}`}
          disabled={!Number.isFinite(number) || number >= max}
          onClick={() => move(1)}
        >
          ＋
        </button>
      </div>
    </div>
  );
}
