// The four candidate 来源 IP 封禁 panels for 机器详情 → 配置. Mockup only; never imported by the app.
// Every option reads and saves through the production useSourceAccess hook, so adding and lifting a ban go
// through the same revisioned list as the real panel (against the mockup backend).
import { useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  fetchUserPresence,
  type NetworkOperator,
  type SourceAccessNode,
  type SourceAccessView,
  type UserPresenceList,
} from '../../src/api';
import { navigate } from '../../src/forge/route';
import { useSourceAccess } from '../../src/panes/source-access';
import { canonicalSourceIp } from '../../src/ui/source-address';
import { ErrorBox } from '../../src/ui/bits';
import { CopyButton } from '../../src/ui/copy-button';
import { Icon, PanelTitle } from '../../src/ui/icons';
import { FieldLoading } from '../../src/ui/loading';
import { RegionFlag } from '../../src/ui/region-flag';
import { SOURCE_LOCATION_TITLE, SourceAddress, sourceNetwork } from '../../src/ui/source-address';
import { GeneratedUserAvatar } from '../../src/ui/user-avatar';

export type Option = 'a' | 'b' | 'c' | 'd';
/** 打开页面时预先展开的部分，便于截图对照。 */
export type OpenTarget = 'none' | 'add' | 'users';

/* ══ 数据 ══ */

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

function useBans(nodeId: string) {
  const { query, save } = useSourceAccess({ nodeId });
  const presence = useQuery({ queryKey: ['mk-presence'], queryFn: fetchUserPresence, refetchInterval: 30_000 });
  const model = useMemo(
    () => (query.data ? modelOf(query.data, presence.data, nodeId) : undefined),
    [query.data, presence.data, nodeId],
  );
  return {
    query,
    model,
    ban: (ip: string) => save.mutateAsync(ips => (ips.includes(ip) ? ips : [...ips, ip])),
    unban: (ip: string) => save.mutateAsync(ips => ips.filter(value => value !== ip)),
  };
}

const isV6 = (ip: string) => ip.includes(':');
const joinNames = (holders: Holder[]) => holders.map(holder => holder.user).join('、');
const ROW_LIMIT = 8;

/* ══ 共用部件 ══ */

const PHASE: Record<SourceAccessNode['phase'], { tone: string; text: string }> = {
  applied: { tone: 'ok', text: '已生效' },
  pending: { tone: 'gold', text: '同步中' },
  unsupported: { tone: 'gold', text: '待升级' },
  error: { tone: 'err', text: '执行异常' },
};

/** 标题栏右端的执行情况。只有一台机器，不再点开逐台列出；等待原因放在悬停说明里。 */
function Status({ node }: { node: SourceAccessNode | undefined }) {
  if (!node) return null;
  const phase = PHASE[node.phase];
  return (
    <span className={`nsb-status ${phase.tone}`} role="status" title={node.error ?? undefined}>
      <span className="nsb-dot" aria-hidden="true" />
      {phase.text}
    </span>
  );
}

/** 异常与待升级意味着名单没有在这台机器上执行，原因写进正文一行；同步中只在标题栏。 */
function PhaseLine({ node }: { node: SourceAccessNode | undefined }) {
  if (!node || (node.phase !== 'error' && node.phase !== 'unsupported')) return null;
  return (
    <p className={`nsb-phase ${node.phase === 'error' ? 'err' : 'gold'}`}>
      <Icon of="warn" size={13} />
      <span>{node.phase === 'unsupported' ? '这台机器需升级 Agent 与 Xray 才会执行封禁' : node.error}</span>
    </p>
  );
}

function Head({ stat, model }: { stat?: ReactNode; model: Model | undefined }) {
  return (
    <header>
      <PanelTitle of="security">来源 IP 封禁</PanelTitle>
      {stat && <span className="nsb-stat header-stat">{stat}</span>}
      <span className="sp" />
      <Status node={model?.node} />
    </header>
  );
}

function PlaceLabel({ place }: { place: Place }) {
  return (
    <span className="nsb-place" title={SOURCE_LOCATION_TITLE}>
      {place.code && <RegionFlag code={place.code} />}
      <span>{place.label}</span>
    </span>
  );
}

function Address({ ip }: { ip: string }) {
  return (
    <span className="nsb-ip">
      <SourceAddress ip={ip} />
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

function UserName({ holder, tenant = false }: { holder: Holder; tenant?: boolean }) {
  return (
    <button
      type="button"
      className="nsb-user"
      title={`${holder.tenant} / ${holder.user} · 到用户详情管理`}
      onClick={() => navigate('users', { p: 'user', id: holder.user })}
    >
      <GeneratedUserAvatar id={holder.user} variant="mini" />
      <span className="nsb-user-name">{holder.user}</span>
      {tenant && <span className="nsb-tenant">{holder.tenant}</span>}
    </button>
  );
}

function Unban({ ip, onUnban, label = '解除' }: { ip: string; onUnban: (ip: string) => Promise<unknown>; label?: string }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  return (
    <>
      <button
        type="button"
        className="source-ban-unban nsb-unban"
        disabled={busy}
        aria-label={`解除本机封禁 ${ip}`}
        title={error instanceof Error ? error.message : undefined}
        onClick={async () => {
          setBusy(true);
          setError(null);
          try {
            await onUnban(ip);
          } catch (failure) {
            setError(failure);
            setBusy(false);
          }
        }}
      >
        {busy ? '解除中…' : label}
      </button>
      {!!error && <ErrorBox error={error} />}
    </>
  );
}

function More({ hidden, open, onToggle, noun = '个地址' }: { hidden: number; open: boolean; onToggle: () => void; noun?: string }) {
  if (hidden <= 0) return null;
  return (
    <button type="button" className="nsb-more" aria-expanded={open} onClick={onToggle}>
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
  initial,
  className = '',
  onClose,
}: {
  model: Model;
  onBan: (ip: string) => Promise<unknown>;
  initial: string | null;
  className?: string;
  /** 由外层决定何时展开（D 的网格入口）；不给时自带「封禁 IP」入口。 */
  onClose?: () => void;
}) {
  const [open, setOpen] = useState(onClose !== undefined || initial !== null);
  const [value, setValue] = useState(initial ?? '');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const input = useRef<HTMLInputElement>(null);
  const full = model.nodeIps.length >= 256;
  if (!open)
    return (
      <button type="button" className={`nsb-add-link ${className}`} disabled={full} onClick={() => setOpen(true)}>
        <Icon of="plus" size={12} />
        {full ? '本机名单已满 256 个' : '封禁 IP'}
      </button>
    );
  const ip = canonicalSourceIp(value);
  const listed = !!ip && model.nodeIps.includes(ip);
  const online = ip && !listed ? model.online.get(ip) : undefined;
  const holders = ip && !listed ? model.holders.get(ip) : undefined;
  const close = () => {
    setValue('');
    setError(null);
    if (onClose) onClose();
    else setOpen(false);
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!value.trim() || busy || listed) return;
    setBusy(true);
    setError(null);
    try {
      await onBan(ip ?? value.trim());
      close();
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
      className={`nsb-add ${className}`}
      aria-label="封禁来源 IP"
      onSubmit={submit}
      onKeyDown={event => {
        if (event.key === 'Escape' && !busy) {
          event.preventDefault();
          close();
        }
      }}
    >
      <span className="nsb-add-row">
        <input
          ref={input}
          className="f mono"
          value={value}
          placeholder="IPv4 / IPv6 地址"
          aria-label="来源 IP"
          autoComplete="off"
          spellCheck={false}
          autoFocus={initial === null}
          maxLength={45}
          disabled={busy}
          onChange={event => {
            setValue(event.target.value);
            setError(null);
          }}
        />
        <button type="submit" className="btn danger" disabled={!value.trim() || busy || listed}>
          {busy ? '封禁中…' : '封禁'}
        </button>
        <button type="button" className="btn" disabled={busy} onClick={close}>
          取消
        </button>
      </span>
      {value.trim() && <p className={`source-ban-hint nsb-hint${tone}`}>{hint}</p>}
      {!!error && <ErrorBox error={error} />}
    </form>
  );
}

function Legacy() {
  return <p className="nsb-legacy">用户名单需升级 Console 后显示</p>;
}

/* ══ 行 ══ */

/** 一条地址：禁行标记 · 地址 · 归属，行尾放操作或归属用户。 */
function Row({ ip, model, end, sub }: { ip: string; model: Model; end?: ReactNode; sub?: ReactNode }) {
  return (
    <li className={`nsb-row${isV6(ip) ? ' v6' : ''}`}>
      <span className="nsb-mark" aria-hidden="true" />
      <Address ip={ip} />
      <PlaceLabel place={model.place(ip)} />
      {sub && <span className="nsb-sub">{sub}</span>}
      {end && <span className="nsb-end">{end}</span>}
    </li>
  );
}

function NodeRows({ model, onUnban }: { model: Model; onUnban: (ip: string) => Promise<unknown> }) {
  const [open, setOpen] = useState(false);
  const shown = open ? model.nodeIps : model.nodeIps.slice(0, ROW_LIMIT);
  return (
    <>
      {model.nodeIps.length > 0 && (
        <ul className="nsb-list" aria-label="本机封禁">
          {shown.map(ip => (
            <Row key={ip} ip={ip} model={model} end={<Unban ip={ip} onUnban={onUnban} />} />
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
      <ul className="nsb-users" aria-label="用户封禁">
        {shown.map(list => (
          <li key={list.key}>
            <span className="nsb-user-line">
              <UserName holder={list} />
              <span className="nsb-tenant">{list.tenant}</span>
              <span className="nsb-chev" aria-hidden="true">›</span>
            </span>
            <ul className="nsb-list nested">
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

/* ══ A 分组清单 ══ */

function OptionA({ model, ban, unban, open }: OptionProps) {
  return (
    <>
      <Head model={model} stat={<>本机 <b>{model.nodeIps.length}</b> · 用户 <b>{model.userEntries ?? '—'}</b></>} />
      <PhaseLine node={model.node} />
      <section className="nsb-group" aria-label="本机名单">
        <h5 className="nsb-group-head">
          本机<small>所有用户与端口转发</small>
        </h5>
        <NodeRows model={model} onUnban={unban} />
        <AddBan model={model} onBan={ban} initial={open === 'add' ? '203.0.113.24' : null} />
      </section>
      {model.users === undefined ? (
        <section className="nsb-group" aria-label="用户名单">
          <h5 className="nsb-group-head">用户</h5>
          <Legacy />
        </section>
      ) : (
        model.users.length > 0 && (
          <section className="nsb-group" aria-label="用户名单">
            <h5 className="nsb-group-head">
              用户<small>仅限对应用户</small>
            </h5>
            <UserGroups users={model.users} model={model} />
          </section>
        )
      )}
    </>
  );
}

/* ══ B 按地址合并 ══ */

interface Merged {
  ip: string;
  node: boolean;
  users: Holder[];
}
function mergedOf(model: Model): Merged[] {
  const rows: Merged[] = model.nodeIps.map(ip => ({ ip, node: true, users: model.holders.get(ip) ?? [] }));
  for (const [ip, users] of model.holders) if (!model.nodeIps.includes(ip)) rows.push({ ip, node: false, users });
  return rows;
}

/** 这个地址对谁生效：本机（所有用户）和各用户的名单，命中任意一份即拒绝。 */
function Scope({ row }: { row: Merged }) {
  return (
    <span className="nsb-scope">
      {row.node && (
        <span className="nsb-scope-node">
          <Icon of="servers" size={12} />
          本机
        </span>
      )}
      {row.users.map(holder => (
        <UserName key={`${holder.tenant}/${holder.user}`} holder={holder} />
      ))}
    </span>
  );
}

function OptionB({ model, ban, unban, open }: OptionProps) {
  const rows = mergedOf(model);
  const [expanded, setExpanded] = useState(false);
  const shown = expanded ? rows : rows.slice(0, ROW_LIMIT);
  return (
    <>
      <Head model={model} stat={<><b>{rows.length}</b> 个地址</>} />
      <PhaseLine node={model.node} />
      {rows.length > 0 && (
        <ul className="nsb-list nsb-merged" aria-label="已封禁的来源">
          {shown.map(row => (
            <Row
              key={row.ip}
              ip={row.ip}
              model={model}
              sub={<Scope row={row} />}
              end={row.node ? <Unban ip={row.ip} onUnban={unban} /> : undefined}
            />
          ))}
        </ul>
      )}
      <More hidden={rows.length - ROW_LIMIT} open={expanded} onToggle={() => setExpanded(value => !value)} />
      <AddBan model={model} onBan={ban} initial={open === 'add' ? '203.0.113.24' : null} />
      {model.users === undefined && <Legacy />}
    </>
  );
}

/* ══ C 本机为主 ══ */

function OptionC({ model, ban, unban, open }: OptionProps) {
  const [showUsers, setShowUsers] = useState(open === 'users');
  const users = model.users ?? [];
  return (
    <>
      <Head model={model} stat={<>本机 <b>{model.nodeIps.length}</b></>} />
      <PhaseLine node={model.node} />
      <NodeRows model={model} onUnban={unban} />
      {model.nodeIps.length === 0 && <p className="nsb-empty">本机未封禁任何来源</p>}
      <AddBan model={model} onBan={ban} initial={open === 'add' ? '203.0.113.24' : null} />
      {model.users === undefined && <Legacy />}
      {users.length > 0 && (
        <div className={`nsb-userbar${showUsers ? ' open' : ''}`}>
          <button
            type="button"
            className="nsb-userbar-toggle"
            aria-expanded={showUsers}
            onClick={() => setShowUsers(value => !value)}
          >
            <span className="nsb-faces" aria-hidden="true">
              {users.slice(0, 3).map(list => (
                <GeneratedUserAvatar key={list.key} id={list.user} variant="mini" />
              ))}
            </span>
            <span className="nsb-userbar-text">
              {users.length} 位用户另有 <b>{model.userEntries}</b> 个封禁
            </span>
            <span className="nsb-userbar-names">{users.map(list => list.user).join('、')}</span>
            <Icon of="chevronDown" size={12} className="nsb-userbar-chev" />
          </button>
          {showUsers && <UserGroups users={users} model={model} />}
        </div>
      )}
    </>
  );
}

/* ══ D 紧凑网格 ══ */

function Cell({ ip, model, onUnban }: { ip: string; model: Model; onUnban?: (ip: string) => Promise<unknown> }) {
  const place = model.place(ip);
  const [busy, setBusy] = useState(false);
  return (
    <li className={`nsb-cell${isV6(ip) ? ' v6' : ''}${busy ? ' busy' : ''}`} title={`${ip} · ${place.label}`}>
      {place.code ? <RegionFlag code={place.code} /> : <span className="nsb-cell-noflag" aria-hidden="true" />}
      <SourceAddress ip={ip} />
      {onUnban && (
        <button
          type="button"
          className="nsb-cell-x"
          aria-label={`解除本机封禁 ${ip}`}
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            try {
              await onUnban(ip);
            } catch {
              setBusy(false);
            }
          }}
        >
          <Icon of="close" size={10} />
        </button>
      )}
    </li>
  );
}

function OptionD({ model, ban, unban, open }: OptionProps) {
  const [adding, setAdding] = useState(open === 'add');
  return (
    <>
      <Head model={model} stat={<>本机 <b>{model.nodeIps.length}</b> · 用户 <b>{model.userEntries ?? '—'}</b></>} />
      <PhaseLine node={model.node} />
      <div className="nsb-grid-block">
        <span className="nsb-grid-who nsb-scope-node">
          <Icon of="servers" size={12} />
          本机
        </span>
        <ul className="nsb-grid" aria-label="本机封禁">
          {model.nodeIps.map(ip => (
            <Cell key={ip} ip={ip} model={model} onUnban={unban} />
          ))}
          {!adding && (
            <li className="nsb-cell add">
              <button type="button" className="nsb-add-link" onClick={() => setAdding(true)}>
                <Icon of="plus" size={12} />
                封禁 IP
              </button>
            </li>
          )}
        </ul>
        {adding && (
          <AddBan
            model={model}
            onBan={ban}
            initial={open === 'add' ? '203.0.113.24' : null}
            className="inline"
            onClose={() => setAdding(false)}
          />
        )}
      </div>
      {model.users === undefined ? (
        <div className="nsb-grid-block user">
          <span className="nsb-grid-who">用户</span>
          <Legacy />
        </div>
      ) : (
        model.users.map(list => (
          <div key={list.key} className="nsb-grid-block user">
            <span className="nsb-grid-who">
              <UserName holder={list} />
            </span>
            <ul className="nsb-grid" aria-label={`${list.user} 的封禁`}>
              {list.ips.map(ip => (
                <Cell key={ip} ip={ip} model={model} />
              ))}
            </ul>
          </div>
        ))
      )}
    </>
  );
}

/* ══ 外壳 ══ */

interface OptionProps {
  model: Model;
  ban: (ip: string) => Promise<unknown>;
  unban: (ip: string) => Promise<unknown>;
  open: OpenTarget;
}

const OPTION_VIEWS: Record<Option, (props: OptionProps) => ReactNode> = {
  a: OptionA,
  b: OptionB,
  c: OptionC,
  d: OptionD,
};

export function BanPanel({ option, nodeId, open }: { option: Option; nodeId: string; open: OpenTarget }) {
  const { query, model, ban, unban } = useBans(nodeId);
  const View = OPTION_VIEWS[option];
  return (
    <section className={`panel config-panel nsb nsb-${option}`} aria-label="来源 IP 封禁">
      {!model && <Head model={undefined} />}
      {query.isPending && <FieldLoading label="读取封禁名单" />}
      {!!query.error && <ErrorBox error={query.error} />}
      {model && <View model={model} ban={ban} unban={unban} open={open} />}
    </section>
  );
}
