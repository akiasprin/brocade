import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import {
  mountCvmNic,
  fetchCvmChainCatalog,
  setCvmRoutes,
  changeCvmNicTransport,
  updateCvmNicVersion,
  type CvmInstance,
  type CvmNicCatalog,
  type CvmNicState,
  type CvmNicTransport,
  type CvmContainerRoutes,
  type CvmInterface,
} from '../../api';
import { EmptyState, ErrorBox } from '../../ui/bits';
import { useUnsavedChanges } from '../../ui/navigation-guard';
import { NIC_STATE, NicDialog, networkLines, transportName, useNicRequestId } from './network-common';
import { QUERY } from './queries';
import { CompilePreview, useCompilePreview } from './compile-preview';
import { routeTarget, retarget, targetKey } from './route-target';

function useNetworkSaved(onClose: () => void) {
  const client = useQueryClient();
  return () => {
    void client.invalidateQueries({ queryKey: QUERY });
    void client.invalidateQueries({ queryKey: NIC_STATE });
    onClose();
  };
}
const nicName = (nic: CvmInterface, catalog: CvmNicCatalog) =>
  catalog.publications.find(p => p.id === nic.allocated?.publication_id)?.spec.name ?? '业务网卡';

export function MountNicDialog({
  instance,
  catalog,
  state,
  onClose,
}: {
  instance: CvmInstance;
  catalog: CvmNicCatalog;
  state: CvmNicState;
  onClose: () => void;
}) {
  const [generation] = useState(instance.desired.generation);
  const [useDns, setUseDns] = useState(false);
  const [publication, setPublication] = useState(
    catalog.publications.find(p => state.mount_options[p.id]?.transports.length)?.id ?? '',
  );
  const kinds = state.mount_options[publication]?.transports ?? [];
  const [kind, setKind] = useState<CvmNicTransport>(kinds[0] ?? 'wireguard');
  const selected = catalog.publications.find(p => p.id === publication);
  const operationId = useNicRequestId();
  const onSaved = useNetworkSaved(onClose);
  const mountBody = {
    expected_generation: generation,
    publication_id: publication,
    transport: kind,
    use_dns_suggestions: useDns,
  };
  const mountRequest = { ...mountBody, request_id: operationId(mountBody) };
  const preview = useCompilePreview(instance.desired.id, { kind: 'mount', request: mountRequest });
  const save = useMutation({
    mutationFn: () => {
      return mountCvmNic(instance.desired.id, mountRequest, preview.inputSha256);
    },
    onSuccess: onSaved,
  });
  return (
    <NicDialog
      title="挂载网卡"
      pending={save.isPending}
      onClose={onClose}
      onSubmit={() => save.mutate()}
      submit="挂载并分配地址"
      disabled={!selected || !kinds.includes(kind) || !preview.ready}
    >
      {!catalog.publications.length ? (
        <EmptyState icon="forwarding" title="没有已授权的网络">
          请管理员创建网络，并授权给此容器的归属用户。
        </EmptyState>
      ) : (
        <>
          <label className="cvm-field">
            网络
            <select
              className="f"
              value={publication}
              onChange={e => {
                setPublication(e.target.value);
                setUseDns(false);
                setKind(state.mount_options[e.target.value]?.transports[0] ?? 'wireguard');
              }}
            >
              <option value="">选择网卡</option>
              {catalog.publications.map(p => (
                <option key={p.id} value={p.id}>
                  {p.spec.name}
                  {!state.mount_options[p.id]?.transports.length ? ' · 当前不可挂载' : ''}
                </option>
              ))}
            </select>
          </label>
          {state.mount_options[publication]?.error && (
            <p className="cvm-notice" role="status">
              {state.mount_options[publication].error}
            </p>
          )}
          {selected && (
            <>
              <dl className="cvm-facts">
                <div>
                  <dt>提供机器</dt>
                  <dd>
                    {catalog.providers.find(p => p.node_id === selected.spec.provider_node)?.name ??
                      selected.spec.provider_node}
                  </dd>
                </div>
                <div>
                  <dt>规格版本</dt>
                  <dd>{selected.revision}</dd>
                </div>
                <div>
                  <dt>可访问网络</dt>
                  <dd>{selected.spec.destinations.join('、') || '仅主动入站及回包'}</dd>
                </div>
              </dl>
              <label className="cvm-field">
                IP 承载
                <select className="f" value={kind} onChange={e => setKind(e.target.value as CvmNicTransport)}>
                  {kinds.map(kind => (
                    <option key={kind} value={kind}>
                      {transportName[kind]}
                    </option>
                  ))}
                </select>
              </label>
              {!!selected.spec.dns_suggestions.length && instance.desired.spec.execution?.kind !== 'baremetal' && (
                <label className="cvm-check">
                  <input type="checkbox" checked={useDns} onChange={e => setUseDns(e.target.checked)} />
                  将容器 DNS 设置为此网络建议：{selected.spec.dns_suggestions.join('、')}（替换当前设置）
                </label>
              )}
              <p className="note">
                每次挂载生成独立网卡和地址分配。初始路由：{selected.spec.route_presets.join('、') || '不自动添加'}
                ；之后可在路由表中调整。
              </p>
            </>
          )}
        </>
      )}
      <CompilePreview preview={preview} />
      {save.error && <ErrorBox error={save.error} />}
    </NicDialog>
  );
}

export function RoutesDialog({
  instance,
  catalog,
  onClose,
}: {
  instance: CvmInstance;
  catalog: CvmNicCatalog;
  onClose: () => void;
}) {
  const [base, setBase] = useState({
    generation: instance.desired.generation,
    revision: instance.desired.spec.routing.revision,
  });
  const [routing, setRouting] = useState<CvmContainerRoutes>(() => structuredClone(instance.desired.spec.routing));
  const [dns, setDns] = useState(routing.dns.join('\n'));
  const [localError, setLocalError] = useState<string | null>(null);
  const chains = useQuery({
    queryKey: ['cvm-chain-egress', instance.desired.id],
    queryFn: () => fetchCvmChainCatalog(instance.desired.id),
    refetchInterval: 15_000,
  });
  const nics = instance.desired.spec.interfaces.filter(n => n.allocated);
  const options = [
    ...nics.map(n => ({
      key: n.id,
      name: nicName(n, catalog),
      ipv4: !!n.allocated?.ipv4,
      ipv6: !!n.allocated?.ipv6,
      available: true,
    })),
    ...(chains.data?.chains ?? []).map(c => ({
      key: `chain:${c.id}`,
      name: c.unavailable_reason ? `${c.name} · ${c.unavailable_reason}` : c.name,
      ipv4: true,
      ipv6: c.ipv6,
      available: !!chains.data?.supported && c.authorized && !c.unavailable_reason,
    })),
  ];
  const operationId = useNicRequestId();
  const onSaved = useNetworkSaved(onClose);
  useUnsavedChanges(
    JSON.stringify(routing) !== JSON.stringify(instance.desired.spec.routing) ||
      dns !== instance.desired.spec.routing.dns.join('\n'),
    '容器路由表',
  );
  const routeBody = {
    expected_generation: base.generation,
    expected_revision: base.revision,
    routes: { ...routing, dns: networkLines(dns) },
  };
  const routeRequest = { ...routeBody, request_id: operationId(routeBody) };
  const preview = useCompilePreview(instance.desired.id, { kind: 'routes', request: routeRequest });
  const save = useMutation({
    mutationFn: () => {
      return setCvmRoutes(instance.desired.id, routeRequest, preview.inputSha256);
    },
    onSuccess: onSaved,
  });
  const ensurePriority = (next: CvmContainerRoutes, key: string) => {
    if (next.priorities.some(p => targetKey(p) === key)) return next;
    const option = options.find(o => o.key === key);
    const value = Math.min(65535, Math.max(0, ...next.priorities.flatMap(p => [p.ipv4 ?? 0, p.ipv6 ?? 0])) + 10);
    return {
      ...next,
      priorities: [
        ...next.priorities,
        { ...routeTarget(key), ipv4: option?.ipv4 ? value : null, ipv6: option?.ipv6 ? value : null },
      ],
    };
  };
  const submit = () => {
    if (
      routing.routes.some(
        r =>
          !r.destination.trim() ||
          !targetKey(r) ||
          (r.service &&
            (r.service.port_start < 1 || r.service.port_end > 65535 || r.service.port_end < r.service.port_start)),
      ) ||
      routing.priorities.some(p =>
        [p.ipv4, p.ipv6].some(v => v !== null && (!Number.isInteger(v) || v < 0 || v > 65535)),
      )
    ) {
      setLocalError('请填写目标 IP／网段，并将优先级设为 0–65535 的整数。');
      return;
    }
    setLocalError(null);
    save.mutate();
  };
  const stale = base.generation !== instance.desired.generation;
  return (
    <NicDialog
      title="容器路由表"
      pending={save.isPending}
      onClose={onClose}
      onSubmit={submit}
      disabled={stale || !preview.ready}
    >
      <p className="note">先比网段长短，再比协议端口，最后比较出口优先级；数值越小越优先。所选出口故障时保持不可达。</p>
      {stale && (
        <div className="cvm-notice" role="alert">
          <p>容器设置已变化，当前输入仍保留。</p>
          <button
            className="btn small"
            onClick={() => {
              setBase({ generation: instance.desired.generation, revision: instance.desired.spec.routing.revision });
              setRouting(structuredClone(instance.desired.spec.routing));
              setDns(instance.desired.spec.routing.dns.join('\n'));
              save.reset();
            }}
          >
            载入最新配置并替换当前输入
          </button>
        </div>
      )}
      <fieldset className="cvm-form-fieldset">
        <legend>出口优先级</legend>
        {routing.priorities.map(p => {
          const key = targetKey(p);
          const option = options.find(o => o.key === key);
          return (
            <div className="cvm-route-priority" key={key}>
              <b>
                {p.chain_id ? '线路 · ' : ''}
                {option?.name ?? '已失效的出口引用'}
              </b>
              {(['ipv4', 'ipv6'] as const).map(f => (
                <label className="cvm-field" key={f}>
                  {f === 'ipv4' ? 'IPv4 优先级' : 'IPv6 优先级'}
                  <input
                    className="f"
                    type="number"
                    min={0}
                    max={65535}
                    disabled={!option?.[f]}
                    value={p[f] ?? ''}
                    onChange={e =>
                      setRouting(old => ({
                        ...old,
                        priorities: old.priorities.map(x =>
                          targetKey(x) === key
                            ? { ...x, [f]: e.target.value === '' ? null : Number(e.target.value) }
                            : x,
                        ),
                      }))
                    }
                  />
                </label>
              ))}
              <button
                className="btn small"
                onClick={() =>
                  setRouting(old => ({
                    ...old,
                    priorities: old.priorities.filter(x => targetKey(x) !== key),
                    routes: old.routes.filter(x => targetKey(x) !== key),
                  }))
                }
              >
                移除此出口的路由引用
              </button>
            </div>
          );
        })}
        {!routing.priorities.length && <p className="note">添加路由时自动生成出口优先级，可继续调整。</p>}
      </fieldset>
      <fieldset className="cvm-form-fieldset">
        <legend>目标路由</legend>
        {routing.routes.map((r, i) => (
          <div className="cvm-route-row" key={i}>
            <label className="cvm-field">
              目标 IP／网段
              <input
                className="f"
                placeholder="0.0.0.0/0、::/0 或指定网段"
                value={r.destination}
                onChange={e =>
                  setRouting(old => ({
                    ...old,
                    routes: old.routes.map((x, j) => (j === i ? { ...x, destination: e.target.value } : x)),
                  }))
                }
              />
            </label>
            <div className="cvm-field">
              <label className="cvm-check">
                <input
                  type="checkbox"
                  checked={!!r.service}
                  onChange={e =>
                    setRouting(old => ({
                      ...old,
                      routes: old.routes.map((x, j) =>
                        j === i
                          ? {
                              ...x,
                              service: e.target.checked ? { protocol: 'tcp', port_start: 443, port_end: 443 } : null,
                            }
                          : x,
                      ),
                    }))
                  }
                />
                按协议端口（L4）
              </label>
              {r.service && (
                <div className="cvm-route-service-fields">
                  <select
                    className="f"
                    aria-label="协议"
                    value={r.service.protocol}
                    onChange={e =>
                      setRouting(old => ({
                        ...old,
                        routes: old.routes.map((x, j) =>
                          j === i && x.service
                            ? { ...x, service: { ...x.service, protocol: e.target.value as 'tcp' | 'udp' } }
                            : x,
                        ),
                      }))
                    }
                  >
                    <option value="tcp">TCP</option>
                    <option value="udp">UDP</option>
                  </select>
                  {(['port_start', 'port_end'] as const).map(field => (
                    <input
                      className="f"
                      key={field}
                      type="number"
                      min={1}
                      max={65535}
                      aria-label={field === 'port_start' ? '目的起始端口' : '目的结束端口'}
                      value={r.service?.[field] ?? ''}
                      onChange={e =>
                        setRouting(old => ({
                          ...old,
                          routes: old.routes.map((x, j) =>
                            j === i && x.service
                              ? { ...x, service: { ...x.service, [field]: Number(e.target.value) } }
                              : x,
                          ),
                        }))
                      }
                    />
                  ))}
                </div>
              )}
            </div>
            <label className="cvm-field">
              经由
              <select
                className="f"
                value={targetKey(r)}
                onChange={e =>
                  setRouting(old =>
                    ensurePriority(
                      { ...old, routes: old.routes.map((x, j) => (j === i ? retarget(x, e.target.value) : x)) },
                      e.target.value,
                    ),
                  )
                }
              >
                {!options.some(o => o.key === targetKey(r)) && <option value={targetKey(r)}>已失效的出口引用</option>}
                <optgroup label="网卡">
                  {options
                    .filter(o => !o.key.startsWith('chain:'))
                    .map(o => (
                      <option key={o.key} value={o.key}>
                        {o.name}
                      </option>
                    ))}
                </optgroup>
                <optgroup label="线路">
                  {options
                    .filter(o => o.key.startsWith('chain:'))
                    .map(o => (
                      <option key={o.key} value={o.key} disabled={!o.available}>
                        {o.name}
                        {!o.available ? ' · 不可选（授权或宿主能力）' : !o.ipv6 ? ' · IPv4' : ''}
                      </option>
                    ))}
                </optgroup>
              </select>
            </label>
            <button
              className="btn small"
              onClick={() => setRouting(old => ({ ...old, routes: old.routes.filter((_, j) => j !== i) }))}
            >
              移除路由
            </button>
          </div>
        ))}
        <button
          className="btn"
          disabled={!options.some(o => o.available) || routing.routes.length >= 256}
          onClick={() => {
            const key = options.find(o => o.available)?.key;
            if (key)
              setRouting(old =>
                ensurePriority({ ...old, routes: [...old.routes, { destination: '', ...routeTarget(key) }] }, key),
              );
          }}
        >
          ＋ 添加路由
        </button>
      </fieldset>
      {chains.error && <ErrorBox error={chains.error} />}
      {chains.data?.reason && <p className="cvm-notice">{chains.data.reason}</p>}
      <p className="note">
        线路使用 {chains.data?.owner?.name ?? '容器使用者'} 的接入授权、额度和限速；只转发 TCP/UDP，ping
        不通。经主网卡接入，不占业务网卡槽位；出网地址由线路落地决定。所选线路不通时不会回落宿主
        NAT。线路内分流和入口防护继续生效。
      </p>
      <p className="note">
        无匹配业务路由时
        {instance.desired.spec.execution?.kind === 'baremetal' ? '沿用宿主路由。' : '通过主网卡使用宿主 NAT。'}
        卸载网卡前先移除其路由与优先级引用。
      </p>
      <label className="cvm-field">
        容器 DNS（可选）
        <textarea
          className="f"
          rows={2}
          disabled={instance.desired.spec.execution?.kind === 'baremetal'}
          value={dns}
          onChange={e => setDns(e.target.value)}
          placeholder="每行一个解析服务器 IP；留空保留系统默认"
        />
      </label>
      <CompilePreview preview={preview} />
      {localError && (
        <p className="cvm-notice error" role="alert">
          {localError}
        </p>
      )}
      {save.error && <ErrorBox error={save.error} />}
    </NicDialog>
  );
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
      title="更换网卡承载"
      pending={save.isPending}
      onClose={onClose}
      onSubmit={() => save.mutate()}
      submit="更换承载"
      disabled={(kind === nic.allocated?.transport && !rebuild) || !available?.transports.includes(kind)}
    >
      <p>网卡 ID、MAC、已分配地址和路由引用保持不变，现有连接可能短暂中断。切换失败会保留或尝试恢复原承载。</p>
      <label className="cvm-field">
        承载类型
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
        重建所选承载并更新连接身份，保留网卡与地址
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
  const publication = catalog.publications.find(p => p.id === allocated.publication_id);
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
      pending={save.isPending}
      onClose={onClose}
      onSubmit={() => save.mutate()}
      submit="应用新规格"
      disabled={!publication || publication.revision === allocated.publication_revision}
    >
      <p>
        从版本 {allocated.publication_revision} 更新至版本 {publication?.revision ?? '不可用'}
        。网卡身份和独立路由表保留；不兼容的路由依赖需要先修改。
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
            <dt>支持承载</dt>
            <dd>{publication.spec.transports.map(t => transportName[t.kind]).join(' / ')}</dd>
          </div>
        </dl>
      )}
      {publication && (
        <label className="cvm-field">
          更新后使用的承载
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
        {instance.desired.spec.execution?.kind === 'baremetal'
          ? '明确允许运行中重新分配业务地址（现有业务连接会中断，管理联络保留）'
          : '如需改变地址，明确允许重新分配（需先关机）'}
      </label>
      {save.error && <ErrorBox error={save.error} />}
    </NicDialog>
  );
}
