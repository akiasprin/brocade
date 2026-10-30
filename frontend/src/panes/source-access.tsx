import { useId, useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchSourceAccess,
  fetchUserPresence,
  saveSourceAccess,
  sourceAccessPath,
  type NetworkOperator,
  type SourceAccessNode,
  type SourceAccessScope,
  type SourceAccessView,
  type UserPresenceList,
} from '../api';
import { navigate } from '../forge/route';
import { ErrorBox } from '../ui/bits';
import { CopyButton } from '../ui/copy-button';
import { Icon, PanelTitle } from '../ui/icons';
import { FieldLoading } from '../ui/loading';
import { RegionFlag } from '../ui/region-flag';
import { canonicalSourceIp, SOURCE_LOCATION_TITLE, SourceAddress, sourceNetwork } from '../ui/source-address';
import { GeneratedUserAvatar } from '../ui/user-avatar';
import './user-source-access.css';
import './source-access.css';

export const SOURCE_ACCESS_PHASE_TEXT: Record<SourceAccessNode['phase'], string> = {
  pending: '待同步',
  applied: '已生效',
  unsupported: '待升级',
  error: '执行异常',
};

/**
 * One denial list per scope. A change is a function of the list it was made against and is applied to the cached
 * copy together with that copy's revision, so a concurrent edit is refused by the Console instead of silently undone
 * here. While a machine has yet to confirm the current list it is read every 3 s, about as often as Agents pull it;
 * otherwise every 30 s.
 */
export function useSourceAccess(scope: SourceAccessScope) {
  const qc = useQueryClient();
  const queryKey = ['source-access', sourceAccessPath(scope)];
  const query = useQuery({
    queryKey,
    queryFn: () => fetchSourceAccess(scope),
    refetchInterval: current => (current.state.data?.nodes.some(node => node.phase === 'pending') ? 3_000 : 30_000),
  });
  const save = useMutation({
    mutationFn: (change: (ips: string[]) => string[]) => {
      const current = qc.getQueryData<SourceAccessView>(queryKey);
      if (!current) throw new Error('请先读取封禁名单');
      return saveSourceAccess(scope, current.revision, change(current.ips));
    },
    onSuccess: view => qc.setQueryData(queryKey, view),
    onError: () => void qc.invalidateQueries({ queryKey }),
  });
  return { query, save };
}

interface Holder {
  tenant: string;
  user: string;
}
interface Place {
  code: string | undefined;
  label: string;
}
interface UserList extends Holder {
  key: string;
  ips: string[];
}
interface Model {
  node: SourceAccessNode | undefined;
  nodeIps: string[];
  /** undefined：旧 Console 未提供用户名单，不能当作空。 */
  users: UserList[] | undefined;
  userEntries: number | undefined;
  /** 地址 → 名单里有它的用户。 */
  holders: Map<string, Holder[]>;
  /** 正在这台机器上在线的来源 → 在用的用户。 */
  online: Map<string, Holder[]>;
  place: (ip: string) => Place;
}

function modelOf(view: SourceAccessView, presence: UserPresenceList | undefined, nodeId: string): Model {
  const countries = new Map<string, string>();
  const operators = new Map<string, NetworkOperator>();
  for (const item of [...(view.source_countries ?? []), ...(presence?.source_countries ?? [])])
    countries.set(item.ip, item.country);
  for (const item of [...(view.source_operators ?? []), ...(presence?.source_operators ?? [])])
    operators.set(item.ip, item.operator);
  const users = view.user_bans?.map(item => ({
    key: `${item.tenant_id}/${item.user_id}`,
    tenant: item.tenant_id,
    user: item.user_id,
    ips: item.ips,
  }));
  const holders = new Map<string, Holder[]>();
  for (const list of users ?? [])
    for (const ip of list.ips) holders.set(ip, [...(holders.get(ip) ?? []), { tenant: list.tenant, user: list.user }]);
  const online = new Map<string, Holder[]>();
  for (const entry of presence?.users ?? [])
    for (const source of entry.sources)
      if (source.node_ids.includes(nodeId))
        online.set(source.ip, [...(online.get(source.ip) ?? []), { tenant: entry.tenant_id, user: entry.user_id }]);
  return {
    node: view.nodes.find(item => item.node_id === nodeId),
    nodeIps: view.ips,
    users,
    userEntries: users?.reduce((sum, list) => sum + list.ips.length, 0),
    holders,
    online,
    place: ip => sourceNetwork(countries.get(ip), operators.get(ip)),
  };
}

const joinNames = (holders: Holder[]) => holders.map(holder => holder.user).join('、');
const ROW_LIMIT = 8;

const PHASE_TONE: Record<SourceAccessNode['phase'], string> = {
  applied: 'ok',
  pending: 'gold',
  unsupported: 'gold',
  error: 'err',
};

/** 标题栏右端的执行情况。只有一台机器，不再点开逐台列出；等待原因放在悬停说明里。 */
function Status({ node }: { node: SourceAccessNode | undefined }) {
  if (!node) return null;
  return (
    <span className={`source-access-status ${PHASE_TONE[node.phase]}`} role="status" title={node.error ?? undefined}>
      <span className="source-access-dot" aria-hidden="true" />
      {node.phase === 'pending' ? '同步中' : SOURCE_ACCESS_PHASE_TEXT[node.phase]}
    </span>
  );
}

/** 异常与待升级意味着名单没有在这台机器上执行，原因写进正文一行；同步中只在标题栏。 */
function PhaseLine({ node }: { node: SourceAccessNode | undefined }) {
  if (!node || (node.phase !== 'error' && node.phase !== 'unsupported')) return null;
  return (
    <p className={`source-access-phase ${node.phase === 'error' ? 'err' : 'gold'}`}>
      <Icon of="warn" size={13} />
      <span>
        {node.phase === 'unsupported'
          ? '这台机器需升级 Agent 与 Xray 才会执行封禁'
          : node.error || 'Xray 接入策略执行异常，请检查节点回执'}
      </span>
    </p>
  );
}

function Head({ stat, model }: { stat?: ReactNode; model: Model | undefined }) {
  return (
    <header>
      <PanelTitle of="security">来源 IP 封禁</PanelTitle>
      {stat && <span className="source-access-stat header-stat">{stat}</span>}
      <span className="sp" />
      <Status node={model?.node} />
    </header>
  );
}

function PlaceLabel({ place }: { place: Place }) {
  return (
    <span className="source-access-place" title={SOURCE_LOCATION_TITLE}>
      {place.code && <RegionFlag code={place.code} />}
      <span>{place.label}</span>
    </span>
  );
}

function Address({ ip }: { ip: string }) {
  return (
    <span className="source-access-ip">
      <span className="source-access-address" tabIndex={0} title={ip}>
        <SourceAddress ip={ip} />
      </span>
      <CopyButton
        className="user-fcopy"
        text={ip}
        label={`复制 ${ip}`}
        successLabel="已复制"
        failureLabel="复制失败"
        iconOnly
      />
    </span>
  );
}

function UserName({ holder }: { holder: Holder }) {
  return (
    <button
      type="button"
      className="source-access-user"
      title={`${holder.tenant} / ${holder.user} · 到用户详情管理`}
      onClick={() => navigate('users', { p: 'user', id: holder.user })}
    >
      <GeneratedUserAvatar id={holder.user} variant="mini" />
      <span className="source-access-user-name">{holder.user}</span>
    </button>
  );
}

function Unban({
  ip,
  onUnban,
  disabled,
}: {
  ip: string;
  onUnban: (ip: string) => Promise<unknown>;
  disabled: boolean;
}) {
  const [busy, setBusy] = useState(false);
  return (
    <>
      <button
        type="button"
        className="source-ban-unban source-access-unban"
        disabled={busy || disabled}
        aria-label={`解除本机封禁 ${ip}`}
        onClick={async () => {
          setBusy(true);
          try {
            await onUnban(ip);
          } catch {
            setBusy(false);
          }
        }}
      >
        {busy ? '解除中…' : '解除'}
      </button>
    </>
  );
}

function More({
  hidden,
  open,
  onToggle,
  noun = '个地址',
}: {
  hidden: number;
  open: boolean;
  onToggle: () => void;
  noun?: string;
}) {
  if (hidden <= 0) return null;
  return (
    <button type="button" className="source-access-more" aria-expanded={open} onClick={onToggle}>
      {open ? '收起' : `显示其余 ${hidden} ${noun}`}
    </button>
  );
}

/**
 * 添加本机封禁。输入时下方一行写出对象和后果：已在名单中、正在这台机器上在线（谁在用，封禁后断开）、
 * 已在某些用户的名单里，或只写影响范围。红色按钮即确认，不再另弹对话框。
 */
function AddBan({
  model,
  onBan,
  disabled,
  onClose,
  presencePending,
  presenceError,
}: {
  model: Model;
  onBan: (ip: string) => Promise<unknown>;
  disabled: boolean;
  onClose: () => void;
  presencePending: boolean;
  presenceError: unknown;
}) {
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const input = useRef<HTMLInputElement>(null);
  const hintId = useId();
  const full = model.nodeIps.length >= 256;
  const ip = canonicalSourceIp(value);
  const listed = !!ip && model.nodeIps.includes(ip);
  const online = ip && !listed ? model.online.get(ip) : undefined;
  const holders = ip && !listed ? model.holders.get(ip) : undefined;
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!value.trim() || busy || disabled || listed || full) return;
    setBusy(true);
    setError(null);
    try {
      await onBan(ip ?? value.trim());
      onClose();
    } catch (failure) {
      setError(failure);
      // 保存期间输入框被禁用、焦点落到页面上；失败后交回输入框，便于改正或按 Esc 收起。
      requestAnimationFrame(() => input.current?.focus());
    } finally {
      setBusy(false);
    }
  };
  let hint: ReactNode = '对本机所有用户与端口转发生效';
  let tone = '';
  if (listed) hint = '已在本机名单中';
  else if (online) {
    const place = model.place(ip!);
    hint = (
      <>
        {place.code && <RegionFlag code={place.code} />}
        {place.label} · {joinNames(online)} 正在使用，封禁后断开
      </>
    );
    tone = ' gold';
  } else if (holders) hint = `已在 ${joinNames(holders)} 的名单中；加入本机后对所有用户生效`;
  return (
    <form
      className="source-access-add"
      aria-label="封禁来源 IP"
      onSubmit={submit}
      onKeyDown={event => {
        if (event.key === 'Escape' && !busy) {
          event.preventDefault();
          onClose();
        }
      }}
    >
      <span className="source-access-add-row">
        <input
          ref={input}
          className="f mono"
          value={value}
          placeholder="IPv4 / IPv6 地址"
          aria-label="来源 IP"
          aria-describedby={hintId}
          autoComplete="off"
          spellCheck={false}
          autoFocus
          maxLength={45}
          disabled={busy || disabled}
          onChange={event => {
            setValue(event.target.value);
            setError(null);
          }}
        />
        <button type="submit" className="btn danger" disabled={!value.trim() || busy || disabled || listed || full}>
          {busy ? '封禁中…' : '封禁'}
        </button>
        <button type="button" className="btn" disabled={busy || disabled} onClick={onClose}>
          取消
        </button>
      </span>
      <p id={hintId} className={`source-ban-hint source-access-hint${tone}`}>
        {hint}
      </p>
      {presencePending && <p className="source-access-lookup">正在读取本机在线来源…</p>}
      {!!presenceError && <p className="source-access-lookup">在线来源读取失败，暂时无法确认谁正在使用此地址。</p>}
      {!!error && <ErrorBox error={error} />}
    </form>
  );
}

function Legacy() {
  return <p className="source-access-legacy">用户名单需升级 Console 后显示</p>;
}

/** 本机与用户名单共用地址、归属与禁行标记；只有本机行提供解除操作。 */
function Row({ ip, model, end }: { ip: string; model: Model; end?: ReactNode }) {
  return (
    <li className="source-access-row">
      <span className="source-access-mark" aria-hidden="true" />
      <Address ip={ip} />
      <PlaceLabel place={model.place(ip)} />
      {end && <span className="source-access-end">{end}</span>}
    </li>
  );
}

function NodeRows({
  model,
  onUnban,
  disabled,
}: {
  model: Model;
  onUnban?: (ip: string) => Promise<unknown>;
  disabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const shown = open ? model.nodeIps : model.nodeIps.slice(0, ROW_LIMIT);
  return (
    <>
      {model.nodeIps.length > 0 && (
        <ul className="source-access-list" aria-label="本机封禁">
          {shown.map(ip => (
            <Row
              key={ip}
              ip={ip}
              model={model}
              end={onUnban && <Unban ip={ip} onUnban={onUnban} disabled={disabled} />}
            />
          ))}
        </ul>
      )}
      <More hidden={model.nodeIps.length - ROW_LIMIT} open={open} onToggle={() => setOpen(value => !value)} />
    </>
  );
}

/** 用户名单：每位用户一行名字（点开到用户详情），其下是该用户名单里的地址，只读。 */
function UserGroups({ users, model }: { users: UserList[]; model: Model }) {
  const [open, setOpen] = useState(false);
  const shown = open ? users : users.slice(0, 5);
  return (
    <>
      <ul className="source-access-users" aria-label="用户封禁">
        {shown.map(list => (
          <li key={list.key}>
            <span className="source-access-user-line">
              <UserName holder={list} />
              <span className="source-access-tenant">{list.tenant}</span>
              <span className="source-access-chev" aria-hidden="true">
                ›
              </span>
            </span>
            <ul className="source-access-list nested">
              {list.ips.map(ip => (
                <Row key={ip} ip={ip} model={model} />
              ))}
            </ul>
          </li>
        ))}
      </ul>
      <More hidden={users.length - 5} open={open} onToggle={() => setOpen(value => !value)} noun="位用户" />
    </>
  );
}

/** 机器配置中的 C 方案：本机名单常显，相关用户名单在卡底就地展开。 */
export function SourceAccessPanel({ nodeId, editable }: { nodeId: string; editable: boolean }) {
  const { query, save } = useSourceAccess({ nodeId });
  const [addOpen, setAddOpen] = useState(false);
  const [showUsers, setShowUsers] = useState(false);
  const addLink = useRef<HTMLButtonElement>(null);
  const usersId = useId();
  const presence = useQuery({
    queryKey: ['node-source-access-presence'],
    queryFn: fetchUserPresence,
    enabled: editable && addOpen,
    refetchInterval: 30_000,
  });
  const model = useMemo(
    () => query.data && modelOf(query.data, presence.data, nodeId),
    [query.data, presence.data, nodeId],
  );
  const users = model?.users ?? [];
  const full = (model?.nodeIps.length ?? 0) >= 256;
  return (
    <section className="panel config-panel source-access-panel" aria-label="来源 IP 封禁">
      <Head
        model={model}
        stat={
          model && (
            <>
              本机 <b>{model.nodeIps.length}</b>
            </>
          )
        }
      />
      {query.isPending && <FieldLoading label="读取封禁名单" />}
      {!!query.error && <ErrorBox error={query.error} />}
      {!!save.error && !addOpen && <ErrorBox error={save.error} />}
      {model && (
        <>
          <PhaseLine node={model.node} />
          <NodeRows
            model={model}
            disabled={save.isPending}
            onUnban={editable ? ip => save.mutateAsync(ips => ips.filter(value => value !== ip)) : undefined}
          />
          {model.nodeIps.length === 0 && <p className="source-access-empty">本机未封禁任何来源</p>}
          {editable &&
            (addOpen ? (
              <AddBan
                model={model}
                disabled={save.isPending}
                onBan={ip => save.mutateAsync(ips => (ips.includes(ip) ? ips : [...ips, ip]))}
                onClose={() => {
                  save.reset();
                  setAddOpen(false);
                  requestAnimationFrame(() => addLink.current?.focus());
                }}
                presencePending={presence.isFetching}
                presenceError={presence.error}
              />
            ) : (
              <button
                ref={addLink}
                type="button"
                className="source-access-add-link"
                disabled={full || save.isPending}
                onClick={() => {
                  save.reset();
                  setAddOpen(true);
                }}
              >
                <Icon of="plus" size={12} />
                {full ? '本机名单已满 256 个' : '封禁 IP'}
              </button>
            ))}
          {model.users === undefined && <Legacy />}
          {users.length > 0 && (
            <div className={`source-access-userbar${showUsers ? ' open' : ''}`}>
              <button
                type="button"
                className="source-access-userbar-toggle"
                aria-expanded={showUsers}
                aria-controls={usersId}
                onClick={() => setShowUsers(value => !value)}
              >
                <span className="source-access-faces" aria-hidden="true">
                  {users.slice(0, 3).map(list => (
                    <GeneratedUserAvatar key={list.key} id={list.user} variant="mini" />
                  ))}
                </span>
                <span className="source-access-userbar-text">
                  {users.length} 位用户另有 <b>{model.userEntries}</b> 个封禁
                </span>
                <span className="source-access-userbar-names">{users.map(list => list.user).join('、')}</span>
                <Icon of="chevronDown" size={12} className="source-access-userbar-chev" />
              </button>
              <div id={usersId} hidden={!showUsers}>
                {showUsers && <UserGroups users={users} model={model} />}
              </div>
            </div>
          )}
        </>
      )}
    </section>
  );
}
