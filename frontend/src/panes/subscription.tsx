import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchArtifactContent,
  fetchClashSubscription,
  fetchMyArtifact,
  fetchMyClashSubscription,
  fetchRevisions,
  issueClashHaitunSubscription,
  issueMyClashHaitunSubscription,
  revokeClashHaitunSubscription,
  revokeMyClashHaitunSubscription,
  type ArtifactFamily,
  type ArtifactProtocol,
  type ClashSubscriptionInfo,
} from '../api';
import { ErrorBox, Loading } from '../ui/bits';
import { CopyButton } from '../ui/copy-button';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { bytes } from '../ui/format';
import { Icon } from '../ui/icons';

export type SubscriptionKind = 'uri' | 'clash';

type FamilyPick = 'both' | ArtifactFamily;
type ProtocolPick = 'both' | ArtifactProtocol;
type ClashTemplatePick = 'standard' | 'haitun';

const FAMILY_PICKS: { key: FamilyPick; label: string }[] = [
  { key: 'both', label: 'IPv4 + IPv6' },
  { key: 'v4', label: '仅 IPv4' },
  { key: 'v6', label: '仅 IPv6' },
];

const PROTOCOL_PICKS: { key: ProtocolPick; label: string }[] = [
  { key: 'vless', label: 'VLESS' },
  { key: 'anytls', label: 'AnyTLS' },
  { key: 'hysteria2', label: 'Hysteria 2' },
  { key: 'both', label: '全部协议' },
];

function SubscriptionTabs<T extends string>({
  label,
  ariaLabel,
  picks,
  value,
  onChange,
}: {
  label: string;
  ariaLabel: string;
  picks: { key: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
}) {
  return (
    <div className="sub-filter-row">
      <span className="sub-filter-label">{label}</span>
      <div className="sub-tabs" role="tablist" aria-label={ariaLabel}>
        {picks.map(pick => (
          <button
            key={pick.key}
            type="button"
            role="tab"
            aria-selected={value === pick.key}
            onClick={() => onChange(pick.key)}
          >
            {pick.label}
          </button>
        ))}
      </div>
    </div>
  );
}

export function SubscriptionViewer({
  tenant,
  user,
  kind,
  selfService = false,
  onClose,
}: {
  tenant: string;
  user: string;
  kind: SubscriptionKind;
  selfService?: boolean;
  onClose: () => void;
}) {
  return kind === 'clash' ? (
    <ClashSubscription tenant={tenant} user={user} selfService={selfService} onClose={onClose} />
  ) : (
    <VlessAddresses tenant={tenant} user={user} selfService={selfService} onClose={onClose} />
  );
}

/* The VLESS entry keeps the existing address-list interaction, while the server narrows both
 * protocol and address family so every tab remains an exact view of the current artifact. */
function VlessAddresses({
  tenant,
  user,
  selfService,
  onClose,
}: {
  tenant: string;
  user: string;
  selfService: boolean;
  onClose: () => void;
}) {
  const [family, setFamily] = useState<FamilyPick>('both');
  const [protocol, setProtocol] = useState<ProtocolPick>('both');
  const [allowInsecure, setAllowInsecure] = useState(false);

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

  const text = content.data?.content ?? '';
  const lines = text ? text.split('\n') : [];
  const empty = text
    .split('\n')
    .filter(line => line.trim() !== '')
    .every(line => line.startsWith('#'));

  return (
    <DialogLayer label={`${user} 的订阅地址`} onClose={onClose}>
      <div className="dialog-surface sub-card">
        <SubscriptionHead user={user} suffix="uri.txt" />
        <div className="cfg-main">
          <div className="sub-filterbar">
            <SubscriptionTabs
              label="协议"
              ariaLabel="订阅协议"
              picks={PROTOCOL_PICKS}
              value={protocol}
              onChange={value => {
                setProtocol(value);
                if (value === 'vless') setAllowInsecure(false);
              }}
            />
            <div className="sub-filter-with-action">
              <SubscriptionTabs
                label="网络"
                ariaLabel="地址族"
                picks={FAMILY_PICKS}
                value={family}
                onChange={setFamily}
              />
              {text && <CopyButton className="sub-copy" text={text} />}
            </div>
            {protocol !== 'vless' && (
              <div className={allowInsecure ? 'sub-insecure on' : 'sub-insecure'}>
                <span className="sub-insecure-icon" aria-hidden="true">
                  <svg viewBox="0 0 20 20">
                    <path d="M10 2.5 16 4.8v5c0 3.9-2.4 6.8-6 8.3-3.6-1.5-6-4.4-6-8.3v-5L10 2.5Z" />
                    <path d="M7.7 10.3h4.6M10 8v4.6" />
                  </svg>
                </span>
                <span className="sub-insecure-copy">
                  <span>允许 insecure</span>
                  <small>仅本次显示自签连接地址；客户端不会验证证书</small>
                </span>
                <button
                  type="button"
                  className="sub-insecure-toggle"
                  aria-pressed={allowInsecure}
                  aria-label="允许 insecure"
                  onClick={() => setAllowInsecure(value => !value)}
                >
                  <span aria-hidden="true" />
                </button>
              </div>
            )}
          </div>
          {!revision || content.isPending ? (
            <Loading variant="code" />
          ) : content.error ? (
            <ErrorBox error={content.error} />
          ) : (
            <>
              <div className="cfg-code">
                <div className="lnum">{lines.map((_, index) => index + 1).join('\n')}</div>
                <pre className="cd">{text}</pre>
              </div>
              <div className="cfg-foot">
                <b>{lines.length} 行</b>
                <span>·</span>
                <span>修订 {content.data.revision}</span>
                {empty && (
                  <>
                    <span className="cfg-sp" />
                    <span className="st st-warn">没有符合当前协议与网络条件的接入面</span>
                  </>
                )}
              </div>
            </>
          )}
        </div>
      </div>
    </DialogLayer>
  );
}

function ClashSubscription({
  tenant,
  user,
  selfService,
  onClose,
}: {
  tenant: string;
  user: string;
  selfService: boolean;
  onClose: () => void;
}) {
  const [family, setFamily] = useState<FamilyPick>('both');
  const [protocol, setProtocol] = useState<ProtocolPick>('both');
  const [template, setTemplate] = useState<ClashTemplatePick>('standard');
  const [revealed, setRevealed] = useState(false);
  const qc = useQueryClient();
  const queryKey = ['clash-subscription', tenant, user] as const;
  const subscription = useQuery({
    queryKey,
    queryFn: () => (selfService ? fetchMyClashSubscription() : fetchClashSubscription(tenant, user)),
    staleTime: 0,
  });
  const updateHaitun = (haitun: ClashSubscriptionInfo['haitun']) => {
    qc.setQueryData<ClashSubscriptionInfo>(queryKey, current => (current ? { ...current, haitun } : current));
  };
  const issueHaitun = useMutation({
    mutationFn: () => (selfService ? issueMyClashHaitunSubscription() : issueClashHaitunSubscription(tenant, user)),
    onSuccess: updateHaitun,
  });
  const revokeHaitun = useMutation({
    mutationFn: () => (selfService ? revokeMyClashHaitunSubscription() : revokeClashHaitunSubscription(tenant, user)),
    onSuccess: data => {
      setRevealed(false);
      updateHaitun(data);
    },
  });
  const value = subscription.data;
  const haitunActive = value?.haitun.status === 'active' && !!value.haitun.urls;
  const selectedUrls = template === 'haitun' ? value?.haitun.urls : value?.urls;
  const selectedUrl = withSubscriptionProtocol(selectedUrls?.[family] ?? '', protocol);
  const shownUrl = selectedUrl ? (revealed ? selectedUrl : maskSubscriptionUrl(selectedUrl)) : '';
  const actionError = issueHaitun.error ?? revokeHaitun.error;

  return (
    <DialogLayer label={`${user} 的 Clash 订阅地址`} onClose={onClose}>
      <div className="dialog-surface sub-card clash-card">
        <SubscriptionHead user={user} suffix="Clash" />
        {subscription.isPending ? (
          <div className="clash-state">
            <Loading variant="code" />
          </div>
        ) : subscription.error ? (
          <div className="clash-state">
            <ErrorBox error={subscription.error} />
          </div>
        ) : value ? (
          <div className="clash-body">
            <div className="clash-intro">
              <span className="clash-mark">{template === 'haitun' ? 'KOI' : 'CLASH'}</span>
              <span>
                <b>{template === 'haitun' ? 'koipy 测速订阅' : 'Clash 订阅地址'}</b>
                <span>
                  {template === 'haitun'
                    ? '精简为 koipy 测速需要的具体节点与必要链路'
                    : '选择客户端可用的地址族，再复制到 Mihomo / Clash Meta'}
                </span>
              </span>
            </div>
            <label className="clash-template">
              <span>订阅模板</span>
              <select
                aria-label="订阅模板"
                value={template}
                onChange={event => {
                  setTemplate(event.target.value as ClashTemplatePick);
                  setRevealed(false);
                }}
              >
                <option value="standard">{value.template}</option>
                <option value="haitun">{value.haitun.template}</option>
              </select>
              <small>{template === 'haitun' ? '供 koipy 测速拉取' : '日常客户端配置'}</small>
            </label>
            <div className="clash-filters">
              <SubscriptionTabs
                label="协议"
                ariaLabel="订阅协议"
                picks={PROTOCOL_PICKS}
                value={protocol}
                onChange={next => {
                  setProtocol(next);
                  setRevealed(false);
                }}
              />
              <SubscriptionTabs
                label="网络"
                ariaLabel="订阅地址族"
                picks={FAMILY_PICKS}
                value={family}
                onChange={next => {
                  setFamily(next);
                  setRevealed(false);
                }}
              />
            </div>
            {template === 'haitun' && !haitunActive ? (
              <div className="clash-haitun-empty">
                <span className={value.haitun.status === 'revoked' ? 'st st-warn' : 'st'}>
                  {value.haitun.status === 'revoked' ? '已撤销' : '尚未生成'}
                </span>
                <span>
                  <b>{value.haitun.status === 'revoked' ? '旧 koipy 测速地址已经失效' : '生成独立的 koipy 测速地址'}</b>
                  <small>不会修改或替换当前的标准 Clash 订阅。</small>
                </span>
                <button type="button" disabled={issueHaitun.isPending} onClick={() => issueHaitun.mutate()}>
                  {issueHaitun.isPending
                    ? '生成中…'
                    : value.haitun.status === 'revoked'
                      ? '重新生成'
                      : '生成 koipy 测速地址'}
                </button>
              </div>
            ) : (
              <div>
                <div className="clash-url-label">
                  <b>{template === 'haitun' ? 'koipy 测速 URL' : '订阅 URL'}</b>
                  <span>{template === 'haitun' ? '独立 Token，可单独撤销' : '地址中的 UUID 是访问凭据'}</span>
                  {template === 'haitun' && (
                    <>
                      <span className="sp" />
                      <span className="clash-live">● 可用</span>
                      <button
                        className="clash-revoke"
                        type="button"
                        disabled={revokeHaitun.isPending}
                        onClick={() => revokeHaitun.mutate()}
                      >
                        {revokeHaitun.isPending ? '撤销中…' : '撤销 koipy 测速地址'}
                      </button>
                    </>
                  )}
                </div>
                <div className="clash-url">
                  <code title={revealed ? selectedUrl : undefined}>{shownUrl}</code>
                  <button type="button" onClick={() => setRevealed(current => !current)}>
                    {revealed ? '隐藏' : '显示'}
                  </button>
                  <CopyButton text={selectedUrl} />
                </div>
              </div>
            )}
            {template === 'haitun' && actionError && <ErrorBox error={actionError} />}
            <div className="clash-meta">
              <span>
                <small>剩余流量</small>
                <b>{value.remaining_bytes === null ? '不限量' : bytes(value.remaining_bytes)}</b>
              </span>
              <span>
                <small>重置时间</small>
                <b>{formatResetAt(value.reset_at)}</b>
              </span>
            </div>
            <div className="clash-note">
              <i>{template === 'haitun' ? '!' : '●'}</i>
              {template === 'haitun' ? (
                <span>
                  撤销后 koipy 测速无法再次获取；已经下载的节点凭据仍需通过<b>换 UUID</b>失效。
                </span>
              ) : (
                <span>
                  每次获取都按当前授权与全局 {value.template} 实时生成；<b>不保存结果，不使用缓存。</b>换 UUID
                  后旧地址立即失效。
                </span>
              )}
            </div>
          </div>
        ) : null}
      </div>
    </DialogLayer>
  );
}

function SubscriptionHead({ user, suffix }: { user: string; suffix: string }) {
  return (
    <div className="sub-head">
      <span className="fw-kind">订阅</span>
      <span className="sub-title">
        {user}
        <span className="dim"> · {suffix}</span>
      </span>
      <DialogClose title="关闭" aria-label="关闭">
        <Icon of="close" size={14} />
      </DialogClose>
    </div>
  );
}

function maskSubscriptionUrl(url: string): string {
  return url.replace(
    /(\/sub\/v1\/(?:haitun\/)?)([^/]+)(\/clash\.yaml)/,
    (_match, prefix: string, token: string, suffix: string) => {
      const tail = token.slice(-4);
      return `${prefix}••••••••-••••-••••-••••-••••••••${tail}${suffix}`;
    },
  );
}

function withSubscriptionProtocol(url: string, protocol: ProtocolPick): string {
  if (!url || protocol === 'both') return url;
  const selected = new URL(url);
  selected.searchParams.set('protocol', protocol);
  return selected.toString();
}

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
  }).format(date)} +08`;
}
