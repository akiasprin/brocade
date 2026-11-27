import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { ApiError } from '../../api/client';
import {
  approveStackRelease,
  cancelStackRelease,
  createStackRelease,
  fetchStackReleases,
  STACK_COLD_PAIR_CONTRACT,
  STACK_FRESH_INSTALL_CONTRACT,
  type CreateStackRelease,
  type StackRelease,
  type StackReleaseHost,
  type StackReleasePair,
  type StackReleaseStatus,
  type StackReleaseView,
  type StackStorageBackend,
} from '../../api/cvm-stack-releases';
import { navigate } from '../../forge/route';
import { Confirm, Empty, ErrorBox, Loading } from '../../ui/bits';
import { useNow } from '../../ui/clock';
import { useNodeNames } from '../../ui/node-name';
import './stack-release.css';

const RELEASES = ['cvm-stack-releases'] as const;
// Match the window and observation limits in brocade-deployment/src/cvm/stack.rs.
const OBSERVATION_MAX_AGE = 150;
const WINDOW_MAX_AGE = 24 * 60 * 60;
const STATUS: Record<StackReleaseStatus, [string, string]> = {
  preparing: ['准备中', 'st-dispatched'],
  ready: ['待批准', 'st-gold'],
  approved: ['已批准', 'st-dispatched'],
  applying: ['正在切换', 'st-running'],
  succeeded: ['宿主运行栈切换成功', 'st-succeeded'],
  failed: ['切换失败', 'st-halted'],
  recovered: ['宿主已恢复', 'st-warn'],
  blocked: ['已阻断', 'st-halted'],
  cancelled: ['已取消', 'st-skipped'],
};

const FIRST_INSTALL_STATUS: Partial<Record<StackReleaseStatus, string>> = {
  applying: '正在首次安装',
  succeeded: '宿主运行栈首次安装成功',
  failed: '首次安装失败',
};
const FIRST_INSTALL_APPROVAL = '批准首次安装';

const canCancel = (status: StackReleaseStatus) => status === 'preparing' || status === 'ready' || status === 'approved';
const active = (release: StackRelease) => canCancel(release.status) || release.status === 'applying';
const open = (release: StackRelease) => active(release) || release.status === 'blocked';
const unixNow = () => Math.floor(Date.now() / 1000);
const expiredAt = (expiresAt: number, now: number) => expiresAt <= now;
const validWindow = (deadline: number, pairExpiry: number, now: number) =>
  Number.isFinite(deadline) && deadline > now && deadline <= Math.min(pairExpiry, now + WINDOW_MAX_AGE);
const accessDenied = (error: unknown) => error instanceof ApiError && (error.status === 401 || error.status === 403);
const storageLabel = (backend: StackStorageBackend) => (backend === 'xfs' ? 'XFS' : 'Plain');

function compatiblePair(host: StackReleaseHost, pair: StackReleasePair) {
  if (host.installable) {
    return pair.contract === STACK_FRESH_INSTALL_CONTRACT && pair.storage_backend === host.installable;
  }
  return (
    pair.contract === STACK_COLD_PAIR_CONTRACT &&
    pair.storage_backend === host.storage_backend &&
    pair.source_sha256 === host.bundle_sha256 &&
    pair.source_cubelet_sha256 === host.cubelet_sha256 &&
    pair.source_shim_sha256 === host.shim_sha256
  );
}

function evidenceState(host: StackReleaseHost | undefined, now: number, readFailed: boolean) {
  if (!host || readFailed || host.observed_at > now) return 'unknown';
  return now - host.observed_at <= OBSERVATION_MAX_AGE ? 'fresh' : 'stale';
}

function StackTime({ seconds }: { seconds: number }) {
  const date = new Date(seconds * 1000);
  return <time dateTime={date.toISOString()}>{date.toLocaleString()}</time>;
}

function StackDigest({ value }: { value?: string | null }) {
  return value ? <code>{value}</code> : '未知';
}

function HostEvidence({ host, now, readFailed }: { host?: StackReleaseHost; now: number; readFailed: boolean }) {
  const state = evidenceState(host, now, readFailed);
  return (
    <div>
      <div className={`st ${state === 'fresh' ? 'st-ok' : 'st-warn'}`}>
        {state === 'fresh'
          ? host?.installable
            ? '已确认宿主符合首次安装条件'
            : '宿主安装证据有效'
          : state === 'stale'
            ? '证据已过期，当前状态未知'
            : '宿主安装状态未知'}
      </div>
      {host && (
        <dl className="kv">
          <dt>观测时间</dt>
          <dd>
            <StackTime seconds={host.observed_at} />
          </dd>
          <dt>存储后端</dt>
          <dd>{storageLabel(host.installable ?? host.storage_backend)}</dd>
          {!host.installable && (
            <>
              <dt>安装包 SHA-256</dt>
              <dd>
                <StackDigest value={host.bundle_sha256} />
              </dd>
              <dt>Cubelet SHA-256</dt>
              <dd>
                <StackDigest value={host.cubelet_sha256} />
              </dd>
              <dt>已安装 Shim SHA-256</dt>
              <dd>
                <StackDigest value={host.shim_sha256} />
              </dd>
            </>
          )}
        </dl>
      )}
    </div>
  );
}

type Action = { kind: 'create'; request: CreateStackRelease } | { kind: 'approve' | 'cancel'; id: number };
type PendingConfirmation = Extract<Action, { id: number }>['kind'];

export default function CubeStackReleaseTab() {
  const client = useQueryClient();
  const nodeName = useNodeNames();
  const now = Math.floor(useNow() / 1000);
  const [nodeId, setNodeId] = useState('');
  const [pairId, setPairId] = useState('');
  const [expiry, setExpiry] = useState('');
  const [confirmation, setConfirmation] = useState<{ kind: PendingConfirmation; id: number }>();
  const [acknowledged, setAcknowledged] = useState(false);
  const query = useQuery({
    queryKey: RELEASES,
    queryFn: ({ signal }) => fetchStackReleases(signal),
    refetchInterval: ({ state }) =>
      accessDenied(state.error) ? false : state.data?.releases.some(active) ? 5_000 : 30_000,
  });
  const mutation = useMutation({
    mutationFn: (action: Action) => {
      if (action.kind === 'create') return createStackRelease(action.request);
      return action.kind === 'approve' ? approveStackRelease(action.id) : cancelStackRelease(action.id);
    },
    onMutate: () => client.cancelQueries({ queryKey: RELEASES }),
    onSuccess: (release, action) => {
      client.setQueryData<StackReleaseView>(
        RELEASES,
        previous =>
          previous && {
            ...previous,
            releases: [release, ...previous.releases.filter(item => item.id !== release.id)],
          },
      );
      if (action.kind === 'create') setExpiry('');
      else setConfirmation(undefined);
    },
    onSettled: () => client.invalidateQueries({ queryKey: RELEASES }),
  });
  const data = accessDenied(query.error) ? undefined : query.data;
  const canWrite = !!data && !query.isError && !mutation.isPending;
  const hosts = new Map(data?.hosts.map(host => [host.node_id, host]));
  const host = hosts.get(nodeId);
  const firstInstall = nodeId
    ? !!host?.installable
    : !!data?.hosts.length && data.hosts.every(item => !!item.installable);
  const pairs = host && data ? data.pairs.filter(pair => compatiblePair(host, pair)) : [];
  const pair = pairs.find(item => item.id === pairId);
  const expiresAt = Math.floor(Date.parse(expiry) / 1000);
  const validExpiry = !!pair && validWindow(expiresAt, pair.expires_at, now);
  const usableHost = evidenceState(host, now, query.isError) === 'fresh';
  const releases = [...(data?.releases ?? [])].sort(
    (left, right) => Number(open(right)) - Number(open(left)) || right.id - left.id,
  );
  const existingRelease = releases.find(release => release.node_id === nodeId && open(release));
  const canApprove = (release: StackRelease, at: number) => {
    const releaseHost = hosts.get(release.node_id);
    return (
      release.status === 'ready' &&
      !expiredAt(release.expires_at, at) &&
      releaseHost &&
      evidenceState(releaseHost, at, query.isError) === 'fresh' &&
      (release.source_sha256 === null
        ? !!releaseHost.installable
        : !releaseHost.installable && releaseHost.bundle_sha256 === release.source_sha256)
    );
  };
  const confirming = releases.find(release => release.id === confirmation?.id);
  const approval = confirmation?.kind === 'approve';
  const confirmingFirstInstall = confirming?.source_sha256 === null;
  const actionError = mutation.error && (
    <div role="alert">
      <ErrorBox error={mutation.error} />
    </div>
  );
  const ask = (kind: PendingConfirmation, id: number) => {
    mutation.reset();
    setAcknowledged(false);
    setConfirmation({ kind, id });
  };
  const preparationTitle = firstInstall ? '准备首次安装' : '准备冷窗口';
  const artifactLabel = firstInstall ? '首次安装包' : '兼容运行包对';

  return (
    <div className="cgc-main cgc-span cube-stack-release">
      <div className="callout cube-stack-instructions">
        {firstInstall ? (
          <p>
            <strong>单宿主首次安装</strong> ·
            选择与宿主存储后端匹配的运行包。安装成功后，核对当前安装包摘要与目标包一致。
          </p>
        ) : (
          <>
            <p>
              <strong>单宿主冷窗口</strong> ·
              批准前，在「容器」中记录并停止这台宿主上正在运行的受影响虚拟机，确认全部停稳。发布期间保持停机。
            </p>
            <p>
              宿主运行栈切换成功后，先核对当前安装包摘要与目标包一致，再通过「容器」启动窗口前正在运行的虚拟机，逐台验证
              VM Shim 的冷启动。原先已停止的虚拟机保持停止。本次发布涉及宿主运行栈，Guest OS 不会升级。
            </p>
            <button className="btn" type="button" onClick={() => navigate('cvm', {})}>
              前往容器电源操作
            </button>
          </>
        )}
      </div>
      {query.isError && (
        <div role="alert">
          <ErrorBox error={query.error} />
          {data && <p className="note">以下为上次读取的记录，当前状态未知。</p>}
        </div>
      )}
      {!confirming && actionError}
      {query.isPending && <Loading />}
      {data && (
        <div className="cube-stack-panels">
          <section className="panel" aria-label={preparationTitle}>
            <header>
              <h4>{preparationTitle}</h4>
            </header>
            {hosts.size === 0 ? (
              <Empty>没有宿主安装证据，当前状态未知。</Empty>
            ) : (
              <form
                className="cube-stack-form"
                onSubmit={event => {
                  event.preventDefault();
                  const at = unixNow();
                  if (
                    !canWrite ||
                    !pair ||
                    existingRelease ||
                    evidenceState(host, at, query.isError) !== 'fresh' ||
                    !validWindow(expiresAt, pair.expires_at, at)
                  )
                    return;
                  mutation.mutate({
                    kind: 'create',
                    request: { node_id: nodeId, pair_id: pair.id, expires_at: expiresAt },
                  });
                }}
              >
                <label>
                  <span>目标宿主</span>
                  <select
                    className="f"
                    value={nodeId}
                    onChange={event => {
                      setNodeId(event.target.value);
                      setPairId('');
                    }}
                    required
                  >
                    <option value="">选择一台宿主</option>
                    {nodeId && !host && <option value={nodeId}>{nodeName(nodeId)} · 状态未知</option>}
                    {data.hosts.map(item => (
                      <option key={item.node_id} value={item.node_id}>
                        {nodeName(item.node_id)}
                        {evidenceState(item, now, query.isError) !== 'fresh'
                          ? ' · 状态未知'
                          : item.installable
                            ? ' · 可首次安装'
                            : ''}
                      </option>
                    ))}
                  </select>
                </label>
                {nodeId && <HostEvidence host={host} now={now} readFailed={query.isError} />}
                {existingRelease && <p className="note">此宿主还有未结清的运行栈发布 #{existingRelease.id}。</p>}
                <label>
                  <span>{artifactLabel}</span>
                  <select
                    className="f"
                    value={pairId}
                    onChange={event => setPairId(event.target.value)}
                    required
                    disabled={!usableHost}
                  >
                    <option value="">{firstInstall ? '选择首次安装包' : '选择运行包对'}</option>
                    {pairs.map(item => (
                      <option key={item.id} value={item.id} disabled={expiredAt(item.expires_at, now)}>
                        {item.target_sha256.slice(0, 12)} · {item.id.slice(0, 16)}
                        {expiredAt(item.expires_at, now) ? ' · 已过期' : ''}
                      </option>
                    ))}
                  </select>
                </label>
                {usableHost && !pairs.some(item => !expiredAt(item.expires_at, now)) && (
                  <p className="note">
                    {firstInstall
                      ? '此宿主的存储后端没有可用的首次安装包。'
                      : '当前安装包和存储后端没有可用的兼容运行包对。'}
                  </p>
                )}
                {pair && (
                  <dl className="kv">
                    <dt>目标包 SHA-256</dt>
                    <dd>
                      <StackDigest value={pair.target_sha256} />
                    </dd>
                    <dt>{firstInstall ? '首次安装包 ID' : '运行包对 ID'}</dt>
                    <dd>
                      <code>{pair.id}</code>
                    </dd>
                    <dt>{firstInstall ? '首次安装包有效至' : '运行包对有效至'}</dt>
                    <dd>
                      <StackTime seconds={pair.expires_at} />
                    </dd>
                  </dl>
                )}
                <label>
                  <span>窗口截止（本地时间）</span>
                  <input
                    className="f"
                    type="datetime-local"
                    value={expiry}
                    onChange={event => setExpiry(event.target.value)}
                    required
                  />
                </label>
                <p className="note">
                  窗口最长 24 小时，截止时间须在{firstInstall ? '首次安装包' : '运行包对'}有效期内。
                </p>
                <div>
                  <button
                    className="btn primary"
                    type="submit"
                    disabled={!canWrite || !usableHost || !validExpiry || !!existingRelease}
                  >
                    {firstInstall ? '创建首次安装准备单' : '创建准备单'}
                  </button>
                </div>
              </form>
            )}
          </section>
          <section className="panel" aria-label="运行栈发布记录">
            <header>
              <h4>运行栈发布记录</h4>
            </header>
            {releases.length === 0 ? (
              <Empty>暂无运行栈发布记录。</Empty>
            ) : (
              <ol className="cube-stack-records">
                {releases.map(release => {
                  const [status, style] = STATUS[release.status];
                  const install = release.source_sha256 === null;
                  const releaseHost = hosts.get(release.node_id);
                  const observedTarget =
                    releaseHost &&
                    !releaseHost.installable &&
                    releaseHost.bundle_sha256 === release.target_sha256 &&
                    evidenceState(releaseHost, now, query.isError) === 'fresh';
                  const allowApproval = canApprove(release, now);
                  return (
                    <li key={release.id}>
                      <div className="chipline">
                        <strong title={release.node_id}>
                          #{release.id} · {nodeName(release.node_id)}
                          {install && ' · 首次安装'}
                        </strong>
                        <span className={`st ${style}`}>
                          {(install && FIRST_INSTALL_STATUS[release.status]) || status}
                        </span>
                      </div>
                      <dl className="kv">
                        <dt>窗口截止</dt>
                        <dd>
                          <StackTime seconds={release.expires_at} />
                        </dd>
                        {!install && (
                          <>
                            <dt>来源包 SHA-256</dt>
                            <dd>
                              <StackDigest value={release.source_sha256} />
                            </dd>
                          </>
                        )}
                        <dt>目标包 SHA-256</dt>
                        <dd>
                          <StackDigest value={release.target_sha256} />
                        </dd>
                        {(!install || release.roots_sha256) && (
                          <>
                            <dt>卷根 SHA-256</dt>
                            <dd>
                              <StackDigest value={release.roots_sha256} />
                            </dd>
                          </>
                        )}
                      </dl>
                      {release.error && <p className="callout err">{release.error}</p>}
                      {install && <p className="note">当前宿主观测</p>}
                      <HostEvidence host={releaseHost} now={now} readFailed={query.isError} />
                      {release.status === 'succeeded' && (
                        <p className="note">
                          {install
                            ? observedTarget
                              ? '当前安装包摘要与目标包一致。'
                              : '当前观测尚未确认安装包摘要与目标包一致。'
                            : 'VM Shim 仍待冷启动验证。'}
                        </p>
                      )}
                      {release.status === 'ready' && !allowApproval && (
                        <p className="note">
                          {expiredAt(release.expires_at, now)
                            ? '窗口已过期，无法批准。'
                            : install
                              ? '批准前需有确认宿主符合首次安装条件的有效观测。'
                              : '批准前需有与来源包一致的有效宿主安装证据。'}
                        </p>
                      )}
                      {canCancel(release.status) && (
                        <div className="chipline cube-stack-record-actions">
                          {release.status === 'ready' && (
                            <button
                              className="btn primary"
                              type="button"
                              disabled={!canWrite || !allowApproval}
                              onClick={() => ask('approve', release.id)}
                            >
                              {install ? FIRST_INSTALL_APPROVAL : '批准冷切换'}
                            </button>
                          )}
                          <button
                            className="btn"
                            type="button"
                            disabled={!canWrite}
                            onClick={() => ask('cancel', release.id)}
                          >
                            取消发布
                          </button>
                        </div>
                      )}
                    </li>
                  );
                })}
              </ol>
            )}
          </section>
        </div>
      )}
      {confirmation && confirming && (
        <Confirm
          title={`${approval ? (confirmingFirstInstall ? FIRST_INSTALL_APPROVAL : '批准冷切换') : '取消发布'} #${confirming.id}`}
          body={
            <>
              {approval ? (
                <>
                  <p>
                    目标宿主为 <strong>{nodeName(confirming.node_id)}</strong>
                    {confirmingFirstInstall
                      ? '。批准后 Agent 可立即开始首次安装。开始安装后无法取消。'
                      : '。批准后 Agent 可立即开始切换宿主运行栈。开始切换后无法取消。'}
                  </p>
                  {confirmingFirstInstall && (
                    <dl className="kv">
                      <dt>目标包 SHA-256</dt>
                      <dd>
                        <StackDigest value={confirming.target_sha256} />
                      </dd>
                    </dl>
                  )}
                  <p>
                    {!confirmingFirstInstall && '保持受影响虚拟机停机，直到核对宿主运行栈。'}窗口截止于{' '}
                    <StackTime seconds={confirming.expires_at} />。
                  </p>
                  <label className="cube-stack-ack">
                    <input
                      type="checkbox"
                      checked={acknowledged}
                      onChange={event => setAcknowledged(event.target.checked)}
                    />
                    {confirmingFirstInstall
                      ? '我已核对目标宿主、安装包和窗口，确认在此宿主首次安装 Cube 运行栈。'
                      : '我已记录原先运行的受影响虚拟机，并通过容器电源操作确认这些虚拟机已停止。'}
                  </label>
                </>
              ) : (
                <p>
                  取消 {nodeName(confirming.node_id)} 上的待执行{confirmingFirstInstall ? '首次安装' : '发布'}。
                  {!confirmingFirstInstall && '受影响虚拟机保持当前电源状态。'}
                </p>
              )}
              {actionError}
            </>
          }
          confirmLabel={mutation.isPending ? '正在提交…' : approval ? '确认批准' : '确认取消发布'}
          confirmDisabled={
            !canWrite || (approval ? !acknowledged || !canApprove(confirming, now) : !canCancel(confirming.status))
          }
          onCancel={() => setConfirmation(undefined)}
          onConfirm={() => {
            if (
              !canWrite ||
              (approval ? !acknowledged || !canApprove(confirming, unixNow()) : !canCancel(confirming.status))
            )
              return;
            mutation.mutate(confirmation);
          }}
        />
      )}
    </div>
  );
}
