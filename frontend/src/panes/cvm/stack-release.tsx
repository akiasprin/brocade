import { useEffect, useId, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type { NodeAgentStateItem } from '../../api';
import { ApiError } from '../../api/client';
import {
  cancelStackRelease,
  createStackRelease,
  fetchStackReleaseHistory,
  fetchStackReleases,
  STACK_COLD_PAIR_CONTRACT,
  STACK_FRESH_INSTALL_CONTRACT,
  type CreateStackRelease,
  type StackRelease,
  type StackReleaseHost,
  type StackReleasePair,
  type StackReleaseStatus,
  type StackReleaseView,
} from '../../api/cvm-stack-releases';
import { navigate } from '../../forge/route';
import { Empty, ErrorBox, Loading } from '../../ui/bits';
import { useNow } from '../../ui/clock';
import { TitleActions } from '../../ui/title-actions';
import { MachineTable, ReleaseLedger, ScopeToolbar, type MachineRow, type ReleaseTone } from '../deploy-cockpit';
import './stack-release.css';

const RELEASES = ['cvm-stack-releases'] as const;
const HISTORY = ['cvm-stack-release-history'] as const;
const OBSERVATION_MAX_AGE = 150;
const WINDOW_HOURS = 5;
const STATUS: Record<StackReleaseStatus, string> = {
  preparing: '准备中',
  ready: '准备中',
  applying: '切换中',
  succeeded: '切换成功',
  failed: '切换失败',
  recovered: '失败 · 已恢复',
  blocked: '已阻断 · 待恢复',
  cancelled: '已取消',
};
const INSTALL_STATUS: Partial<Record<StackReleaseStatus, string>> = {
  applying: '安装中',
  succeeded: '安装成功',
  failed: '安装失败',
};
const TONE_ORDER: Record<ReleaseTone, number> = { run: 0, err: 1, warn: 2, none: 3, ok: 4 };
const MODES = { install: '安装', upgrade: '升级' } as const;
type Mode = keyof typeof MODES;
type Action = { kind: 'create'; request: CreateStackRelease } | { kind: 'cancel'; id: number };

const canCancel = (status: StackReleaseStatus) => status === 'preparing' || status === 'ready';
const active = (release: StackRelease) => canCancel(release.status) || release.status === 'applying';
const open = (release: StackRelease) => active(release) || release.status === 'blocked';
const accessDenied = (error: unknown) => error instanceof ApiError && (error.status === 401 || error.status === 403);
const unixNow = () => Math.floor(Date.now() / 1000);
const stamp = (seconds: number) => new Date(seconds * 1000).toISOString();
const digest = (value: string) => value.slice(0, 8);
const statusOf = (release: StackRelease) =>
  (release.source_sha256 === null && INSTALL_STATUS[release.status]) || STATUS[release.status];

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

function assessHost(
  host: StackReleaseHost | undefined,
  release: StackRelease | undefined,
  catalog: StackReleasePair[],
  now: number,
  readFailed: boolean,
) {
  const evidence = evidenceState(host, now, readFailed);
  const fresh = evidence === 'fresh' ? host : undefined;
  const installed = fresh && !fresh.installable ? fresh : undefined;
  const current = installed?.bundle_sha256;
  const pairs = fresh ? catalog.filter(pair => compatiblePair(fresh, pair)) : [];
  const candidate: Mode | undefined =
    pairs.length && !(release && open(release)) ? (fresh?.installable ? 'install' : 'upgrade') : undefined;
  const confirmed = !!current && release?.status === 'succeeded' && current === release.target_sha256;
  const matchesCatalog =
    installed &&
    catalog.some(pair => pair.storage_backend === installed.storage_backend && pair.target_sha256 === current);
  const recovery = confirmed && release.source_sha256 !== null;
  const unknown = evidence === 'stale' ? '证据已过期，当前状态未知' : '宿主安装状态未知';
  const awaitsVerification = release?.status === 'succeeded' && !confirmed;
  const verificationNote = '当前观测尚未确认安装包摘要与目标包一致。';
  let tone: ReleaseTone = 'none';
  let status = '状态未知';
  let reason: string | undefined;

  if (release && open(release)) {
    tone = release.status === 'blocked' ? 'err' : 'run';
    status = statusOf(release);
    reason =
      release.error ?? (release.status === 'blocked' ? '请先核查宿主并恢复运行栈。' : !fresh ? unknown : undefined);
  } else if (!fresh) {
    reason = unknown;
  } else if (release?.status === 'failed' || release?.status === 'recovered') {
    tone = 'err';
    status = statusOf(release);
    reason = release.error ?? (candidate ? `可重新${MODES[candidate]}` : '当前宿主没有可用的兼容清单');
  } else if (candidate) {
    tone = 'warn';
    status = `可${MODES[candidate]}`;
    if (release?.status === 'cancelled') reason = `#${release.id} 已取消${release.error ? ` · ${release.error}` : ''}`;
    else if (awaitsVerification) reason = verificationNote;
  } else if (awaitsVerification) {
    status = '待核对';
    reason = verificationNote;
  } else if (confirmed || matchesCatalog) {
    tone = 'ok';
    status = '已一致';
  } else {
    status = fresh.installable ? '不可安装' : '不可升级';
    reason = fresh.installable ? '此宿主的存储后端没有可用的首次安装包。' : '当前运行栈和存储后端没有匹配的升级包对。';
  }

  return { candidate, pairs, current, fresh, recovery, tone, status, reason };
}

function StackTime({ seconds }: { seconds: number }) {
  const date = new Date(seconds * 1000);
  return (
    <time dateTime={date.toISOString()}>
      {date.toLocaleString(undefined, { dateStyle: 'short', timeStyle: 'short' })}
    </time>
  );
}

function MaintenanceWindow({ deadline, estimated = false }: { deadline: number; estimated?: boolean }) {
  return (
    <>
      维护窗口 {WINDOW_HOURS} 小时，{estimated && '预计 '}
      <StackTime seconds={deadline} /> 前未开始则自动作废。
    </>
  );
}

function PowerAction() {
  return (
    <button type="button" className="btn cgc-retry" onClick={() => navigate('cvm', {})}>
      前往容器电源操作
    </button>
  );
}

function EntryButton({
  mode,
  reason,
  primary,
  onClick,
}: {
  mode: Mode;
  reason?: string;
  primary: boolean;
  onClick: () => void;
}) {
  const id = useId();
  return (
    <div className="cube-stack-entry">
      <button
        type="button"
        data-cube-entry={mode}
        className={`btn${primary ? ' primary' : ''}`}
        aria-disabled={reason ? true : undefined}
        aria-describedby={reason ? id : undefined}
        onClick={() => {
          if (!reason) onClick();
        }}
      >
        {MODES[mode]} Cube
      </button>
      {reason && (
        <span id={id} className="cube-stack-entry-reason">
          {reason}
        </span>
      )}
    </div>
  );
}

function ReleaseHistory({ nameOf }: { nameOf: (id: string) => string }) {
  const [expanded, setExpanded] = useState(false);
  const [selected, setSelected] = useState<number>();
  const id = useId();
  const history = useQuery({
    queryKey: HISTORY,
    queryFn: ({ signal }) => fetchStackReleaseHistory(signal),
    enabled: expanded,
    refetchOnWindowFocus: false,
  });
  return (
    <section className="cgc-main cgc-span binary-history cube-stack-history" aria-label="运行栈发布记录">
      <button
        type="button"
        className="btn"
        aria-expanded={expanded}
        aria-controls={id}
        onClick={() => setExpanded(value => !value)}
      >
        {expanded ? '收起发布历史' : '发布历史'}
      </button>
      <div id={id} hidden={!expanded}>
        {expanded &&
          (history.isPending ? (
            <Loading />
          ) : history.error ? (
            <div role="alert">
              <ErrorBox error={history.error} />
            </div>
          ) : history.data.length === 0 ? (
            <Empty>暂无运行栈发布记录。</Empty>
          ) : (
            <ol className="binary-history-list">
              {history.data.map(release => (
                <li key={release.id}>
                  <button
                    className="binary-history-row"
                    type="button"
                    aria-expanded={selected === release.id}
                    onClick={() => setSelected(current => (current === release.id ? undefined : release.id))}
                  >
                    <span>
                      <b>#{release.id}</b> · {nameOf(release.node_id)} ·{' '}
                      {release.source_sha256 === null ? '安装' : '升级'}
                    </span>
                    <span>{statusOf(release)}</span>
                    <span className="mono">
                      {release.source_sha256 ? digest(release.source_sha256) : '未安装'} →{' '}
                      {digest(release.target_sha256)}
                    </span>
                  </button>
                  {selected === release.id && (
                    <div className="binary-history-detail">
                      <dl className="kv">
                        <dt>窗口截止</dt>
                        <dd>
                          <StackTime seconds={release.start_deadline_at} />
                        </dd>
                        {release.source_sha256 !== null && (
                          <>
                            <dt>来源包 SHA-256</dt>
                            <dd>
                              <code>{release.source_sha256}</code>
                            </dd>
                          </>
                        )}
                        <dt>目标包 SHA-256</dt>
                        <dd>
                          <code>{release.target_sha256}</code>
                        </dd>
                        {release.roots_sha256 && (
                          <>
                            <dt>卷根 SHA-256</dt>
                            <dd>
                              <code>{release.roots_sha256}</code>
                            </dd>
                          </>
                        )}
                      </dl>
                      {release.error && <p className="callout err">{release.error}</p>}
                    </div>
                  )}
                </li>
              ))}
            </ol>
          ))}
      </div>
    </section>
  );
}

export default function CubeStackReleaseTab({
  titleHost,
  fleet,
}: {
  titleHost: HTMLElement | null;
  fleet: NodeAgentStateItem[];
}) {
  const client = useQueryClient();
  const now = Math.floor(useNow() / 1000);
  const [mode, setMode] = useState<Mode>();
  const [nodeId, setNodeId] = useState('');
  const [pairId, setPairId] = useState('');
  const [submittedId, setSubmittedId] = useState<number>();
  const main = useRef<HTMLElement>(null);
  const entryToFocus = useRef<Mode>(undefined);
  useEffect(() => {
    if (mode) {
      const radio = main.current?.querySelector<HTMLInputElement>('input[type="radio"]:enabled');
      (radio ?? main.current)?.focus();
    } else if (entryToFocus.current && titleHost) {
      titleHost.querySelector<HTMLButtonElement>(`[data-cube-entry="${entryToFocus.current}"]`)?.focus();
      entryToFocus.current = undefined;
    }
  }, [mode, titleHost]);

  const query = useQuery({
    queryKey: RELEASES,
    queryFn: ({ signal }) => fetchStackReleases(signal),
    refetchInterval: ({ state }) =>
      accessDenied(state.error) ? false : state.data?.releases.some(active) ? 5_000 : 30_000,
  });
  const mutation = useMutation({
    mutationFn: (action: Action) =>
      action.kind === 'create' ? createStackRelease(action.request) : cancelStackRelease(action.id),
    onMutate: () => client.cancelQueries({ queryKey: RELEASES, exact: true }),
    onSuccess: (release, action) => {
      client.setQueryData<StackReleaseView>(
        RELEASES,
        previous =>
          previous && {
            ...previous,
            releases: [release, ...previous.releases.filter(item => item.node_id !== release.node_id)],
          },
      );
      if (action.kind === 'create') {
        setSubmittedId(release.id);
        setMode(undefined);
      } else {
        void client.invalidateQueries({ queryKey: HISTORY, exact: true });
      }
    },
    onSettled: () => client.invalidateQueries({ queryKey: RELEASES, exact: true }),
  });
  const data = accessDenied(query.error) ? undefined : query.data;
  const canWrite = !!data && !query.isError && !mutation.isPending;
  const catalog = data?.pairs ?? [];
  const releases = data?.releases ?? [];
  const hosts = new Map(data?.hosts.map(host => [host.node_id, host]));
  const releaseByNode = new Map(releases.map(release => [release.node_id, release]));
  const fleetById = new Map(fleet.map(node => [node.node_id, node]));
  const nameOf = (id: string) => fleetById.get(id)?.name || id;
  // The overview omits stale evidence. Keep active CVM nodes visible while awaiting a new report.
  const cvmNodes = fleet
    .filter(node => node.lifecycle_phase === 'active' && node.runtime_versions?.agent_flavor === 'cvm')
    .map(node => node.node_id);
  const rows = [...new Set([...hosts.keys(), ...releaseByNode.keys(), ...cvmNodes])]
    .map(id => {
      const host = hosts.get(id);
      const release = releaseByNode.get(id);
      return { id, host, release, ...assessHost(host, release, catalog, now, query.isError) };
    })
    .sort((a, b) => TONE_ORDER[a.tone] - TONE_ORDER[b.tone] || nameOf(a.id).localeCompare(nameOf(b.id)));
  const canInstall = rows.some(row => row.candidate === 'install');
  const canUpgrade = rows.some(row => row.candidate === 'upgrade');
  const onlyEmpty = rows.length > 0 && rows.every(row => !!row.fresh?.installable);
  const selected = rows.find(row => row.id === nodeId);
  const pairs = selected?.candidate === mode && selected ? selected.pairs : [];
  const pair = pairs.length === 1 ? pairs[0] : pairs.find(item => item.id === pairId);
  const submitted = releases.find(release => release.id === submittedId && canCancel(release.status));
  const canSubmit = canWrite && !!selected && !!pair && selected.candidate === mode;
  const error = mutation.error && (
    <div role="alert">
      <ErrorBox error={mutation.error} />
    </div>
  );
  const sharedReason = query.isError
    ? '宿主状态读取失败'
    : query.isPending
      ? '正在读取宿主状态'
      : mutation.isPending
        ? '正在提交发布操作'
        : undefined;
  const start = (next: Mode) => {
    entryToFocus.current = next;
    mutation.reset();
    setSubmittedId(undefined);
    setNodeId('');
    setPairId('');
    setMode(next);
  };
  const titleAction = (entry: Mode, primary = false) => (
    <EntryButton
      mode={entry}
      reason={
        sharedReason ?? ((entry === 'install' ? canInstall : canUpgrade) ? undefined : `暂无可${MODES[entry]}的宿主`)
      }
      primary={primary}
      onClick={() => start(entry)}
    />
  );
  const action =
    canInstall && !canUpgrade ? (
      <TitleActions primary={titleAction('install', !sharedReason)}>{titleAction('upgrade')}</TitleActions>
    ) : (
      <TitleActions primary={titleAction('upgrade', canUpgrade && !sharedReason)}>
        {titleAction('install')}
      </TitleActions>
    );

  const scope = mode && (
    <ScopeToolbar
      label={`${MODES[mode]}范围`}
      error={mutation.variables?.kind === 'create' ? error : undefined}
      note={
        <>
          {mode === 'upgrade'
            ? '提交前，请停止该宿主上正在运行的虚拟机并等待回执。预检通过后自动切换。'
            : '预检通过后自动安装。'}{' '}
          <MaintenanceWindow deadline={now + WINDOW_HOURS * 60 * 60} estimated />
        </>
      }
    >
      <span className="cgc-picked">{selected ? `已选 ${nameOf(selected.id)}` : `选择一台可${MODES[mode]}的宿主`}</span>
      {pairs.length > 1 && (
        <label className="cube-stack-pair">
          <span>{mode === 'install' ? '首次安装包' : '兼容运行包对'}</span>
          <select
            className="f"
            value={pairId}
            disabled={mutation.isPending}
            onChange={event => setPairId(event.target.value)}
          >
            <option value="">选择签名清单</option>
            {pairs.map(item => (
              <option key={item.id} value={item.id}>
                {digest(item.target_sha256)} · {item.id.slice(0, 16)}
              </option>
            ))}
          </select>
        </label>
      )}
      {selected && selected.candidate !== mode && <span className="cgc-picked">所选宿主目前不可{MODES[mode]}</span>}
      {pair && (
        <code className="cgc-mono" title={pair.target_sha256}>
          → {digest(pair.target_sha256)}
        </code>
      )}
      <span className="sp" />
      {mode === 'upgrade' && <PowerAction />}
      <button
        className="btn"
        type="button"
        disabled={mutation.isPending}
        onClick={() => {
          mutation.reset();
          setMode(undefined);
        }}
      >
        退出选择
      </button>
      <button
        className="btn primary"
        type="button"
        disabled={!canSubmit}
        onClick={() => {
          if (canSubmit && evidenceState(selected.host, unixNow(), query.isError) === 'fresh') {
            mutation.mutate({ kind: 'create', request: { node_id: selected.id, pair_id: pair.id } });
          }
        }}
      >
        {mutation.isPending && mutation.variables.kind === 'create' ? '提交中…' : MODES[mode]}
      </button>
    </ScopeToolbar>
  );

  const machineRows: MachineRow[] = rows.map(row => {
    const { id, host, release, tone, status, reason } = row;
    const target =
      release && open(release)
        ? release.target_sha256
        : row.pairs.length === 1
          ? row.pairs[0].target_sha256
          : undefined;
    return {
      node: {
        node_id: id,
        name: nameOf(id),
        public_ipv4_country: fleetById.get(id)?.public_ipv4_country,
        last_poll_at: host && host.observed_at <= now ? stamp(host.observed_at) : null,
      },
      tone,
      status,
      reason,
      current: row.current ? digest(row.current) : row.fresh?.installable ? '未安装' : null,
      currentTitle: row.current,
      target: target ? digest(target) : null,
      targetTitle: target,
      note:
        release &&
        (canCancel(release.status) && release.id !== submitted?.id ? (
          <small className="cube-stack-deadline">
            #{release.id} · 窗口截止 <StackTime seconds={release.start_deadline_at} />
          </small>
        ) : row.recovery ? (
          <small>仅启动维护前运行的虚拟机，逐台验证 VM Shim 冷启动。</small>
        ) : undefined),
      action:
        release &&
        (canCancel(release.status) ? (
          <button
            type="button"
            className="btn cgc-retry"
            aria-label={`取消发布 #${release.id}`}
            disabled={!canWrite}
            onClick={() => mutation.mutate({ kind: 'cancel', id: release.id })}
          >
            取消
          </button>
        ) : row.recovery && !mode ? (
          <PowerAction />
        ) : undefined),
      pick:
        mode && (row.candidate === mode || id === nodeId)
          ? {
              label: `${MODES[mode]} ${nameOf(id)}`,
              checked: id === nodeId,
              disabled: !canWrite || row.candidate !== mode,
              onChange: () => {
                mutation.reset();
                setNodeId(id);
                setPairId('');
              },
            }
          : undefined,
    };
  });

  const count = (tone: ReleaseTone) => rows.filter(row => row.tone === tone).length;
  const catalogFacts: [string, string][] = [];
  for (const [contract, label] of [
    [STACK_COLD_PAIR_CONTRACT, '升级包对'],
    [STACK_FRESH_INSTALL_CONTRACT, '首次安装包'],
  ] as const) {
    const matching = catalog.filter(pair => pair.contract === contract);
    if (query.isError || matching.length === 0) catalogFacts.push([label, query.isError ? '—' : '无']);
    else {
      for (const [backend, storage] of [
        ['xfs', 'XFS'],
        ['plain', 'Plain'],
      ] as const) {
        const pairs = matching.filter(pair => pair.storage_backend === backend);
        if (pairs.length)
          catalogFacts.push([
            `${label} · ${storage}`,
            pairs.length === 1 ? digest(pairs[0].target_sha256) : `${pairs.length} 份`,
          ]);
      }
    }
  }
  const latestUpgrade = releases.reduce<StackRelease | undefined>(
    (latest, release) => (release.source_sha256 !== null && (!latest || release.id > latest.id) ? release : latest),
    undefined,
  );
  const known = data && !query.isError;
  const label = onlyEmpty ? '已安装' : '已一致';
  return (
    <>
      {titleHost && !mode && createPortal(action, titleHost)}
      {query.isError && (
        <div className="cgc-main cgc-span" role="alert">
          <ErrorBox error={query.error} />
          {data && <p className="note">以下为上次读取的记录，当前状态未知。</p>}
        </div>
      )}
      {query.isPending && (
        <div className="cgc-main cgc-span">
          <Loading />
        </div>
      )}
      {data && (
        <>
          {rows.length > 0 && (
            <ReleaseLedger
              className="cube-stack-ledger"
              label={label}
              value={known ? count('ok') : '—'}
              unit={known ? `/ ${rows.length} 台` : '台'}
              period={onlyEmpty ? '尚无宿主安装 Cube' : 'Cube 宿主运行栈'}
              parts={[
                { tone: 'ok', label, count: known ? count('ok') : null },
                ...(count('run')
                  ? [{ tone: 'run' as const, label: '进行中', count: known ? count('run') : null }]
                  : []),
                { tone: 'warn', label: onlyEmpty ? '可安装' : '可安装 / 可升级', count: known ? count('warn') : null },
                ...(count('err')
                  ? [{ tone: 'err' as const, label: '失败 / 待恢复', count: known ? count('err') : null }]
                  : []),
                { tone: 'none', label: '暂无可用操作', count: known ? count('none') : null },
              ]}
              facts={[
                ...catalogFacts,
                ['最近升级记录', latestUpgrade ? `#${latestUpgrade.id} · ${statusOf(latestUpgrade)}` : '—'],
              ]}
            />
          )}
          <section
            className={`cgc-main cube-stack-release${rows.length ? '' : ' cgc-span'}`}
            aria-label="Cube 宿主"
            tabIndex={-1}
            ref={main}
          >
            {mutation.variables?.kind === 'cancel' && error}
            {!mode && submitted && (
              <div className="cgc-tbar cube-stack-submitted" role="status">
                <b>
                  发布 #{submitted.id} · {nameOf(submitted.node_id)}
                </b>
                <p className="cgc-tbar-note">
                  <MaintenanceWindow deadline={submitted.start_deadline_at} />
                </p>
              </div>
            )}
            {rows.length > 0 && (
              <MachineTable
                label="逐台 Cube 宿主状态"
                groups={[{ key: 'hosts', rows: machineRows }]}
                editing={!!mode}
                pickType="radio"
                observationLabel="上次观测"
                noObservation="—"
                toolbar={scope}
              />
            )}
            {hosts.size === 0 ? (
              <Empty>暂无有效的宿主运行栈上报。请检查节点的 CVM Agent 和运行栈更新公钥。</Empty>
            ) : (
              catalog.length === 0 && <Empty>暂无可用的签名运行包。请检查 Console 的运行栈发布目录。</Empty>
            )}
          </section>
          <ReleaseHistory nameOf={nameOf} />
        </>
      )}
    </>
  );
}
