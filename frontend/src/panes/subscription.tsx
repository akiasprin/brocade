import { SubscriptionLogo } from './subscription-logo';
import {
  SubStoreShell,
  SubStoreContent,
  SubStoreTicket,
  SubStoreActions,
  SubscriptionTicketPeeks,
  type SubStoreNavigationGuard,
} from './subscription-substore';
import {
  useEffect,
  useCallback,
  useId,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { QRCodeSVG } from 'qrcode.react';
import {
  ApiError,
  cancelGrantProbe,
  fetchArtifactContent,
  fetchClashSubscription,
  fetchGrantProbeCapability,
  fetchGrantProbeJob,
  fetchMyArtifact,
  fetchMyClashSubscription,
  fetchRevisions,
  fetchUserGrantProbePlan,
  grantProbeEventsUrl,
  issueClashHaitunSubscription,
  revokeClashHaitunSubscription,
  startUserGrantProbe,
  type ArtifactFamily,
  type ArtifactProtocol,
  type ClashHaitunSubscriptionInfo,
  type ClashSubscriptionInfo,
  type GrantProbeItemStatus,
  type GrantProbeJob,
  type GrantProbeJobItem,
  type GrantProbePlanItem,
} from '../api';
import { GRANT_REFRESH_MS, GRANT_REFRESH_WINDOW_MS, useGrantEdits } from '../grant-edits';
import { ErrorBox, Loading } from '../ui/bits';
import { ChoiceSelect } from '../ui/choice-menu';
import { CopyButton, copyStateIcon, type CopyState } from '../ui/copy-button';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { bytes } from '../ui/format';
import { Icon, type IconName } from '../ui/icons';
import { RegionFlag } from '../ui/region-flag';

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

const NODE_PROTOCOL_PICKS = [...PROTOCOL_PICKS, { value: 'mtproto' as const, label: 'MTProxy' }];

/* 卡券菜单里只给需要解释的取值写一行说明：「全部」「双栈」各包含什么。 */
const PROTOCOL_DETAIL: Partial<Record<ProtocolPick, string>> = { both: 'VLESS · AnyTLS · Hysteria 2' };
const FAMILY_DETAIL: Partial<Record<FamilyPick, string>> = { both: 'IPv4 · IPv6' };

const TABS: { value: SubscriptionKind; label: string; icon: IconName }[] = [
  { value: 'clash', label: '订阅', icon: 'subscription' },
  { value: 'uri', label: '节点', icon: 'nodeLinks' },
];

/* 两个页签各自渲染正文；正文元素是当前页签对应的 tabpanel。 */
interface TabPanelProps {
  role: 'tabpanel';
  id: string;
  'aria-labelledby': string;
}

/* 订阅与节点共用一个弹窗：标题栏切换「订阅 / 节点」，协议与地址族的取值两个页签共用。
 * 两边的内容都由服务端按筛选条件生成：订阅是带 family / protocol 参数的地址，节点是同一条件下的
 * uri.txt。浏览器解析后按地区或拨测失败筛选，复制与拨测都作用于当前可见节点。
 * 订阅的筛选在卡券上，节点的筛选在白色内容区顶部，灰色边栏选择地区。窄屏弹窗贴底成为面板。 */
export function SubscriptionViewer({
  tenant,
  user,
  kind,
  selfService = false,
  identity,
  onProbePassed,
  onClose,
}: {
  tenant: string;
  user: string;
  kind: SubscriptionKind;
  selfService?: boolean;
  identity?: ReactNode;
  onProbePassed?: (result: { probeId: string; itemId: string }) => void;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<SubscriptionKind>(kind);
  const navigationGuard = useRef<SubStoreNavigationGuard | null>(null);
  const registerGuard = useCallback((guard: SubStoreNavigationGuard | null) => {
    navigationGuard.current = guard;
  }, []);
  const [protocol, setProtocol] = useState<ProtocolPick>('both');
  const [family, setFamily] = useState<FamilyPick>('both');
  // 默认列出自签证书节点（带 insecure 标记），节点页签的说明行里可以改为不列出。切到 VLESS 不收回：
  // VLESS 自签入口没有可互通的链接写法，服务端无论是否允许 insecure 都会跳过并给出说明。
  const [allowInsecure, setAllowInsecure] = useState(true);
  const ids = useId();
  const tabId = (value: SubscriptionKind) => `${ids}-tab-${value}`;
  const panel: TabPanelProps = { role: 'tabpanel', id: `${ids}-panel`, 'aria-labelledby': tabId(tab) };

  const changeTab = (next: SubscriptionKind) => {
    const go = () => {
      if (next === 'clash' && protocol === 'mtproto') setProtocol('both');
      setTab(next);
    };
    if (!navigationGuard.current || navigationGuard.current(go)) go();
  };

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
    changeTab(value);
    window.requestAnimationFrame(() => document.getElementById(tabId(value))?.focus());
  };

  return (
    <DialogLayer
      label={`${user} 的订阅与节点`}
      onClose={onClose}
      canClose={() => navigationGuard.current?.(onClose) ?? true}
    >
      <section
        className={`dialog-surface sub-dialog${tab === 'clash' ? ' sub-dialog-subscription' : ' sub-dialog-nodes'}`}
      >
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
                  onClick={() => changeTab(item.value)}
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
        {tab === 'clash' ? (
          <ClashSubscription
            tenant={tenant}
            user={user}
            selfService={selfService}
            onGuard={registerGuard}
            protocol={protocol}
            family={family}
            onProtocol={setProtocol}
            onFamily={setFamily}
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
            onUseSubscription={() => changeTab('clash')}
            onProbePassed={onProbePassed}
            onProtocol={setProtocol}
            onFamily={setFamily}
            panel={panel}
          />
        )}
      </section>
    </DialogLayer>
  );
}

/* ── 订阅 ──
 * 订阅画成一张卡券：卡头是名称与重置时间，大字是剩余流量，协议与地址族是卡上的两个栏位，撕线下是二维码与
 * 遮罩后的地址。二维码一律白底黑码：反色码不是每个扫码器都认，暗色主题下也不反。管理员的 Koipy 测速订阅是
 * 叠在后面的同版式第二张卡，点露出的卡头换到前面；两张卡共用筛选，地址都取服务端返回的筛选后 URL。 */

type PassCard = 'clash' | 'haitun' | 'substore';

const HAITUN_STATUS: Record<ClashHaitunSubscriptionInfo['status'], string> = {
  active: '可用',
  revoked: '已撤销',
  'not-created': '尚未生成',
};

const CREDENTIAL_NOTE = '订阅地址与二维码包含访问凭据，请勿公开分享；内容按当前授权实时生成。';
const HAITUN_NOTE =
  '测速订阅包含用户 UUID 等真实节点凭据，持有地址的人无需登录即可获取并使用节点，请仅交给可信测速服务。' +
  '撤销仅停止测速地址的后续拉取，不会自动生成新地址，不影响普通订阅，也不会使已下载的节点配置失效。' +
  '若凭据泄露，需撤销测速地址、更换用户 UUID 并发布授权，待节点生效后旧凭据才失效。';

function ClashSubscription({
  tenant,
  user,
  selfService,
  onGuard,
  protocol,
  family,
  onProtocol,
  onFamily,
  panel,
}: {
  tenant: string;
  user: string;
  selfService: boolean;
  onGuard: (guard: SubStoreNavigationGuard | null) => void;
  protocol: ProtocolPick;
  family: FamilyPick;
  onProtocol: (value: ProtocolPick) => void;
  onFamily: (value: FamilyPick) => void;
  panel: TabPanelProps;
}) {
  const qrRef = useRef<SVGSVGElement>(null);
  const [front, setFront] = useState<PassCard>('clash');
  // 换过卡之后新卡才播滑入动效；首次打开时弹窗自己已有进场动效。
  const [swapped, setSwapped] = useState(false);
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
    // A read started before revocation must not restore the invalid old URL.
    await qc.cancelQueries({ queryKey });
    qc.setQueryData<ClashSubscriptionInfo>(queryKey, current => (current ? { ...current, haitun } : current));
  };
  const issueHaitun = useMutation({
    mutationFn: () => issueClashHaitunSubscription(tenant, user),
    onSuccess: updateHaitun,
  });
  const revokeHaitun = useMutation({
    mutationFn: () => revokeClashHaitunSubscription(tenant, user),
    onSuccess: updateHaitun,
  });

  const value = subscription.data;
  const standardUrl = value ? withSubscriptionProtocol(value.urls[family], protocol) : null;
  const haitun = selfService ? null : (value?.haitun ?? null);
  const haitunUrl =
    haitun?.status === 'active' && haitun.urls ? withSubscriptionProtocol(haitun.urls[family], protocol) : null;
  const actionError = issueHaitun.error ?? revokeHaitun.error;
  const haitunPending = issueHaitun.isPending || revokeHaitun.isPending;
  const fields = <PassFields protocol={protocol} family={family} onProtocol={onProtocol} onFamily={onFamily} />;
  const copyContent = (state: CopyState, label: string) => (
    <>
      <Icon of={copyStateIcon(state)} size={14} />
      {label}
    </>
  );

  return (
    <SubStoreShell tenant={tenant} user={user} onGuard={onGuard} panel={panel}>
      <SubStoreContent>
        {available => (
          <div className="sub-dialog-body sub-subscription-body" {...panel}>
            <div
              className={`sub-pass-stack${haitun || available ? ' has-back' : ''}${haitun && available ? ' substore-two-backs' : ''}${swapped ? ' swapped' : ''}`}
            >
              <SubscriptionTicketPeeks
                front={front}
                haitun={haitun}
                value={value}
                onSelect={next => {
                  setFront(next);
                  setSwapped(true);
                }}
              />
              {front === 'substore' ? (
                <SubStoreTicket />
              ) : haitun && front === 'haitun' ? (
                <HaitunPass haitun={haitun} url={haitunUrl} fields={fields} />
              ) : value && standardUrl ? (
                <ClashPass value={value} url={standardUrl} qrRef={qrRef} fields={fields} />
              ) : (
                <div className="sub-pass substore-unavailable">
                  {subscription.error ? <ErrorBox error={subscription.error} /> : <Loading variant="code" />}
                </div>
              )}
            </div>
            {front === 'substore' ? (
              <SubStoreActions />
            ) : haitun && front === 'haitun' ? (
              <>
                <div className="sub-pass-actions">
                  {haitunUrl ? (
                    <>
                      <CopyButton className="btn primary sub-pass-main" text={haitunUrl} label="复制测速地址">
                        {copyContent}
                      </CopyButton>
                      <RevokeButton
                        pending={revokeHaitun.isPending}
                        disabled={haitunPending}
                        onRevoke={() => {
                          issueHaitun.reset();
                          revokeHaitun.mutate();
                        }}
                      />
                    </>
                  ) : (
                    <button
                      type="button"
                      className="btn primary sub-pass-main"
                      disabled={haitunPending}
                      onClick={() => {
                        revokeHaitun.reset();
                        issueHaitun.mutate();
                      }}
                    >
                      {issueHaitun.isPending
                        ? '生成中…'
                        : haitun.status === 'revoked'
                          ? '重新生成测速地址'
                          : '生成测速地址'}
                    </button>
                  )}
                </div>
                {actionError && <ErrorBox error={actionError} />}
              </>
            ) : standardUrl ? (
              <div className="sub-pass-actions">
                <CopyButton className="btn primary sub-pass-main" text={standardUrl} label="复制订阅地址">
                  {copyContent}
                </CopyButton>
                <button
                  type="button"
                  className="btn sub-pass-side"
                  aria-label="下载二维码"
                  title="下载二维码"
                  onClick={() => {
                    if (qrRef.current) downloadSubscriptionQr(qrRef.current);
                  }}
                >
                  <Icon of="download" size={14} />
                  下载
                </button>
              </div>
            ) : null}
          </div>
        )}
      </SubStoreContent>
    </SubStoreShell>
  );
}

const remainingText = (remaining: number | null) => (remaining === null ? '不限量' : bytes(remaining));

/* 卡头名称后的 (!)：与「计量口径」同一画法，点开是卡头下方一段暗色小字。 */
function usePassNote(label: string, text: string) {
  const [open, setOpen] = useState(false);
  const id = useId();
  return {
    button: (
      <button
        type="button"
        className="sub-pass-info"
        aria-label={`${label}说明`}
        aria-expanded={open}
        aria-controls={id}
        onClick={() => setOpen(shown => !shown)}
      >
        <Icon of="info" size={13} />
      </button>
    ),
    note: (
      <p id={id} className="sub-pass-note" hidden={!open}>
        {text}
      </p>
    ),
  };
}

function ClashPass({
  value,
  url,
  qrRef,
  fields,
}: {
  value: ClashSubscriptionInfo;
  url: string;
  qrRef: RefObject<SVGSVGElement | null>;
  fields: ReactNode;
}) {
  const note = usePassNote('订阅凭据', CREDENTIAL_NOTE);
  const [amount, unit] = remainingText(value.remaining_bytes).split(' ');
  const expires = formatDay(value.expires_at);
  return (
    <section className="sub-pass" aria-label="Clash 订阅">
      <div className="sub-pass-top">
        <header className="sub-pass-head">
          <SubscriptionLogo card="clash" />
          <span className="sub-pass-name">
            <b>
              Clash 订阅
              {note.button}
            </b>
            <small title={`模板 ${value.template}`}>Mihomo / Clash Meta</small>
          </span>
          <span className="sub-pass-field end">
            <small>重置</small>
            <b title="按 UTC+8 计">{formatResetAt(value.reset_at)}</b>
          </span>
        </header>
        {note.note}
        <div className="sub-pass-primary">
          <small>剩余流量</small>
          <b className={value.remaining_bytes === 0 ? 'bad' : undefined}>
            {amount}
            {unit && <span>{unit}</span>}
          </b>
        </div>
        <div className="sub-pass-fields">
          {fields}
          {expires && (
            <span className="sub-pass-field">
              <small>到期</small>
              <b>{expires}</b>
            </span>
          )}
        </div>
      </div>
      <div className="sub-pass-code">
        <figure className="sub-qr">
          <QRCodeSVG
            ref={qrRef}
            value={url}
            size={216}
            level="M"
            marginSize={4}
            bgColor="#fff"
            fgColor="#101112"
            title="Clash 订阅地址二维码"
          />
        </figure>
        <PassAddress key={url} url={url} name="Clash 订阅地址" />
      </div>
    </section>
  );
}

/* 测速订阅卡与订阅卡同一版式：大字是状态，没有地址时条码区留一个同尺寸的空框，换卡时弹窗不跳。 */
function HaitunPass({
  haitun,
  url,
  fields,
}: {
  haitun: ClashHaitunSubscriptionInfo;
  url: string | null;
  fields: ReactNode;
}) {
  const note = usePassNote('测速订阅', HAITUN_NOTE);
  const revoked = haitun.status === 'revoked';
  const date = formatDay(revoked ? haitun.revoked_at : haitun.created_at);
  return (
    <section className="sub-pass" aria-label="Koipy 测速订阅">
      <div className="sub-pass-top">
        <header className="sub-pass-head">
          <SubscriptionLogo card="haitun" />
          <span className="sub-pass-name">
            <b>
              Koipy 测速订阅
              {note.button}
            </b>
            <small>仅管理员可见</small>
          </span>
          {date && (
            <span className="sub-pass-field end">
              <small>{revoked ? '撤销于' : '生成于'}</small>
              <b>{date}</b>
            </span>
          )}
        </header>
        {note.note}
        <div className="sub-pass-primary">
          <small>状态</small>
          <b className="sub-pass-status" data-status={haitun.status}>
            {HAITUN_STATUS[haitun.status]}
          </b>
        </div>
        <div className="sub-pass-fields">{fields}</div>
      </div>
      <div className="sub-pass-code">
        {url ? (
          <>
            <figure className="sub-qr">
              <QRCodeSVG
                value={url}
                size={216}
                level="M"
                marginSize={4}
                bgColor="#fff"
                fgColor="#101112"
                title="Koipy 测速地址二维码"
              />
            </figure>
            <PassAddress key={url} url={url} name="Koipy 测速地址" />
          </>
        ) : (
          <div className="sub-pass-empty">暂无地址</div>
        )}
      </div>
    </section>
  );
}

/* 协议与地址族是卡上的两个栏位：小字标签由样式取 data-label，取值后是弹出标记，点开是共用的首版菜单。 */
function PassFields({
  protocol,
  family,
  onProtocol,
  onFamily,
}: {
  protocol: ProtocolPick;
  family: FamilyPick;
  onProtocol: (value: ProtocolPick) => void;
  onFamily: (value: FamilyPick) => void;
}) {
  return (
    <>
      <ChoiceSelect
        className="sub-pass-pick"
        aria-label="协议"
        data-label="协议"
        menuAlign="below"
        value={protocol}
        onChange={event => {
          const next = PROTOCOL_PICKS.find(pick => pick.value === event.target.value);
          if (next) onProtocol(next.value);
        }}
      >
        {PROTOCOL_PICKS.map(pick => (
          <option key={pick.value} value={pick.value} data-detail={PROTOCOL_DETAIL[pick.value]}>
            {pick.label}
          </option>
        ))}
      </ChoiceSelect>
      <ChoiceSelect
        className="sub-pass-pick"
        aria-label="地址族"
        data-label="地址族"
        menuAlign="below"
        value={family}
        onChange={event => {
          const next = FAMILY_PICKS.find(pick => pick.value === event.target.value);
          if (next) onFamily(next.value);
        }}
      >
        {FAMILY_PICKS.map(pick => (
          <option key={pick.value} value={pick.value} data-detail={FAMILY_DETAIL[pick.value]}>
            {pick.label}
          </option>
        ))}
      </ChoiceSelect>
    </>
  );
}

/* 条码下方一行：只露主机名与 Token 末四位，点一下显示完整地址。调用方以地址作 key，筛选一变就重新遮罩。 */
function PassAddress({ url, name }: { url: string; name: string }) {
  const [revealed, setRevealed] = useState(false);
  const masked = revealed ? null : maskSubscriptionUrl(url);
  return (
    <button
      type="button"
      className="sub-pass-address"
      aria-pressed={revealed}
      aria-label={`${revealed ? '隐藏' : '显示'} ${name}`}
      onClick={() => setRevealed(shown => !shown)}
    >
      <code>
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
      <Icon of={revealed ? 'eyeOff' : 'eye'} size={13} />
    </button>
  );
}

/* 撤销就地两步确认：第一次点击变成「确认撤销」，再点才执行；焦点移开或 5 秒内不确认就复原。 */
function RevokeButton({ pending, disabled, onRevoke }: { pending: boolean; disabled: boolean; onRevoke: () => void }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const timer = window.setTimeout(() => setArmed(false), 5_000);
    return () => window.clearTimeout(timer);
  }, [armed]);
  return (
    <button
      type="button"
      className={`btn danger sub-pass-side sub-pass-revoke${armed ? ' armed' : ''}`}
      aria-label={pending ? undefined : armed ? '确认撤销测速地址' : '撤销测速地址'}
      disabled={disabled}
      onBlur={() => setArmed(false)}
      onClick={() => {
        if (!armed) {
          setArmed(true);
          return;
        }
        setArmed(false);
        onRevoke();
      }}
    >
      {pending ? '撤销中…' : armed ? '确认撤销' : '撤销'}
    </button>
  );
}

/** Export resolved colors so the downloaded SVG is self-contained, without external CSS. */
function downloadSubscriptionQr(svg: SVGSVGElement) {
  const image = svg.cloneNode(true) as SVGSVGElement;
  const paths = svg.querySelectorAll('path');
  image
    .querySelectorAll('path')
    .forEach((path, index) => path.setAttribute('fill', getComputedStyle(paths[index]).fill));
  const blob = new Blob([new XMLSerializer().serializeToString(image)], { type: 'image/svg+xml;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = 'clash-subscription.svg';
  document.body.appendChild(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1_000);
}

/* ── 节点 ── */

const NODE_PROTOCOL_LABEL: Record<NodeProtocol, string> = {
  vless: 'VLESS',
  'vless-encryption': 'VLESS',
  anytls: 'AnyTLS',
  hysteria2: 'Hysteria 2',
  mtproto: 'MTProxy',
  other: '其他',
};

const NODE_PROTOCOL_ICON: Record<NodeProtocol, IconName> = {
  vless: 'xray',
  'vless-encryption': 'xray',
  anytls: 'bolt',
  hysteria2: 'hysteria',
  mtproto: 'telegram',
  other: 'protocol',
};

const NODE_FAMILY_LABEL: Record<NodeFamily, string> = { ipv4: 'IPv4', ipv6: 'IPv6' };

type CssVars = CSSProperties & Record<`--${string}`, string>;

function NodeLinks({
  tenant,
  user,
  selfService,
  protocol,
  family,
  allowInsecure,
  onAllowInsecure,
  onUseSubscription,
  onProbePassed,
  onProtocol,
  onFamily,
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
  onProbePassed?: (result: { probeId: string; itemId: string }) => void;
  onProtocol: (value: ProtocolPick) => void;
  onFamily: (value: FamilyPick) => void;
  panel: TabPanelProps;
}) {
  const [scope, setScope] = useState('all');
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (listRef.current) listRef.current.scrollTop = 0;
  }, [scope, protocol, family, allowInsecure]);
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
  const probe = useUserGrantProbe(tenant, user);
  const reportedPassed = useRef<string | null>(null);
  const passedItem = probe.job?.items.find(item => item.status === 'passed');
  useEffect(() => {
    if (!probe.job || !passedItem || !onProbePassed) return;
    const key = `${probe.job.id}:${passedItem.id}`;
    if (reportedPassed.current === key) return;
    reportedPassed.current = key;
    onProbePassed({ probeId: probe.job.id, itemId: passedItem.id });
  }, [onProbePassed, passedItem, probe.job]);

  // 拨测状态与节点一起揭示：先出列表、后补状态灯会让每个地址横移一次。
  if (!revision || content.isPending || content.error || probe.pending) {
    return (
      <div className="sub-dialog-body" {...panel}>
        {content.error ? <ErrorBox error={content.error} /> : <Loading variant="code" />}
      </div>
    );
  }

  const text = content.data.content ?? '';
  const listing = parseNodeListing(text);
  const groups = groupNodeLinks(listing.links);
  const insecureCount = allowInsecure ? listing.links.filter(link => link.insecure).length : 0;

  // 拨测、进度与重试只作用于列出的节点：没有分享链接的条目（带前置代理、VLESS 自签，见说明）
  // 与筛选掉的条目都不在其中，所见即所测。
  const probeOf = probe.ready ? pairProbeItems(listing.links, probe.items) : new Map<NodeLink, ProbeItem>();
  const job = probe.job;
  const running = job?.status === 'running';
  const regions = Array.from(new Set(groups.map(group => group.region)));
  const failedLinks = listing.links.filter(link => probeOf.get(link)?.status === 'failed');
  const knownScope =
    scope === 'all' ||
    (scope === 'failed' && failedLinks.length > 0) ||
    regions.some(region => nodeRegionKey(region) === scope);
  const current = knownScope ? scope : 'all';
  const visibleLinks =
    current === 'failed'
      ? failedLinks
      : current === 'all'
        ? listing.links
        : listing.links.filter(link => nodeRegionKey(link.region) === current);
  const slots = NODE_FAMILIES.filter(slot => visibleLinks.some(link => link.family === slot));
  const visibleGroups = groupNodeLinks(visibleLinks);
  const listed = visibleLinks.flatMap(link => probeOf.get(link) ?? []);
  const listedIds = new Set(listed.map(item => item.id));
  const jobItems = job?.items.filter(item => listedIds.has(item.id)) ?? [];
  const failedIds = listed.filter(item => item.status === 'failed').map(item => item.id);
  const count = (status: GrantProbeItemStatus) => jobItems.filter(item => item.status === status).length;
  const complete = jobItems.length - count('waiting') - count('running');
  const progress: CssVars = {
    '--progress': `${jobItems.length ? Math.round((complete / jobItems.length) * 100) : 0}%`,
  };
  // 空数组是服务端的「全部」，只允许在当前可见节点覆盖整个计划时使用。
  const allIds = listed.length === probe.items.length ? [] : listed.map(item => item.id);
  const groupRunning = (ids: string[]) =>
    job?.status === 'running' && job.items.length === ids.length && job.items.every(item => ids.includes(item.id));
  const jobNote = job && (job.status === 'canceled' || job.status === 'superseded') ? job.message : null;
  const probeIssue = probe.unavailable
    ? { text: `拨测不可用：${probe.unavailable}`, bad: false }
    : probe.error
      ? {
          text: `拨测出错：${probe.error instanceof Error ? probe.error.message : '暂时无法读取 Serving 授权'}`,
          bad: true,
        }
      : null;

  const groupProbe = (group: NodeGroup) => {
    const items = group.rows.flatMap(row =>
      slots.flatMap(slot => {
        const link = row.links[slot];
        return (link && probeOf.get(link)) ?? [];
      }),
    );
    if (items.length === 0) return null;
    const ids = items.map(item => item.id);
    const result = probeResult(items);
    const active = groupRunning(ids);
    return (
      <span className="node-group-probe">
        {result && (
          <span
            className={`node-group-result${result.bad ? ' bad' : ''}`}
            data-tone={result.bad ? 'err' : result.text === '拨测中' ? 'run' : 'ok'}
          >
            <i aria-hidden="true" />
            {result.text === '全部通过' ? '通过' : result.text}
          </span>
        )}
        <button
          type="button"
          className={`btn node-group-run${active ? ' node-probe-progress' : ''}`}
          style={active ? progress : undefined}
          aria-label={`拨测 ${group.name}`}
          disabled={probe.busy}
          onClick={() => probe.start(ids)}
        >
          {result?.bad ? '重试' : '拨测'}
        </button>
      </span>
    );
  };

  const sections =
    current === 'all'
      ? regions.map(region => ({ region, groups: visibleGroups.filter(group => group.region === region) }))
      : [{ region: null, groups: visibleGroups }];
  const scoped = current !== 'all';
  return (
    <div className="sub-dialog-body node-browser" {...panel}>
      <nav className="node-sidebar" aria-label="按地区筛选">
        <NodeScope
          active={current === 'all'}
          label="全部"
          count={listing.links.length}
          icon="nodeLinks"
          onClick={() => setScope('all')}
        />
        {failedLinks.length > 0 && (
          <NodeScope
            active={current === 'failed'}
            label="失败"
            count={failedLinks.length}
            icon="warn"
            onClick={() => setScope('failed')}
          />
        )}
        <small className="node-sidebar-heading">地区</small>
        {regions.map(region => (
          <NodeScope
            key={nodeRegionKey(region)}
            active={current === nodeRegionKey(region)}
            label={nodeRegionName(region)}
            count={listing.links.filter(link => link.region === region).length}
            region={region}
            onClick={() => setScope(nodeRegionKey(region))}
          />
        ))}
      </nav>
      <div className="node-main">
        <div className="node-toolbar">
          <ChoiceSelect
            className="sub-pass-pick"
            aria-label="协议"
            data-label="协议"
            menuAlign="below"
            value={protocol}
            onChange={event => {
              const next = NODE_PROTOCOL_PICKS.find(pick => pick.value === event.target.value);
              if (next) onProtocol(next.value);
            }}
          >
            {NODE_PROTOCOL_PICKS.map(pick => (
              <option
                key={pick.value}
                value={pick.value}
                data-detail={pick.value === 'both' ? 'VLESS · AnyTLS · Hysteria 2 · MTProxy' : undefined}
              >
                {pick.label}
              </option>
            ))}
          </ChoiceSelect>
          <ChoiceSelect
            className="sub-pass-pick"
            aria-label="地址族"
            data-label="地址族"
            menuAlign="below"
            value={family}
            onChange={event => {
              const next = FAMILY_PICKS.find(pick => pick.value === event.target.value);
              if (next) onFamily(next.value);
            }}
          >
            {FAMILY_PICKS.map(pick => (
              <option key={pick.value} value={pick.value} data-detail={FAMILY_DETAIL[pick.value]}>
                {pick.label}
              </option>
            ))}
          </ChoiceSelect>
          <span className="sp" />
          <CopyButton
            key={`copy-${current}-${protocol}-${family}`}
            className="btn node-copy-all"
            text={visibleLinks.map(link => link.uri).join('\n')}
            label={scoped ? `复制 ${visibleLinks.length} 个` : '复制全部'}
            title={`节点链接含 ${user} 的凭据，更换 UUID 后全部失效`}
          >
            {(state, label) => (
              <>
                <Icon of={copyStateIcon(state)} size={13} />
                {label}
              </>
            )}
          </CopyButton>
          {failedIds.length > 0 && !running && current !== 'failed' && (
            <button
              type="button"
              className="btn node-probe-retry"
              disabled={probe.busy}
              onClick={() => probe.start(failedIds)}
            >
              重试失败项
            </button>
          )}
          <button
            type="button"
            className={`btn node-probe-all${running ? ' node-probe-progress' : ' primary'}`}
            style={running ? progress : undefined}
            title={`使用 ${user} 当前生效的真实用户凭据从公网验证当前筛选的节点；拨测流量计入该用户用量；只检查本次授权是否可用，不代替周期健康观测`}
            disabled={running ? probe.cancelling : !probe.ready || probe.busy || listed.length === 0}
            onClick={() => (running ? probe.cancel() : probe.start(allIds))}
          >
            <Icon of="diag" size={13} />
            {running ? '取消拨测' : probe.starting ? '创建中…' : scoped ? `拨测 ${listed.length} 个` : '拨测全部'}
          </button>
        </div>
        <div className={`node-list${slots.length < 2 ? ' single' : ''}`} ref={listRef}>
          <NodeNotes
            notes={listing.notes}
            insecureCount={insecureCount}
            probeIssue={probeIssue}
            onAllowInsecure={onAllowInsecure}
            onUseSubscription={onUseSubscription}
          />
          {job && (jobItems.length > 0 || jobNote) && (
            <p className="node-probe-tally" role="status" title={job.message ?? undefined}>
              {jobNote && <b>{jobNote}</b>}
              {`通过 ${count('passed')} · 运行 ${count('running')} · 等待 ${count('waiting')} · 失败 ${failedIds.length}`}
            </p>
          )}
          {slots.length > 1 && (
            <div className="node-matrix-head" aria-hidden="true">
              <span />
              {slots.map(slot => (
                <span key={slot}>{NODE_FAMILY_LABEL[slot]}</span>
              ))}
            </div>
          )}
          {visibleGroups.length === 0 ? (
            <div className="node-empty">没有符合当前协议与地址族的节点</div>
          ) : (
            sections.map(section => (
              <section key={current === 'all' ? nodeRegionKey(section.region) : 'scope'} className="node-region">
                {current === 'all' && (
                  <header className="node-region-head">
                    <RegionFlag code={section.region} />
                    <b>{nodeRegionName(section.region)}</b>
                    <small>{section.groups.length} 条线路</small>
                  </header>
                )}
                <NodeMatrix
                  groups={section.groups}
                  slots={slots}
                  probeOf={probeOf.size > 0 ? probeOf : null}
                  groupProbe={probeOf.size > 0 ? groupProbe : undefined}
                />
              </section>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

const NODE_FAMILIES: NodeFamily[] = ['ipv4', 'ipv6'];
const NODE_REGION_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
const nodeRegionKey = (region: string | null) => region ?? 'other';
const nodeRegionName = (region: string | null) =>
  region === 'CN' ? '中国大陆' : region ? (NODE_REGION_NAMES.of(region) ?? region) : '其他';

function NodeScope({
  active,
  label,
  count,
  icon,
  region,
  onClick,
}: {
  active: boolean;
  label: string;
  count: number;
  icon?: IconName;
  region?: string | null;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={`node-scope${icon === 'warn' ? ' bad' : ''}`}
      aria-current={active || undefined}
      aria-label={`${label} ${count}`}
      onClick={onClick}
    >
      <span className="node-scope-icon" aria-hidden="true">
        {icon ? <Icon of={icon} size={14} /> : <RegionFlag code={region} />}
      </span>
      <span className="node-scope-name">{label}</span>
      <span className="node-scope-count">{count}</span>
    </button>
  );
}

// 服务端的说明写作「以下自签证书地址默认隐藏；……：名称、名称」，那是 uri.txt 自身的默认。
// 这个弹窗默认列出自签节点，用户改为不列出后，原句的「默认隐藏」与弹窗的默认相反，改写为当前状态。
const HIDDEN_SELF_SIGNED = /^以下自签证书地址默认隐藏[^：]*：(.+)$/;

/* 列表之前的说明：拨测不可用或出错的原因、已列出的自签节点、未列出的条目及原因，后两者各带一个就地操作。 */
function NodeNotes({
  notes,
  insecureCount,
  probeIssue,
  onAllowInsecure,
  onUseSubscription,
}: {
  notes: string[];
  insecureCount: number;
  probeIssue: { text: string; bad: boolean } | null;
  onAllowInsecure: (allow: boolean) => void;
  onUseSubscription: () => void;
}) {
  if (notes.length === 0 && insecureCount === 0 && !probeIssue) return null;
  return (
    <div className="node-notes">
      {probeIssue && (
        <p className={probeIssue.bad ? 'bad' : undefined}>
          <Icon of="diag" size={13} className="node-note-icon" />
          <span className="node-note-text">{probeIssue.text}</span>
        </p>
      )}
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

/* 一条线路一组，一种协议一行；仅为实际存在的地址族留列，完整地址就是复制按钮。 */
function NodeMatrix({
  groups,
  slots,
  probeOf,
  groupProbe,
}: {
  groups: NodeGroup[];
  slots: NodeFamily[];
  probeOf: Map<NodeLink, ProbeItem> | null;
  groupProbe?: (group: NodeGroup) => ReactNode;
}) {
  return (
    <div className="node-matrix">
      {groups.map(group => (
        <section className="node-group" key={group.key} aria-label={group.name}>
          <div className="node-group-head">
            <RegionFlag code={group.region} />
            <b>{group.name}</b>
            {groupProbe?.(group)}
          </div>
          {group.rows.map(row => (
            <div className="node-row" key={row.key}>
              <span className="node-protocol">
                <Icon of={NODE_PROTOCOL_ICON[row.protocol]} size={13} />
                <b>{NODE_PROTOCOL_LABEL[row.protocol]}</b>
                <span>{row.stack}</span>
                {row.insecure && <em>insecure</em>}
              </span>
              {slots.map(slot => {
                const link = row.links[slot];
                return (
                  <NodeAddress
                    key={slot}
                    link={link}
                    family={slot}
                    probe={probeOf && link ? (probeOf.get(link) ?? null) : undefined}
                  />
                );
              })}
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}

const PROBE_LAMP: Record<GrantProbeItemStatus, string> = {
  waiting: 'defer',
  running: 'run',
  passed: 'ok',
  failed: 'err',
  canceled: 'idle',
};

/* 地址格。probe 为 undefined 表示这一页没有拨测（不留状态灯位置）；为 null 表示有拨测但这条链接不是拨测项
 * （MTProxy、无法按名称对上的条目），留出同宽的空位让同列地址对齐。失败原因写在地址下方：触屏没有悬停提示。 */
function NodeAddress({
  link,
  family,
  probe,
}: {
  link: NodeLink | undefined;
  family: NodeFamily;
  probe?: ProbeItem | null;
}) {
  if (!link) return <span className="node-copy-gap" aria-hidden="true" />;
  const familyLabel = NODE_FAMILY_LABEL[family];
  // 主机允许换行，端口保持完整；长 IPv6 与端口段也不会被截断。
  const [, host, port = ''] = /^(.*?)(:\d+(?:-\d+)?)?$/.exec(link.endpoint) ?? [link.endpoint, link.endpoint];
  const status = probe ? probeStatusText(probe) : '';
  const failure = probe?.status === 'failed' ? probe.detail || '失败' : null;
  return (
    <CopyButton
      className={`node-copy${probe?.status ? ` is-${probe.status}` : ''}${failure ? ' has-note' : ''}`}
      text={link.uri}
      aria-label={`复制 ${link.name}（${familyLabel}）`}
      aria-description={probe ? `拨测：${status}` : undefined}
      title={`${link.endpoint} · 复制分享链接${probe ? `\n拨测：${status}` : ''}`}
    >
      {state => (
        <>
          <i className="node-copy-family">{familyLabel}</i>
          {probe !== undefined && (
            <span
              className={`node-copy-lamp${probe?.status ? ` cg-lamp ${PROBE_LAMP[probe.status]}` : ''}`}
              aria-hidden="true"
            />
          )}
          <span className="node-copy-endpoint">
            <span className="node-copy-address">
              <span className="node-copy-host">{host}</span>
              <span className="node-copy-port">{port}</span>
            </span>
            {failure && <span className="node-copy-note">{failure}</span>}
          </span>
          <Icon of={copyStateIcon(state)} size={13} className="node-copy-icon" />
        </>
      )}
    </CopyButton>
  );
}

/* ── 网络拨测 ──
 * 人工网络拨测与周期 E2E 有意分开：此处用真实用户凭据，回答“alice 现在能不能从这个授权入口走通”；
 * 周期 E2E 用专用 probe 身份，回答线路总体是否健康。拨测只给状态：拨测从控制台所在地发起，首字节时延
 * 反映的是控制台到节点的距离，不代表用户自己的线路，放在这里会被读成线路快慢，因此不显示。 */

type ProbeItem = GrantProbePlanItem & Partial<Pick<GrantProbeJobItem, 'status' | 'detail'>>;

const GRANT_PROBE_POLL_MS = 1_000;

const probeJobKey = (tenant: string, user: string) => ['user-grant-probe-job', tenant, user] as const;

/**
 * 一个用户的拨测计划与最近一轮任务。任务只存在 Console 内存里；浏览器把最近一轮放进查询缓存（不发请求，
 * 只由启动、取消、SSE 与轮询写入），关掉弹窗或切到「订阅」页签再回来仍能看到本轮结果，运行中的任务
 * 重新订阅；没有页面观察时按查询缓存的回收时间丢弃。入门向导的清单就地拨测也用它，与本弹窗共享同一轮任务。
 */
export function useUserGrantProbe(tenant: string, user: string) {
  const qc = useQueryClient();
  const edits = useGrantEdits();
  const latestEdit = edits.reduce(
    (latest, edit) =>
      edit.write?.tenant_id === tenant && edit.write.user_id === user ? Math.max(latest, edit.submittedAt) : latest,
    0,
  );
  const capability = useQuery({
    queryKey: ['grant-probe-capability'],
    queryFn: fetchGrantProbeCapability,
    staleTime: 60_000,
  });
  const plan = useQuery({
    queryKey: ['grant-probe-plan', tenant, user],
    queryFn: async () => {
      try {
        return await fetchUserGrantProbePlan(tenant, user);
      } catch (error) {
        // Revoking the last Serving grant returns 404. Cache an explicit empty plan so an older
        // successful result (or completed probe job) cannot keep advertising a removed entry.
        if (error instanceof ApiError && error.status === 404) return null;
        throw error;
      }
    },
    // 本机没有可用的拨测 Xray 时不读计划：计划只用来发起拨测和对位结果。
    enabled: capability.data?.available === true,
    retry: false,
    // Head changes precede Serving. Refresh only this user's recently edited plan, and stop
    // after a bounded window even when a node is offline or an order is halted.
    refetchInterval: () =>
      latestEdit > 0 && Date.now() < latestEdit + GRANT_REFRESH_WINDOW_MS ? GRANT_REFRESH_MS : false,
  });
  const jobKey = probeJobKey(tenant, user);
  const job =
    useQuery({
      queryKey: jobKey,
      queryFn: () => qc.getQueryData<GrantProbeJob | null>(jobKey) ?? null,
      enabled: false,
      staleTime: Infinity,
    }).data ?? null;
  const activeJobId = job?.id;
  const activeJobStatus = job?.status;

  useEffect(() => {
    if (!activeJobId || activeJobStatus !== 'running') return;
    const key = probeJobKey(tenant, user);
    let closed = false;
    let polling = false;
    const events = new EventSource(grantProbeEventsUrl(activeJobId), { withCredentials: true });
    const update = (event: Event) => {
      try {
        qc.setQueryData(key, JSON.parse((event as MessageEvent<string>).data) as GrantProbeJob);
      } catch {
        /* A malformed frame is ignored; the next snapshot is complete and catches up. */
      }
    };
    const poll = () => {
      if (closed || polling) return;
      polling = true;
      void fetchGrantProbeJob(activeJobId).then(
        snapshot => {
          if (!closed) qc.setQueryData(key, snapshot);
          polling = false;
        },
        error => {
          polling = false;
          if (closed || !(error instanceof ApiError) || error.status !== 404) return;
          // Jobs are intentionally memory-only. A Console restart forgets them; without this
          // terminal projection an already open browser would retain "running" forever.
          qc.setQueryData<GrantProbeJob | null>(key, current => {
            if (!current || current.id !== activeJobId || current.status !== 'running') return current;
            return {
              ...current,
              status: 'canceled',
              message: '拨测任务已失效，请重新发起',
              finished_at_unix_secs: Math.floor(Date.now() / 1_000),
              items: current.items.map(item =>
                item.status === 'waiting' || item.status === 'running'
                  ? { ...item, status: 'canceled', ttfb_ms: null, detail: null }
                  : item,
              ),
            };
          });
        },
      );
    };
    events.addEventListener('snapshot', update);
    events.onerror = poll;
    // SSE is the fast path. Polling is a bounded fallback for proxies that buffer an event or a
    // browser that silently loses the stream without firing a useful error.
    const timer = window.setInterval(poll, GRANT_PROBE_POLL_MS);
    return () => {
      closed = true;
      window.clearInterval(timer);
      events.close();
    };
  }, [activeJobId, activeJobStatus, qc, tenant, user]);

  const start = useMutation({
    mutationFn: (ids: string[]) => startUserGrantProbe(tenant, user, ids),
    onSuccess: response => qc.setQueryData(jobKey, response.job),
  });
  const cancel = useMutation({
    mutationFn: (id: string) => cancelGrantProbe(id),
    onSuccess: snapshot => qc.setQueryData(jobKey, snapshot),
  });

  useEffect(() => {
    if (!activeJobStatus || activeJobStatus === 'running') return;
    void qc.invalidateQueries({ queryKey: ['grant-probe-plan', tenant, user] });
  }, [activeJobStatus, qc, tenant, user]);

  // A group probe returns only the selected items. Keep the complete frozen plan and project the
  // current job state over it; otherwise every unrelated node loses its result while one group runs.
  const jobItems = new Map(job?.items.map(item => [item.id, item]) ?? []);
  const items: ProbeItem[] = (plan.data === null ? [] : (plan.data?.items ?? job?.items ?? [])).map(item => ({
    ...item,
    ...jobItems.get(item.id),
  }));
  return {
    pending: capability.isPending || (capability.data?.available === true && plan.isPending),
    /** 可以发起拨测：本机拨测组件可用，且读到了当前 Serving 计划。 */
    ready: capability.data?.available === true && plan.isSuccess,
    unavailable:
      capability.data && !capability.data.available ? capability.data.reason || 'Console 拨测组件不可用' : null,
    error: capability.error ?? plan.error ?? start.error ?? cancel.error,
    items,
    job,
    busy: job?.status === 'running' || start.isPending || cancel.isPending,
    starting: start.isPending,
    cancelling: cancel.isPending,
    start: (ids: string[]) => start.mutate(ids),
    cancel: () => {
      if (job) cancel.mutate(job.id);
    },
  };
}

const probeStatusText = (item: ProbeItem) => {
  switch (item.status) {
    case 'waiting':
      return '等待';
    case 'running':
      return '连接中…';
    case 'passed':
      return '通过';
    case 'failed':
      return item.detail ? `失败（${item.detail}）` : '失败';
    case 'canceled':
      return '已取消';
    default:
      return '尚未验证';
  }
};

/* 一组（一条链）的结果：拨测前不写，之后「拨测中」「全部通过」「通过 x / y」「失败 N」。 */
function probeResult(items: ProbeItem[]): { text: string; bad: boolean } | null {
  if (items.some(item => item.status === 'waiting' || item.status === 'running')) return { text: '拨测中', bad: false };
  const failed = items.filter(item => item.status === 'failed').length;
  if (failed > 0) return { text: `失败 ${failed}`, bad: true };
  const passed = items.filter(item => item.status === 'passed').length;
  if (passed === 0) return null;
  return { text: passed === items.length ? '全部通过' : `通过 ${passed} / ${items.length}`, bad: false };
}

/**
 * 拨测条目与节点链接取自同一组用户条目、顺序相同，但链接里没有接入点 ID，只能按名称对位。拨测条目的名称带
 * 地区旗前缀（core/physical/user.rs），链接解析时已去掉。同一链路的多个接入点名称相同，按出现顺序配对；
 * 同名的条目数与链接数不一致（有条目被 uri.txt 跳过）时无法确定对应关系，这些链接不给拨测状态，
 * 免得把别的入口的结果显示在这里。筛选只会整名去掉链接，按名称对位不受影响。
 */
function pairProbeItems(links: readonly NodeLink[], items: readonly ProbeItem[]): Map<NodeLink, ProbeItem> {
  const itemsByName = new Map<string, ProbeItem[]>();
  for (const item of items) {
    const name = splitRegion(item.name).label;
    itemsByName.set(name, [...(itemsByName.get(name) ?? []), item]);
  }
  const linksByName = new Map<string, NodeLink[]>();
  for (const link of links) linksByName.set(link.name, [...(linksByName.get(link.name) ?? []), link]);
  const paired = new Map<NodeLink, ProbeItem>();
  for (const [name, named] of linksByName) {
    const candidates = itemsByName.get(name);
    if (candidates?.length !== named.length) continue;
    named.forEach((link, index) => paired.set(link, candidates[index]));
  }
  return paired;
}

/* ── 节点链接解析 ──
 * 输入是服务端生成的 uri.txt：每行一条分享链接，# 开头的行是说明，空行分隔说明段落。
 * 名称的写法与 core/physical/user.rs 一致：可选的地区旗（两个区域指示符）+ 链名 + 协议后缀
 * （「 | VLESS Encryption」「 | QUIC」「 | AnyTLS」）+ 地址族后缀（「 | v6」）。 */

export type NodeProtocol = 'vless' | 'vless-encryption' | 'anytls' | 'hysteria2' | 'mtproto' | 'other';
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
const WIRE_SUFFIXES = [' | VLESS Encryption', ' | QUIC', ' | AnyTLS', ' | MTProxy'];

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

function parseNodeUriParts(line: string): { endpoint: string; params: URLSearchParams } {
  const schemeAt = line.indexOf('://');
  if (schemeAt < 0) return { endpoint: '—', params: new URLSearchParams() };

  const authorityStart = schemeAt + 3;
  const hashAt = line.indexOf('#', authorityStart);
  const queryAt = line.indexOf('?', authorityStart);
  const pathAt = line.indexOf('/', authorityStart);
  const authorityEnd = [pathAt, queryAt, hashAt]
    .filter(index => index >= 0)
    .reduce((earliest, index) => Math.min(earliest, index), line.length);
  const authority = line.slice(authorityStart, authorityEnd);
  const userInfoAt = authority.lastIndexOf('@');
  const endpoint = authority.slice(userInfoAt + 1) || '—';
  const hasQuery = queryAt >= 0 && (hashAt < 0 || queryAt < hashAt);
  const params = hasQuery
    ? new URLSearchParams(line.slice(queryAt + 1, hashAt >= 0 ? hashAt : line.length))
    : new URLSearchParams();

  if (line.slice(0, schemeAt).toLowerCase() === 'tg' && authority === 'proxy') {
    const server = params.get('server') ?? '—';
    const port = params.get('port');
    const host = server.includes(':') && !server.startsWith('[') ? `[${server}]` : server;
    return { endpoint: port ? `${host}:${port}` : host, params };
  }

  return { endpoint, params };
}

function parseNodeLink(line: string, metadataName?: string): NodeLink {
  const hashAt = line.indexOf('#');
  let fragment = hashAt >= 0 ? line.slice(hashAt + 1) : '';
  try {
    fragment = decodeURIComponent(fragment);
  } catch {
    // 名称不是合法的百分号编码时按原文显示。
  }
  if (!fragment && metadataName) fragment = metadataName;
  const { region, label } = splitRegion(fragment);
  const scheme = line.slice(0, Math.max(0, line.indexOf('://'))).toLowerCase();
  // Hy2 把端口跳跃范围写在 authority 中（如 `host:50000-50009`），这是它的
  // 合法 URI 形式，但 WHATWG URL 会将它拒绝为非法端口。按订阅 URI 的分隔符取地址和查询参数。
  const { endpoint, params } = parseNodeUriParts(line);
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
  } else if (scheme === 'tg' && line.startsWith('tg://proxy?')) {
    protocol = 'mtproto';
    stack = params.get('secret')?.startsWith('dd') ? 'Padded intermediate' : 'MTProto';
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
  let metadataName: string | undefined;
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
      const metadata = /^#\s*@brocade-name=(.*)$/.exec(line);
      if (metadata) {
        try {
          metadataName = decodeURIComponent(metadata[1]);
        } catch {
          metadataName = metadata[1];
        }
      } else {
        block.push(stripRegions(line.replace(/^#\s?/, '')).trim());
      }
    } else {
      flush();
      links.push(parseNodeLink(line, metadataName));
      metadataName = undefined;
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

const TOKEN_MASK = '••••';

/* 订阅地址在一行里只露主机名与 Token 末四位：路径收成「…/」，Token 换成四个圆点，clash.yaml 与筛选参数原样保留。
 * 地址不是预期形状时返回 null，原样显示。 */
function maskSubscriptionUrl(url: string): { head: string; tail: string } | null {
  const match = /\/sub\/v1\/(?:haitun\/)?([^/?#]+)(\/clash\.yaml.*)$/.exec(url);
  if (!match) return null;
  try {
    return { head: `${new URL(url).host}/…/`, tail: `${match[1].slice(-4)}${match[2]}` };
  } catch {
    return null;
  }
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

/* 日期按 UTC+8 显示，跨年时带年份。测速链接的时间是 PostgreSQL 的 timestamptz::text
 * （「2026-08-28 12:00:00.123456+00」）：Safari 不认空格分隔、六位小数与两位时区，先规整成 ISO 8601。 */
function formatDay(value: string | null | undefined): string | null {
  if (!value) return null;
  const iso = value
    .trim()
    .replace(' ', 'T')
    .replace(/(\.\d{3})\d+/, '$1')
    .replace(/([+-]\d{2})$/, '$1:00');
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return null;
  const yearOf = (at: Date) =>
    new Intl.DateTimeFormat('en', { timeZone: 'Asia/Hong_Kong', year: 'numeric' }).format(at);
  return new Intl.DateTimeFormat('zh-CN', {
    timeZone: 'Asia/Hong_Kong',
    ...(yearOf(date) === yearOf(new Date()) ? {} : { year: 'numeric' as const }),
    month: 'numeric',
    day: 'numeric',
  }).format(date);
}
