import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useId, useRef, useState, type ReactNode } from 'react';
import {
  createCvm,
  entryProtocolText,
  type CvmCreate,
  type CvmInstance,
  type CvmResources,
  type CvmWorkspace,
} from '../../api';
import { navigate } from '../../forge/route';
import { useSession } from '../../session';
import { ErrorBox } from '../../ui/bits';
import { Icon, PanelTitle } from '../../ui/icons';
import { useUnsavedChanges } from '../../ui/navigation-guard';
import { CapacityMeter } from './cards';
import {
  canCopyInstance,
  copyHost,
  copyHostProblem,
  copyName,
  createHosts,
  resourceInputsFor,
  validCreateResources,
} from './create-model';
import {
  CreateNetworkFields,
  initialCreateNetwork,
  useCreateNetwork,
  type CreateNetworkTemplate,
} from './create-network';
import { entryPortText } from './entry-ports';
import { initialInterfaces, interfaceProblem } from './interfaces';
import { uncertainCvmResult } from './network-common';
import { capacity, GIB, MIB, QUERY, requestId, invalidateCvmChanges } from './queries';
import './create.css';

export type { CreateNetworkTemplate } from './create-network';

type CreateAttempt = { payload: CvmCreate; network: CreateNetworkTemplate | null };
const cores = (value: number) => `${value} 核`;
const describeCapacity = (allocated: number | null, total: number, format: (value: number) => string) =>
  `${allocated === null ? '—' : format(allocated)} / ${total > 0 ? format(total) : '未上报'}`;

export function CreateForm({
  workspace,
  initialHost,
  initialCopy,
  onCreated,
}: {
  workspace: CvmWorkspace;
  initialHost?: string;
  initialCopy?: string;
  onCreated?: (id: string, network: CreateNetworkTemplate | null) => void;
}) {
  const copyId = useId();
  const qc = useQueryClient();
  const { who } = useSession();
  const hosts = createHosts(workspace, who.tenant_scope != null);
  const usable = hosts.flatMap(item => (item.reason ? [] : [item.host]));
  const tenants =
    workspace.tenants ??
    [...new Set(workspace.owners.map(o => o.tenant_id))].map(tenant_id => ({ tenant_id, name: tenant_id }));
  const requestedCopy = workspace.instances.find(
    item =>
      item.desired.id === initialCopy &&
      canCopyInstance(item) &&
      tenants.some(tenant => tenant.tenant_id === item.tenant_id),
  );
  const requestedCopyHost = requestedCopy && copyHost(requestedCopy, usable, initialHost);
  const initialSource = requestedCopyHost ? requestedCopy : undefined;
  const startHost = requestedCopyHost ?? (initialHost ? usable.find(item => item.node_id === initialHost) : usable[0]);
  const [copied, setCopied] = useState(initialSource);
  const [hostId, setHostId] = useState(startHost?.node_id ?? initialHost ?? '');
  const selectedHost = hosts.find(item => item.host.node_id === hostId);
  const host = selectedHost && !selectedHost.reason ? selectedHost.host : undefined;
  const defaultTenant = tenants.find(item => item.tenant_id === who.tenant_scope) ?? tenants[0];
  const [tenant, setTenant] = useState(initialSource?.tenant_id ?? defaultTenant?.tenant_id ?? '');
  const [user, setUser] = useState<string | null>(null);
  const [name, setName] = useState(initialSource ? copyName(initialSource) : '');
  const [changed, setChanged] = useState(false);
  const [imageId, setImageId] = useState(
    initialSource?.desired.spec.image?.id ?? startHost?.capabilities.images[0]?.id ?? '',
  );
  const [resourceInputs, setResourceInputs] = useState(() =>
    resourceInputsFor(startHost, initialSource?.desired.spec.resources),
  );
  const [networkDraft, setNetworkDraft] = useState(() => initialCreateNetwork(initialSource));
  const [labAccepted, setLabAccepted] = useState(false);
  const [localError, setLocalError] = useState<string | null>(
    initialCopy && !initialSource ? '所选源容器没有可用的复制目标，请重新选择。' : null,
  );
  const attempted = useRef<CreateAttempt | null>(null);
  const clearUnsaved = useUnsavedChanges(changed || !!name, '新建 容器', 'cvm:new');
  const image = host?.capabilities.images.find(item => item.id === imageId);
  const limits = host?.capabilities.resource_limits;
  const resources: CvmResources = {
    vcpus: Number(resourceInputs.vcpus),
    memory_bytes: Number(resourceInputs.memoryMiB) * MIB,
    root_disk_bytes: Number(resourceInputs.diskGiB) * GIB,
  };
  const validResources = validCreateResources(host, resources);
  const sources = workspace.instances.filter(item => item.tenant_id === tenant && canCopyInstance(item));
  const choices = useCreateNetwork({ enabled: workspace.can_write, host, tenant, user, draft: networkDraft });
  const hasNetworkChoices = !!networkDraft.entry.selection || !!networkDraft.nics.length;
  const create = useMutation({
    mutationFn: ({ payload }: CreateAttempt) => createCvm(payload),
    onSuccess: (result, { network }) => {
      clearUnsaved();
      qc.setQueryData<CvmWorkspace>(QUERY, old =>
        old
          ? { ...old, instances: [result, ...old.instances.filter(item => item.desired.id !== result.desired.id)] }
          : old,
      );
      invalidateCvmChanges(qc);
      if (onCreated) onCreated(result.desired.id, network);
      else navigate('cvm', { p: 'instance', id: result.desired.id });
    },
  });
  const isUncertain = !!create.error && uncertainCvmResult(create.error);
  const clearCopy = () => {
    if (!copied) return;
    if (name === copyName(copied)) setName('');
    setCopied(undefined);
    setNetworkDraft(initialCreateNetwork());
    setImageId(host?.capabilities.images[0]?.id ?? '');
    setResourceInputs(resourceInputsFor(host));
  };
  const applyCopy = (source: CvmInstance) => {
    const nextHost = copyHost(source, usable, hostId);
    if (!nextHost) return;
    if (!name.trim() || (copied && name === copyName(copied))) setName(copyName(source));
    setCopied(source);
    setUser(null);
    setHostId(nextHost.node_id);
    setImageId(source.desired.spec.image?.id ?? '');
    setResourceInputs(resourceInputsFor(nextHost, source.desired.spec.resources));
    setNetworkDraft(initialCreateNetwork(source));
    setLabAccepted(false);
    setLocalError(null);
  };
  const changeHost = (id: string) => {
    const next = usable.find(item => item.node_id === id);
    if (!next || id === hostId) return;
    setChanged(true);
    setHostId(id);
    setImageId(current =>
      next.capabilities.images.some(item => item.id === current)
        ? current
        : (copied?.desired.spec.image?.id ?? next.capabilities.images[0]?.id ?? ''),
    );
    if (!validCreateResources(next, resources))
      setResourceInputs(resourceInputsFor(next, copied?.desired.spec.resources));
    setLabAccepted(false);
  };
  const submit = () => {
    setLocalError(null);
    if (!workspace.can_write || create.isPending) return;
    // A successful write whose response was lost can change the very capacity being previewed.
    // Retries must replay the submitted body rather than revalidating against that new state.
    if (isUncertain && attempted.current) {
      create.mutate(attempted.current);
      return;
    }
    const interfaces = initialInterfaces(host);
    const problem =
      interfaceProblem(interfaces, host) ?? (copied && host && copyHostProblem(copied, host)) ?? choices.problem;
    if (problem || !name.trim() || !image || !tenants.some(item => item.tenant_id === tenant)) {
      setLocalError(problem ?? '请填写实例名称、系统镜像和所属租户。');
      return;
    }
    if (!validResources) {
      setLocalError('CPU、内存或系统盘超出机器支持范围。');
      return;
    }
    if (!host?.capabilities.stable_storage && !labAccepted) {
      setLocalError('请先确认：这台机器暂不支持关机、重启和修改配置。');
      return;
    }
    if (choices.network && !onCreated) {
      setLocalError('无法保留这些网络选项，请先移除，稍后在容器详情配置网络。');
      return;
    }
    const payload: CvmCreate = {
      request_id: attempted.current?.payload.request_id ?? requestId(),
      tenant_id: tenant,
      user_id: user,
      node_id: hostId,
      name: name.trim(),
      image_id: image.id,
      resources,
      interfaces,
    };
    if (attempted.current && JSON.stringify(attempted.current.payload) !== JSON.stringify(payload))
      payload.request_id = requestId();
    attempted.current = { payload, network: choices.network };
    create.mutate(attempted.current);
  };
  const plannedRows = selectedHost && [
    {
      label: 'vCPU',
      used: selectedHost.used.vcpus,
      add: resources.vcpus,
      total: selectedHost.host.capabilities.allocatable_vcpus,
      format: cores,
    },
    {
      label: '内存',
      used: selectedHost.used.memory,
      add: resources.memory_bytes,
      total: selectedHost.host.capabilities.allocatable_memory_bytes,
      format: capacity,
    },
    {
      label: '系统盘',
      used: selectedHost.used.disk,
      add: resources.root_disk_bytes,
      total: selectedHost.host.capabilities.allocatable_disk_bytes,
      format: capacity,
    },
  ];
  const entry = choices.network?.entry;
  const address = choices.entryOption?.offer.public_address ?? entry?.address;
  const entryPreview =
    entry?.mode.kind === 'protocols'
      ? `${address} · ${entryPortText(entry.mode)}`
      : entry?.mode.kind === 'l4_range'
        ? `${entryProtocolText(entry.mode.protocol)} ${address}:${entryPortText(entry.mode)} → ${entryPortText(entry.mode, true)}`
        : entry
          ? `整 IP ${address}`
          : null;
  const presets = limits
    ? SPEC_PRESETS.filter(
        preset =>
          preset.vcpus >= limits.min_vcpus &&
          preset.vcpus <= limits.max_vcpus &&
          preset.memory >= limits.min_memory_bytes &&
          preset.memory <= limits.max_memory_bytes &&
          preset.disk >= limits.min_root_disk_bytes &&
          preset.disk <= limits.max_root_disk_bytes &&
          preset.memory % limits.memory_step_bytes === 0 &&
          preset.disk % limits.root_disk_step_bytes === 0,
      )
    : [];
  const preset = presets.find(
    item =>
      item.vcpus === resources.vcpus &&
      item.memory === resources.memory_bytes &&
      item.disk === resources.root_disk_bytes,
  );
  const owners = workspace.owners.filter(item => item.tenant_id === tenant);
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
        <fieldset disabled={create.isPending || isUncertain || !workspace.can_write} className="cvm-new-steps">
          <CreateStep
            step={1}
            title="名称与归属"
            aside={
              <label className="cvm-new-copy" htmlFor={copyId}>
                <Icon of="copy" size={13} />
                <span>复制现有容器</span>
                <select
                  id={copyId}
                  value={copied?.desired.id ?? ''}
                  onChange={event => {
                    const source = sources.find(item => item.desired.id === event.target.value);
                    if (source) applyCopy(source);
                    else clearCopy();
                  }}
                >
                  <option value="">不复制</option>
                  {copied && !sources.some(item => item.desired.id === copied.desired.id) && (
                    <option value={copied.desired.id} disabled>
                      {copied.desired.spec.name}（源容器已不可用）
                    </option>
                  )}
                  {sources.map(source => {
                    const eligible = !!copyHost(source, usable, hostId);
                    return (
                      <option key={source.desired.id} value={source.desired.id} disabled={!eligible}>
                        {source.desired.spec.name}
                        {eligible ? '' : ' · 无匹配宿主'}
                      </option>
                    );
                  })}
                </select>
              </label>
            }
          >
            <div className="cvm-new-row">
              <label className="cvm-new-field cvm-new-grow">
                <span>名称</span>
                <input
                  aria-label="容器名称"
                  value={name}
                  maxLength={80}
                  onChange={event => setName(event.target.value)}
                  placeholder="例如 build-runner-2"
                />
              </label>
              <label className="cvm-new-field">
                <span>租户</span>
                <select
                  aria-label="所属租户"
                  value={tenant}
                  onChange={event => {
                    clearCopy();
                    setTenant(event.target.value);
                    setUser(null);
                    setNetworkDraft(initialCreateNetwork());
                    setLocalError(null);
                  }}
                >
                  {tenants.map(item => (
                    <option key={item.tenant_id} value={item.tenant_id}>
                      {item.name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="cvm-new-field">
                <span>使用者</span>
                <select aria-label="使用者" value={user ?? ''} onChange={event => setUser(event.target.value || null)}>
                  <option value="">暂不授权</option>
                  {owners.map(item => (
                    <option key={item.user_id} value={item.user_id}>
                      {item.name}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <p className="cvm-new-hint">
              {copied
                ? `复制 ${copied.desired.spec.name} 的镜像、规格与出站方式选项；不复制系统盘数据、登录凭证、授权、入站方式或路由。`
                : '管理员创建并配置容器，再授权给用户使用；也可以稍后在「使用授权」指定。'}
            </p>
          </CreateStep>
          <CreateStep step={2} title="宿主" meta="创建后不迁移">
            <div className="cvm-new-hosts">
              {hosts.map(({ host: item, used, reason }) => {
                const blocked = reason ?? (copied ? copyHostProblem(copied, item) : null);
                const caps = item.capabilities;
                const on = hostId === item.node_id;
                return (
                  <button
                    type="button"
                    className="cvm-new-host"
                    data-on={on || undefined}
                    key={item.node_id}
                    aria-pressed={on}
                    disabled={!!blocked}
                    onClick={() => changeHost(item.node_id)}
                  >
                    <span className="cvm-new-radio" aria-hidden="true" />
                    <span className="cvm-new-host-main">
                      <span>
                        <b>{item.name}</b> <small>{caps.backend}</small>
                      </span>
                      <small className={blocked ? 'cvm-new-why' : undefined}>
                        {blocked ?? `${used.instances} 个容器 · 最多 ${caps.max_interfaces} 个出站方式`}
                      </small>
                    </span>
                    {!blocked && item.fresh && !item.error && (
                      <span className="cvm-new-host-cap mono">
                        <span>
                          <i>vCPU</i> {describeCapacity(used.vcpus, caps.allocatable_vcpus, String)}
                        </span>
                        <span>
                          <i>内存</i> {describeCapacity(used.memory, caps.allocatable_memory_bytes, capacity)}
                        </span>
                      </span>
                    )}
                  </button>
                );
              })}
              {!hosts.length && <p className="note">没有已启用的容器宿主。</p>}
            </div>
          </CreateStep>
          <CreateStep step={3} title="系统" meta="创建后可通过重装更换">
            <div className="cvm-new-chips">
              {host?.capabilities.images.map(item => (
                <button
                  type="button"
                  className="cvm-new-chip"
                  key={item.id}
                  title={`${item.name} ${item.version} · ${item.architecture}`}
                  aria-label={`${item.name} ${item.version} · ${item.architecture}`}
                  aria-pressed={imageId === item.id}
                  onClick={() => {
                    setChanged(true);
                    setImageId(item.id);
                  }}
                >
                  {item.name} {item.version}
                  <small>{item.architecture}</small>
                </button>
              ))}
              {!host && <p className="note">选择可用宿主后显示可选系统。</p>}
            </div>
          </CreateStep>
          <CreateStep
            step={4}
            title="规格"
            meta={
              host?.capabilities.stable_storage
                ? '关机后保留系统盘数据，再次开机继续使用'
                : '这台机器暂不支持关机、重启和修改配置'
            }
          >
            {limits ? (
              <>
                {!!presets.length && (
                  <div className="cvm-new-chips" role="group" aria-label="常用规格">
                    {presets.map(item => (
                      <button
                        type="button"
                        className="cvm-new-chip mono"
                        key={item.label}
                        aria-pressed={preset === item}
                        onClick={() => {
                          setChanged(true);
                          setResourceInputs({
                            vcpus: String(item.vcpus),
                            memoryMiB: String(item.memory / MIB),
                            diskGiB: String(item.disk / GIB),
                          });
                        }}
                      >
                        {item.label}
                      </button>
                    ))}
                    <span className="cvm-new-chip-note">{preset ? '' : '自定义'}</span>
                  </div>
                )}
                <div className="cvm-new-spec">
                  <ResourceField
                    label="CPU（核）"
                    short="CPU"
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
                    short="内存"
                    unit="MiB"
                    value={resourceInputs.memoryMiB}
                    min={limits.min_memory_bytes / MIB}
                    max={limits.max_memory_bytes / MIB}
                    step={limits.memory_step_bytes / MIB}
                    onChange={value => {
                      setChanged(true);
                      setResourceInputs(old => ({ ...old, memoryMiB: value }));
                    }}
                  />
                  <ResourceField
                    label="系统盘（GiB）"
                    short="系统盘"
                    unit="GiB"
                    value={resourceInputs.diskGiB}
                    min={limits.min_root_disk_bytes / GIB}
                    max={limits.max_root_disk_bytes / GIB}
                    step={limits.root_disk_step_bytes / GIB}
                    onChange={value => {
                      setChanged(true);
                      setResourceInputs(old => ({ ...old, diskGiB: value }));
                    }}
                  />
                </div>
                <p className="cvm-new-hint">
                  {host?.name} 允许范围：vCPU {limits.min_vcpus}–{limits.max_vcpus} · 内存{' '}
                  {capacity(limits.min_memory_bytes)}–{capacity(limits.max_memory_bytes)} · 系统盘{' '}
                  {capacity(limits.min_root_disk_bytes)}–{capacity(limits.max_root_disk_bytes)}
                </p>
              </>
            ) : (
              <p className="note">这台机器尚不支持设置 CPU 和内存，请管理员升级宿主组件。</p>
            )}
          </CreateStep>
          <CreateStep step={5} title="网络与访问" meta="默认网络自动生成 · 密码自动生成">
            <div className="cvm-new-net">
              <div className="cvm-new-net-row">
                <span className="cvm-new-net-label">默认网络</span>
                <span className="cvm-chip via">
                  <span>默认出站</span>
                  <i>宿主 NAT</i>
                </span>
                <span className="cvm-new-hint">
                  创建时生成，优先级最低{host ? ` · 宿主最多支持 ${host.capabilities.max_interfaces} 个出站方式` : ''}
                </span>
              </div>
              <CreateNetworkFields
                choices={choices}
                draft={networkDraft}
                user={user}
                onChange={draft => {
                  setNetworkDraft(draft);
                  setChanged(true);
                  setLocalError(null);
                }}
              />
            </div>
            <p className="cvm-new-hint cvm-new-key">
              <Icon of="access" size={13} />
              无需预填 SSH 公钥或密码：系统首次启动为 <span className="mono">cube</span>{' '}
              生成高强度随机密码，创建生效后可在详情查看或重置。
              {hasNetworkChoices && ' 预选的网络不随容器保存，首次发布前请到新容器的「网络」中核对并保存。'}
            </p>
            {host && !host.capabilities.stable_storage && (
              <label className="cvm-check">
                <input type="checkbox" checked={labAccepted} onChange={event => setLabAccepted(event.target.checked)} />
                我了解这台机器暂不支持关机、重启和修改配置。
              </label>
            )}
          </CreateStep>
        </fieldset>
        <aside className="cvm-new-summary" aria-label="创建汇总">
          <section className="panel cvm-new-card">
            <header>
              <PanelTitle of="config">配置预览</PanelTitle>
            </header>
            <dl className="cvm-create-facts">
              <dt>名称</dt>
              <dd className={name.trim() ? 'mono' : 'dim'}>{name.trim() || '未填写'}</dd>
              <dt>宿主</dt>
              <dd>{host ? `${host.name} · ${host.capabilities.backend}` : '未选择宿主'}</dd>
              <dt>系统</dt>
              <dd>{image ? `${image.name} ${image.version} · ${image.architecture}` : '未选择系统'}</dd>
              <dt>规格</dt>
              <dd className="mono">
                {validResources
                  ? `${resources.vcpus} vCPU · ${capacity(resources.memory_bytes)} · ${capacity(resources.root_disk_bytes)}`
                  : '待确认'}
              </dd>
              <dt>使用者</dt>
              <dd>{owners.find(owner => owner.user_id === user)?.name ?? <span className="st">暂不授权</span>}</dd>
              <dt>登录</dt>
              <dd>
                自动生成密码（<span className="mono">cube</span>）
              </dd>
              <dt>初始网络</dt>
              <dd>
                默认出站 · 宿主 NAT
                {hasNetworkChoices &&
                  (choices.network
                    ? ['', entryPreview, ...choices.network.nics.map(choices.nicLabel)].filter(Boolean).join(' + ')
                    : ' + 选项待确认')}
              </dd>
            </dl>
          </section>
          <section className="panel cvm-new-card" aria-label="创建后的宿主分配">
            <p className="cvm-new-alloc-head">
              {host ? `创建后 ${host.name} 的分配` : '创建后的宿主分配'}
              <small>条纹为本次新增（分配量 / 宿主容量）</small>
            </p>
            {host && plannedRows ? (
              plannedRows.map(({ label, used, add, total, format }) => (
                <div className="cvm-capacity-row" key={label}>
                  <span>{label}</span>
                  {total > 0 && validResources ? (
                    <CapacityMeter used={used} add={add} total={total} allowOversell />
                  ) : (
                    <span />
                  )}
                  <b>{describeCapacity(validResources ? used + add : null, total, format)}</b>
                </div>
              ))
            ) : (
              <p className="note">选择可用宿主后显示分配预览。</p>
            )}
            <div className="cvm-new-submit">
              {localError && (
                <p className="error" role="alert">
                  {localError}
                </p>
              )}
              {create.error && <ErrorBox error={create.error} />}
              {isUncertain && <p className="note">上次创建结果尚未确认。可以按原内容重试，避免重复创建。</p>}
              <button className="btn primary" type="submit" disabled={create.isPending || !workspace.can_write}>
                {create.isPending ? '正在保存…' : isUncertain ? '重试保存创建变更' : '保存创建变更'}
              </button>
              <span className="note">
                {hasNetworkChoices
                  ? '先保存容器创建到待发布，进入「网络」后可核对并保存预填选项。'
                  : '保存后进入顶部待发布条，发布生效后开始创建。'}
              </span>
            </div>
          </section>
        </aside>
      </div>
    </form>
  );
}

const SPEC_PRESETS = [
  { label: '1C · 512M · 5G', vcpus: 1, memory: 512 * MIB, disk: 5 * GIB },
  { label: '2C · 2G · 20G', vcpus: 2, memory: 2 * GIB, disk: 20 * GIB },
  { label: '4C · 4G · 40G', vcpus: 4, memory: 4 * GIB, disk: 40 * GIB },
];

function CreateStep({
  step,
  title,
  meta,
  aside,
  children,
}: {
  step: number;
  title: string;
  meta?: string;
  aside?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="cvm-new-step" aria-label={title}>
      <span className="cvm-new-no" aria-hidden="true">
        {step}
      </span>
      <div className="cvm-new-body">
        <header>
          <h3>{title}</h3>
          {meta && <small>{meta}</small>}
          {aside}
        </header>
        {children}
      </div>
    </section>
  );
}

function ResourceField({
  label,
  short,
  unit,
  value,
  min,
  max,
  step,
  onChange,
}: {
  label: string;
  short: string;
  unit?: string;
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
      <label htmlFor={id}>{short}</label>
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
          title={`${min}–${max} · 步长 ${step}`}
          className="f"
          type="number"
          min={min}
          max={max}
          step={step}
          required
          value={value}
          onChange={event => onChange(event.target.value)}
        />
        {unit && <span className="cvm-stepper-unit">{unit}</span>}
        <button
          type="button"
          aria-label={`增加${label}`}
          disabled={!Number.isFinite(number) || number >= max}
          onClick={() => move(1)}
        >
          +
        </button>
      </div>
    </div>
  );
}
