import { useId, useState, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchArtifactContent,
  fetchClashSubscription,
  fetchMyArtifact,
  fetchMyClashSubscription,
  fetchRevisions,
  issueClashHaitunSubscription,
  regenerateClashHaitunSubscription,
  type ArtifactFamily,
  type ArtifactProtocol,
  type ClashSubscriptionInfo,
} from '../api';
import { ErrorBox, Loading, SegmentedControl } from '../ui/bits';
import { CopyButton, copyStateIcon } from '../ui/copy-button';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { bytes } from '../ui/format';
import { Icon, type IconName } from '../ui/icons';
import { RegionFlag } from '../ui/region-flag';
import { useNarrow } from '../ui/viewport';

export type SubscriptionKind = 'uri' | 'clash';

type FamilyPick = 'both' | ArtifactFamily;
type ProtocolPick = 'both' | ArtifactProtocol;

const FAMILY_PICKS: { value: FamilyPick; label: string }[] = [
  { value: 'both', label: '双栈' },
  { value: 'v4', label: 'IPv4' },
  { value: 'v6', label: 'IPv6' },
];

const PROTOCOL_PICKS: { value: ProtocolPick; label: string }[] = [
  { value: 'both', label: '全部' },
  { value: 'vless', label: 'VLESS' },
  { value: 'anytls', label: 'AnyTLS' },
  { value: 'hysteria2', label: 'Hysteria 2' },
];

const TABS: { value: SubscriptionKind; label: string; icon: IconName }[] = [
  { value: 'clash', label: '订阅', icon: 'subscription' },
  { value: 'uri', label: '节点', icon: 'client' },
];

/* 两个页签各自渲染正文；正文元素是当前页签对应的 tabpanel。 */
interface TabPanelProps {
  role: 'tabpanel';
  id: string;
  'aria-labelledby': string;
}

/* 订阅与节点共用一个弹窗：标题栏切换「订阅 / 节点」，协议与地址族筛选两个页签共用。
 * 两边的内容都由服务端按筛选条件生成：订阅是带 family / protocol 参数的地址，节点是同一条件下的
 * uri.txt。浏览器只把 uri.txt 解析成列表用于展示，不自行过滤条目。
 * 窄屏（与 useNarrow 同一断点）弹窗贴底成为面板，筛选换成一行两个下拉。 */
export function SubscriptionViewer({
  tenant,
  user,
  kind,
  selfService = false,
  identity,
  onClose,
}: {
  tenant: string;
  user: string;
  kind: SubscriptionKind;
  selfService?: boolean;
  identity?: ReactNode;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<SubscriptionKind>(kind);
  const [protocol, setProtocol] = useState<ProtocolPick>('both');
  const [family, setFamily] = useState<FamilyPick>('both');
  // 默认列出自签证书节点（带 insecure 标记），节点页签的说明行里可以改为不列出。切到 VLESS 不收回：
  // VLESS 自签入口没有可互通的链接写法，服务端无论是否允许 insecure 都会跳过并给出说明。
  const [allowInsecure, setAllowInsecure] = useState(true);
  const narrow = useNarrow();
  const ids = useId();
  const tabId = (value: SubscriptionKind) => `${ids}-tab-${value}`;
  const panel: TabPanelProps = { role: 'tabpanel', id: `${ids}-panel`, 'aria-labelledby': tabId(tab) };

  // 与机器详情页签相同：左右方向键、Home / End 在两个页签之间移动并切换。
  const moveTab = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    const index = TABS.findIndex(item => item.value === tab);
    let next: number | null = null;
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (index + 1) % TABS.length;
    if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') next = (index - 1 + TABS.length) % TABS.length;
    if (event.key === 'Home') next = 0;
    if (event.key === 'End') next = TABS.length - 1;
    if (next === null) return;
    event.preventDefault();
    const value = TABS[next].value;
    setTab(value);
    window.requestAnimationFrame(() => document.getElementById(tabId(value))?.focus());
  };

  return (
    <DialogLayer label={`${user} 的订阅与节点`} onClose={onClose}>
      <section className="dialog-surface sub-dialog">
        <header className="sub-dialog-head">
          {identity}
          <b className="sub-dialog-user">{user}</b>
          <div className="nd-tabs sub-dialog-tabs" role="tablist" aria-label="订阅与节点">
            <div className="nd-tabs-seg">
              {TABS.map(item => (
                <button
                  key={item.value}
                  type="button"
                  id={tabId(item.value)}
                  role="tab"
                  aria-selected={tab === item.value}
                  aria-controls={panel.id}
                  tabIndex={tab === item.value ? 0 : -1}
                  onKeyDown={moveTab}
                  onClick={() => setTab(item.value)}
                >
                  <Icon of={item.icon} size={14} className="nd-tab-ic" />
                  {item.label}
                </button>
              ))}
            </div>
          </div>
          <DialogClose className="sub-dialog-close" title="关闭" aria-label="关闭">
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="sub-dialog-filter">
          {narrow ? (
            <>
              <FilterPick
                icon="protocol"
                label="协议"
                value={protocol}
                options={PROTOCOL_PICKS}
                onChange={setProtocol}
              />
              <FilterPick icon="family" label="地址族" value={family} options={FAMILY_PICKS} onChange={setFamily} />
            </>
          ) : (
            <>
              <span className="sub-dialog-field">
                <span className="sub-dialog-label">
                  <Icon of="protocol" size={13} />
                  协议
                </span>
                <SegmentedControl
                  ariaLabel="订阅协议"
                  value={protocol}
                  options={PROTOCOL_PICKS}
                  onChange={setProtocol}
                />
              </span>
              <span className="sub-dialog-field">
                <span className="sub-dialog-label">
                  <Icon of="family" size={13} />
                  地址族
                </span>
                <SegmentedControl ariaLabel="地址族" value={family} options={FAMILY_PICKS} onChange={setFamily} />
              </span>
            </>
          )}
        </div>
        {tab === 'clash' ? (
          <ClashSubscription
            tenant={tenant}
            user={user}
            selfService={selfService}
            protocol={protocol}
            family={family}
            panel={panel}
          />
        ) : (
          <NodeLinks
            tenant={tenant}
            user={user}
            selfService={selfService}
            protocol={protocol}
            family={family}
            allowInsecure={allowInsecure}
            onAllowInsecure={setAllowInsecure}
            onUseSubscription={() => setTab('clash')}
            panel={panel}
          />
        )}
      </section>
    </DialogLayer>
  );
}

/* 窄屏筛选：显示层是图标、名称、当前值与箭头，透明的原生 select 覆盖在上面，点开即系统选择器。
 * select 自身用 16px：iOS 聚焦字号小于 16px 的表单控件会放大页面。 */
function FilterPick<T extends string>({
  icon,
  label,
  value,
  options,
  onChange,
}: {
  icon: IconName;
  label: string;
  value: T;
  options: readonly { value: T; label: string }[];
  onChange: (value: T) => void;
}) {
  return (
    <label className="sub-pick">
      <span className="sub-pick-label">
        <Icon of={icon} size={14} />
        {label}
      </span>
      <b className="sub-pick-value">{options.find(option => option.value === value)?.label}</b>
      <Icon of="chevronDown" size={14} className="sub-pick-chevron" />
      <select
        aria-label={label}
        value={value}
        onChange={event => {
          const next = options.find(option => option.value === event.target.value);
          if (next) onChange(next.value);
        }}
      >
        {options.map(option => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
    </label>
  );
}

/* ── 订阅 ──
 * 一个订阅一块：左侧图标列，标题与说明在上，读数在右；地址框与说明对齐到标题文字。 */

function ClashSubscription({
  tenant,
  user,
  selfService,
  protocol,
  family,
  panel,
}: {
  tenant: string;
  user: string;
  selfService: boolean;
  protocol: ProtocolPick;
  family: FamilyPick;
  panel: TabPanelProps;
}) {
  const qc = useQueryClient();
  const queryKey = ['clash-subscription', tenant, user, selfService ? 'self' : 'admin'] as const;
  // 地址本身是访问凭据：只在打开「订阅」页签时读取，不随用户列表下发。
  const subscription = useQuery({
    queryKey,
    queryFn: ({ signal }) =>
      selfService ? fetchMyClashSubscription(signal) : fetchClashSubscription(tenant, user, signal),
    staleTime: 0,
  });
  const updateHaitun = async (haitun: ClashSubscriptionInfo['haitun']) => {
    // A read started before rotation must not replace the new URL with the invalid old token.
    await qc.cancelQueries({ queryKey });
    qc.setQueryData<ClashSubscriptionInfo>(queryKey, current => (current ? { ...current, haitun } : current));
  };
  const issueHaitun = useMutation({
    mutationFn: () => issueClashHaitunSubscription(tenant, user),
    onSuccess: updateHaitun,
  });
  const regenerateHaitun = useMutation({
    mutationFn: () => regenerateClashHaitunSubscription(tenant, user),
    onSuccess: updateHaitun,
  });

  if (subscription.isPending || subscription.error) {
    return (
      <div className="sub-dialog-body" {...panel}>
        {subscription.error ? <ErrorBox error={subscription.error} /> : <Loading variant="code" />}
      </div>
    );
  }

  const value = subscription.data;
  const standardUrl = withSubscriptionProtocol(value.urls[family], protocol);
  const haitun = selfService ? null : value.haitun;
  const haitunUrls = haitun?.status === 'active' ? haitun.urls : null;
  const haitunUrl = haitunUrls ? withSubscriptionProtocol(haitunUrls[family], protocol) : '';
  const actionError = issueHaitun.error ?? regenerateHaitun.error;
  const haitunPending = issueHaitun.isPending || regenerateHaitun.isPending;

  return (
    <>
      <div className="sub-dialog-body" {...panel}>
        <section className="sub-item" aria-label="Clash 订阅">
          <Icon of="subscription" size={16} className="sub-item-icon" />
          <div className="sub-item-head">
            <div className="sub-item-title">
              <div className="sub-item-name">
                <b>Clash 订阅</b>
              </div>
              <span className="sub-item-sub">模板 {value.template} · 适用 Mihomo / Clash Meta</span>
            </div>
            <dl className="sub-readings">
              <div>
                <dt>剩余流量</dt>
                <dd className={value.remaining_bytes === 0 ? 'bad' : undefined}>
                  {value.remaining_bytes === null ? '不限量' : bytes(value.remaining_bytes)}
                </dd>
              </div>
              <div>
                <dt>重置时间</dt>
                <dd title="按 UTC+8 计">{formatResetAt(value.reset_at)}</dd>
              </div>
            </dl>
          </div>
          <div className="sub-item-body">
            {/* key 随地址变化：切换协议或地址族后重新遮罩。 */}
            <SubscriptionUrl key={standardUrl} url={standardUrl} name="Clash 订阅地址" />
          </div>
        </section>
        {haitun && (
          <section className="sub-item" aria-label="koipy 测速订阅">
            <Icon of="observe" size={16} className="sub-item-icon" />
            <div className="sub-item-head">
              <div className="sub-item-title">
                <div className="sub-item-name">
                  <b>koipy 测速订阅</b>
                  <span className={`sub-live${haitunUrls ? ' ok' : haitun.status === 'revoked' ? ' warn' : ''}`}>
                    {haitunUrls ? '可用' : haitun.status === 'revoked' ? '旧地址已停用' : '尚未生成'}
                  </span>
                </div>
                <span className="sub-item-sub">仅管理员</span>
              </div>
              {haitunUrls ? (
                <button
                  type="button"
                  className="btn sub-item-action"
                  aria-label="重新生成 koipy 测速地址"
                  disabled={haitunPending}
                  onClick={() => {
                    if (
                      !window.confirm(
                        '重新生成后，旧测速地址将无法继续拉取，但已下载的 UUID 和节点配置仍然有效。此操作不更换 UUID，也不发布授权。确定重新生成？',
                      )
                    )
                      return;
                    issueHaitun.reset();
                    regenerateHaitun.mutate();
                  }}
                >
                  {regenerateHaitun.isPending ? '生成中…' : '重新生成'}
                </button>
              ) : (
                <button
                  type="button"
                  className="btn sub-item-action"
                  aria-label={haitun.status === 'revoked' ? '重新生成 koipy 测速地址' : '生成 koipy 测速地址'}
                  disabled={haitunPending}
                  onClick={() => issueHaitun.mutate()}
                >
                  {issueHaitun.isPending ? '生成中…' : haitun.status === 'revoked' ? '重新生成' : '生成'}
                </button>
              )}
            </div>
            <div className="sub-item-body">
              {haitunUrls && <SubscriptionUrl key={haitunUrl} url={haitunUrl} name="koipy 测速地址" />}
              <p className="sub-item-note">
                风险提示：测速订阅包含用户 UUID
                等真实节点凭据，持有地址的人无需登录即可获取并使用节点，请仅交给可信测速服务。
                重新生成仅更换测速地址，不影响普通订阅，也不会使已下载的节点配置失效。
                若凭据泄露，需同时重新生成测速地址、更换用户 UUID 并发布授权，待节点生效后旧凭据才失效。
              </p>
              {actionError && <ErrorBox error={actionError} />}
            </div>
          </section>
        )}
      </div>
      <footer className="sub-dialog-foot">
        <span>每次拉取按当前授权实时生成，不缓存</span>
        <span>节点凭据变更需发布授权并等待节点生效</span>
      </footer>
    </>
  );
}

/* 地址框：遮住的 Token 降一档墨色，主机与路径保持可读；「显示」与「复制」收在框内右侧。
 * 窄屏时地址整段换行显示，两个按钮落到框底各占一半。 */
function SubscriptionUrl({ url, name }: { url: string; name: string }) {
  const [revealed, setRevealed] = useState(false);
  const masked = revealed ? null : maskSubscriptionUrl(url);
  return (
    <div className="sub-url">
      <code title={revealed ? url : undefined}>
        {masked ? (
          <>
            {masked.head}
            <span className="sub-url-mask">{TOKEN_MASK}</span>
            {masked.tail}
          </>
        ) : (
          url
        )}
      </code>
      <button
        type="button"
        className="sub-url-reveal"
        aria-label={`${revealed ? '隐藏' : '显示'} ${name}`}
        onClick={() => setRevealed(current => !current)}
      >
        {revealed ? '隐藏' : '显示'}
      </button>
      <CopyButton className="sub-url-copy" text={url}>
        {(state, label) => (
          <>
            <Icon of={copyStateIcon(state)} size={13} />
            {label}
          </>
        )}
      </CopyButton>
    </div>
  );
}

/* ── 节点 ── */

const NODE_PROTOCOL_LABEL: Record<NodeProtocol, string> = {
  vless: 'VLESS',
  'vless-encryption': 'VLESS',
  anytls: 'AnyTLS',
  hysteria2: 'Hysteria 2',
  other: '其他',
};

const NODE_FAMILY_LABEL: Record<NodeFamily, string> = { ipv4: 'IPv4', ipv6: 'IPv6' };

function NodeLinks({
  tenant,
  user,
  selfService,
  protocol,
  family,
  allowInsecure,
  onAllowInsecure,
  onUseSubscription,
  panel,
}: {
  tenant: string;
  user: string;
  selfService: boolean;
  protocol: ProtocolPick;
  family: FamilyPick;
  allowInsecure: boolean;
  onAllowInsecure: (allow: boolean) => void;
  onUseSubscription: () => void;
  panel: TabPanelProps;
}) {
  const [view, setView] = useState<'list' | 'raw'>('list');
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions(), refetchInterval: 10_000 });
  const revision = revisions.data?.current_revision;
  const content = useQuery({
    queryKey: ['artifact', revision, 'user', `${tenant}:${user}`, 'uri', family, protocol, allowInsecure],
    queryFn: () =>
      selfService
        ? fetchMyArtifact(
            family === 'both' ? undefined : family,
            protocol === 'both' ? undefined : protocol,
            allowInsecure,
          )
        : fetchArtifactContent(
            'user',
            `${tenant}:${user}`,
            'uri',
            undefined,
            family === 'both' ? undefined : family,
            protocol === 'both' ? undefined : protocol,
            true,
            allowInsecure,
          ),
    enabled: !!revision,
  });

  if (!revision || content.isPending || content.error) {
    return (
      <div className="sub-dialog-body" {...panel}>
        {content.error ? <ErrorBox error={content.error} /> : <Loading variant="code" />}
      </div>
    );
  }

  const text = content.data.content ?? '';
  const listing = parseNodeListing(text);
  const groups = groupNodeLinks(listing.links);
  const slots: NodeFamily[] = family === 'both' ? ['ipv4', 'ipv6'] : [family === 'v4' ? 'ipv4' : 'ipv6'];
  const insecureCount = allowInsecure ? listing.links.filter(link => link.insecure).length : 0;
  const lines = text ? text.replace(/\n$/, '').split('\n') : [];

  return (
    <>
      <div className="sub-dialog-body" {...panel}>
        <div className="node-toolbar">
          <span className="node-count">
            <b>{listing.links.length}</b> 个节点
          </span>
          <span className="sp" />
          <SegmentedControl
            className="node-view"
            ariaLabel="显示方式"
            value={view}
            onChange={setView}
            options={[
              { value: 'list', label: '列表' },
              { value: 'raw', label: '原文' },
            ]}
          />
          <CopyButton
            className="btn node-copy-all"
            text={listing.links.map(link => link.uri).join('\n')}
            label="复制全部"
          >
            {(state, label) => (
              <>
                <Icon of={copyStateIcon(state)} size={13} />
                {label}
              </>
            )}
          </CopyButton>
        </div>
        <NodeNotes
          notes={listing.notes}
          insecureCount={insecureCount}
          onAllowInsecure={onAllowInsecure}
          onUseSubscription={onUseSubscription}
        />
        {view === 'raw' ? (
          <div className="cfg-code node-raw">
            <div className="lnum">{lines.map((_, index) => index + 1).join('\n')}</div>
            <pre className="cd">{lines.join('\n')}</pre>
          </div>
        ) : groups.length === 0 ? (
          <div className="node-empty">没有符合当前协议与地址族的节点</div>
        ) : (
          <NodeMatrix groups={groups} slots={slots} />
        )}
      </div>
      <footer className="sub-dialog-foot">
        <span>修订 {content.data.revision}</span>
        <span>更换 UUID 后全部节点链接失效</span>
      </footer>
    </>
  );
}

// 服务端的说明写作「以下自签证书地址默认隐藏；……：名称、名称」，那是 uri.txt 自身的默认。
// 这个弹窗默认列出自签节点，用户改为不列出后，原句的「默认隐藏」与弹窗的默认相反，改写为当前状态。
const HIDDEN_SELF_SIGNED = /^以下自签证书地址默认隐藏[^：]*：(.+)$/;

/* 列表之前的说明：已列出的自签节点、未列出的条目及原因，各带一个就地操作。 */
function NodeNotes({
  notes,
  insecureCount,
  onAllowInsecure,
  onUseSubscription,
}: {
  notes: string[];
  insecureCount: number;
  onAllowInsecure: (allow: boolean) => void;
  onUseSubscription: () => void;
}) {
  if (notes.length === 0 && insecureCount === 0) return null;
  return (
    <div className="node-notes">
      {insecureCount > 0 && (
        <p>
          <Icon of="info" size={13} className="node-note-icon" />
          <span className="node-note-text">已列出 {insecureCount} 个自签证书节点（标 insecure），客户端不验证证书</span>
          <button type="button" className="sub-text-action" onClick={() => onAllowInsecure(false)}>
            不再列出
          </button>
        </p>
      )}
      {notes.map(note => {
        const hidden = HIDDEN_SELF_SIGNED.exec(note);
        return (
          <p key={note}>
            <Icon of="info" size={13} className="node-note-icon" />
            <span className="node-note-text">
              {hidden ? `未列出 ${hidden[1].split('、').length} 个自签证书节点：${hidden[1]}` : note}
            </span>
            {hidden ? (
              <button type="button" className="sub-text-action" onClick={() => onAllowInsecure(true)}>
                允许 insecure
              </button>
            ) : (
              note.includes('Clash') && (
                <button type="button" className="sub-text-action" onClick={onUseSubscription}>
                  改用订阅
                </button>
              )
            )}
          </p>
        );
      })}
    </div>
  );
}

/* 一条链一组；一种协议一行，IPv4 / IPv6 各一列，地址本身就是复制按钮，缺少的地址族画「—」。
 * 只选一个地址族时只剩一列。 */
function NodeMatrix({ groups, slots }: { groups: NodeGroup[]; slots: NodeFamily[] }) {
  return (
    <div className={`node-matrix${slots.length === 1 ? ' single' : ''}`}>
      <div className="node-matrix-head" aria-hidden="true">
        <span />
        {slots.map(slot => (
          <span key={slot}>{NODE_FAMILY_LABEL[slot]}</span>
        ))}
      </div>
      {groups.map(group => (
        <section className="node-group" key={group.key} aria-label={group.name}>
          <div className="node-group-head">
            <RegionFlag code={group.region} />
            <b>{group.name}</b>
          </div>
          {group.rows.map(row => (
            <div className="node-row" key={row.key}>
              <span className="node-protocol">
                <b>{NODE_PROTOCOL_LABEL[row.protocol]}</b>
                <span>{row.stack}</span>
                {row.insecure && <em>insecure</em>}
              </span>
              {slots.map(slot => (
                <NodeAddress key={slot} link={row.links[slot]} family={slot} />
              ))}
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}

function NodeAddress({ link, family }: { link: NodeLink | undefined; family: NodeFamily }) {
  if (!link) {
    return (
      <span className="node-copy-gap" aria-hidden="true">
        —
      </span>
    );
  }
  const familyLabel = NODE_FAMILY_LABEL[family];
  // 主机与端口分开排：长 IPv6 只截主机部分，端口始终可见。
  const [, host, port = ''] = /^(.*?)(:\d+)?$/.exec(link.endpoint) ?? [link.endpoint, link.endpoint];
  return (
    <CopyButton
      className="node-copy"
      text={link.uri}
      aria-label={`复制 ${link.name}（${familyLabel}）`}
      title={`${link.endpoint} · 复制分享链接`}
    >
      {state => (
        <>
          <i className="node-copy-family">{familyLabel}</i>
          <span className="node-copy-endpoint">
            <span className="node-copy-host">{host}</span>
            <span className="node-copy-port">{port}</span>
          </span>
          <Icon of={copyStateIcon(state)} size={13} className="node-copy-icon" />
        </>
      )}
    </CopyButton>
  );
}

/* ── 节点链接解析 ──
 * 输入是服务端生成的 uri.txt：每行一条分享链接，# 开头的行是说明，空行分隔说明段落。
 * 名称的写法与 core/physical/user.rs 一致：可选的地区旗（两个区域指示符）+ 链名 + 协议后缀
 * （「 | VLESS Encryption」「 | QUIC」「 | AnyTLS」）+ 地址族后缀（「 | v6」）。 */

export type NodeProtocol = 'vless' | 'vless-encryption' | 'anytls' | 'hysteria2' | 'other';
export type NodeFamily = 'ipv4' | 'ipv6';

export interface NodeLink {
  uri: string;
  /** 客户端看到的名称，去掉地区旗。 */
  name: string;
  /** 去掉协议与地址族后缀的链名，用于分组。 */
  base: string;
  region: string | null;
  protocol: NodeProtocol;
  family: NodeFamily;
  endpoint: string;
  stack: string;
  insecure: boolean;
}

export interface NodeRow {
  key: string;
  protocol: NodeProtocol;
  stack: string;
  endpoint: string;
  insecure: boolean;
  links: Partial<Record<NodeFamily, NodeLink>>;
}

export interface NodeGroup {
  key: string;
  name: string;
  region: string | null;
  rows: NodeRow[];
}

const REGIONAL_INDICATOR_A = 0x1f1e6;
const FAMILY_SUFFIX = ' | v6';
const WIRE_SUFFIXES = [' | VLESS Encryption', ' | QUIC', ' | AnyTLS'];

// 浏览器不一定装有彩色 emoji 字体，地区旗拆成代码后交给 RegionFlag 的雪碧图显示。
function splitRegion(name: string): { region: string | null; label: string } {
  const points = [...name];
  const letters = points.slice(0, 2).map(point => (point.codePointAt(0) ?? 0) - REGIONAL_INDICATOR_A);
  if (letters.length === 2 && letters.every(letter => letter >= 0 && letter < 26)) {
    return { region: String.fromCharCode(...letters.map(letter => 65 + letter)), label: points.slice(2).join('') };
  }
  return { region: null, label: name };
}

const stripRegions = (text: string) => text.replace(/[\u{1F1E6}-\u{1F1FF}]{2}/gu, '');

function baseName(label: string) {
  let base = label.endsWith(FAMILY_SUFFIX) ? label.slice(0, -FAMILY_SUFFIX.length) : label;
  const wire = WIRE_SUFFIXES.find(suffix => base.endsWith(suffix));
  if (wire) base = base.slice(0, -wire.length);
  return base;
}

function parseNodeLink(line: string): NodeLink {
  const hashAt = line.indexOf('#');
  let fragment = hashAt >= 0 ? line.slice(hashAt + 1) : '';
  try {
    fragment = decodeURIComponent(fragment);
  } catch {
    // 名称不是合法的百分号编码时按原文显示。
  }
  const { region, label } = splitRegion(fragment);
  const scheme = line.slice(0, Math.max(0, line.indexOf('://'))).toLowerCase();
  let endpoint = '—';
  let params = new URLSearchParams();
  try {
    const url = new URL(line);
    endpoint = url.host || endpoint;
    params = url.searchParams;
  } catch {
    // 没有可用地址的条目（服务端写作「?」）仍然列出，便于对照原文。
  }
  let protocol: NodeProtocol = 'other';
  let stack = '';
  if (scheme === 'vless') {
    const encryption = params.get('encryption');
    protocol = encryption && encryption !== 'none' ? 'vless-encryption' : 'vless';
    const security = params.get('security');
    const securityLabel =
      protocol === 'vless-encryption'
        ? 'Encryption'
        : security === 'reality'
          ? 'REALITY'
          : (security ?? '').toUpperCase();
    stack = [securityLabel, (params.get('type') ?? '').toUpperCase()].filter(Boolean).join(' · ');
  } else if (scheme === 'anytls') {
    protocol = 'anytls';
    stack = params.get('security') === 'reality' ? 'REALITY' : 'TLS';
  } else if (scheme === 'hysteria2' || scheme === 'hy2') {
    protocol = 'hysteria2';
    stack = 'QUIC';
  }
  return {
    uri: line,
    name: label,
    base: baseName(label),
    region,
    protocol,
    family: label.endsWith(FAMILY_SUFFIX) ? 'ipv6' : 'ipv4',
    endpoint,
    stack,
    insecure: params.get('insecure') === '1',
  };
}

export function parseNodeListing(text: string): { links: NodeLink[]; notes: string[] } {
  const links: NodeLink[] = [];
  const notes: string[] = [];
  let block: string[] = [];
  const flush = () => {
    // 段内各行接成一句：上一行以冒号或句读结尾时直接相接，否则补一个空格。
    const note = block
      .reduce((joined, line) => (joined && !/[：:，,。]$/.test(joined) ? `${joined} ${line}` : `${joined}${line}`), '')
      .trim();
    // 「（……）」是没有任何条目时的占位行，由空列表本身表达。
    if (note && !/^（.*）$/.test(note) && !notes.includes(note)) notes.push(note);
    block = [];
  };
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line) {
      flush();
    } else if (line.startsWith('#')) {
      block.push(stripRegions(line.replace(/^#\s?/, '')).trim());
    } else {
      flush();
      links.push(parseNodeLink(line));
    }
  }
  flush();
  return { links, notes };
}

// 连续同名的链接归为一组（一条链的一个或多个接入点）；同一协议的 IPv6 条目并入前面缺 IPv6 的
// 那一行，作为该行的第二个复制目标。服务端按「协议 → 地址族」的顺序输出。
export function groupNodeLinks(links: NodeLink[]): NodeGroup[] {
  const groups: NodeGroup[] = [];
  for (const link of links) {
    let group = groups.at(-1);
    if (!group || group.name !== link.base || group.region !== link.region) {
      group = { key: `${groups.length}`, name: link.base, region: link.region, rows: [] };
      groups.push(group);
    }
    if (link.family === 'ipv6') {
      const pair = [...group.rows]
        .reverse()
        .find(row => row.protocol === link.protocol && row.stack === link.stack && row.links.ipv4 && !row.links.ipv6);
      if (pair) {
        pair.links.ipv6 = link;
        pair.insecure = pair.insecure || link.insecure;
        continue;
      }
    }
    group.rows.push({
      key: `${group.rows.length}`,
      protocol: link.protocol,
      stack: link.stack,
      endpoint: link.endpoint,
      insecure: link.insecure,
      links: { [link.family]: link },
    });
  }
  return groups;
}

const TOKEN_MASK = '••••••••-••••-••••-••••-••••••••';

/* 订阅地址里的 Token 换成与 UUID 同形的圆点，只留末四位。地址不是预期形状时原样返回 null。 */
function maskSubscriptionUrl(url: string): { head: string; tail: string } | null {
  const match = /(\/sub\/v1\/(?:haitun\/)?)([^/]+)(\/clash\.yaml)/.exec(url);
  if (!match) return null;
  const start = match.index + match[1].length;
  const end = start + match[2].length;
  return { head: url.slice(0, start), tail: `${match[2].slice(-4)}${url.slice(end)}` };
}

function withSubscriptionProtocol(url: string, protocol: ProtocolPick): string {
  if (!url || protocol === 'both') return url;
  const selected = new URL(url);
  selected.searchParams.set('protocol', protocol);
  return selected.toString();
}

/* 重置时间按 UTC+8 显示，时区写在读数的 title 里。 */
function formatResetAt(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return `${new Intl.DateTimeFormat('zh-CN', {
    timeZone: 'Asia/Hong_Kong',
    month: 'numeric',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(date)}`;
}
