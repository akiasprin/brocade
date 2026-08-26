import { useEffect, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { fetchArtifactContent, fetchClashSubscription, fetchRevisions, type ArtifactFamily } from '../api';
import { ErrorBox, Loading } from '../ui/bits';
import { copyText } from '../ui/platform';

export type SubscriptionKind = 'uri' | 'clash';

type FamilyPick = 'both' | ArtifactFamily;

const FAMILY_PICKS: { key: FamilyPick; label: string }[] = [
  { key: 'both', label: 'IPv4 + IPv6' },
  { key: 'v4', label: '仅 IPv4' },
  { key: 'v6', label: '仅 IPv6' },
];

export function SubscriptionViewer({
  tenant,
  user,
  kind,
  onClose,
}: {
  tenant: string;
  user: string;
  kind: SubscriptionKind;
  onClose: () => void;
}) {
  return kind === 'clash' ? (
    <ClashSubscription tenant={tenant} user={user} onClose={onClose} />
  ) : (
    <VlessAddresses tenant={tenant} user={user} onClose={onClose} />
  );
}

function useEscape(onClose: () => void) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);
}

/* VLESS keeps the existing address-list interaction. Address-family narrowing is performed by
 * the server, so every tab remains an exact view of a current compiled artifact. */
function VlessAddresses({ tenant, user, onClose }: { tenant: string; user: string; onClose: () => void }) {
  const [family, setFamily] = useState<FamilyPick>('both');
  useEscape(onClose);

  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions(), refetchInterval: 10_000 });
  const revision = revisions.data?.current_revision;
  const content = useQuery({
    queryKey: ['artifact', revision, 'user', `${tenant}:${user}`, 'uri', family],
    queryFn: () =>
      fetchArtifactContent('user', `${tenant}:${user}`, 'uri', undefined, family === 'both' ? undefined : family),
    enabled: !!revision,
  });

  const text = content.data?.content ?? '';
  const lines = text ? text.split('\n') : [];
  const empty = text
    .split('\n')
    .filter(line => line.trim() !== '')
    .every(line => line.startsWith('#'));

  return (
    <div className="confirm-mask" onClick={onClose}>
      <div className="sub-card" onClick={event => event.stopPropagation()} role="dialog" aria-modal="true">
        <SubscriptionHead user={user} suffix="uri.txt" onClose={onClose} />
        <div className="cfg-main">
          <div className="sub-tabbar">
            <div className="sub-tabs" role="tablist" aria-label="地址族">
              {FAMILY_PICKS.map(pick => (
                <button
                  key={pick.key}
                  type="button"
                  role="tab"
                  aria-selected={family === pick.key}
                  onClick={() => setFamily(pick.key)}
                >
                  {pick.label}
                </button>
              ))}
            </div>
            {text && (
              <button className="sub-copy" onClick={() => void copyText(text)}>
                复制
              </button>
            )}
          </div>
          {!revision || content.isPending ? (
            <Loading />
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
                {family !== 'both' && empty && (
                  <>
                    <span className="cfg-sp" />
                    <span className="st st-warn">这个人名下没有 {family === 'v4' ? 'IPv4' : 'IPv6'} 接入面</span>
                  </>
                )}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function ClashSubscription({ tenant, user, onClose }: { tenant: string; user: string; onClose: () => void }) {
  const [family, setFamily] = useState<FamilyPick>('both');
  const [revealed, setRevealed] = useState(false);
  useEscape(onClose);
  const subscription = useQuery({
    queryKey: ['clash-subscription', tenant, user],
    queryFn: () => fetchClashSubscription(tenant, user),
    staleTime: 0,
  });
  const value = subscription.data;
  const selectedUrl = value?.urls[family] ?? '';
  const shownUrl = selectedUrl ? (revealed ? selectedUrl : maskSubscriptionUrl(selectedUrl)) : '';

  return (
    <div className="confirm-mask" onClick={onClose}>
      <div
        className="sub-card clash-card"
        onClick={event => event.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={`${user} 的 Clash 订阅地址`}
      >
        <SubscriptionHead user={user} suffix="Clash" onClose={onClose} />
        {subscription.isPending ? (
          <div className="clash-state">
            <Loading />
          </div>
        ) : subscription.error ? (
          <div className="clash-state">
            <ErrorBox error={subscription.error} />
          </div>
        ) : value ? (
          <div className="clash-body">
            <div className="clash-intro">
              <span className="clash-mark">CLASH</span>
              <span>
                <b>Clash 订阅地址</b>
                <span>选择客户端可用的地址族，再复制到 Mihomo / Clash Meta</span>
              </span>
            </div>
            <div className="clash-family">
              <div className="sub-tabs" role="tablist" aria-label="订阅地址族">
                {FAMILY_PICKS.map(pick => (
                  <button
                    key={pick.key}
                    type="button"
                    role="tab"
                    aria-selected={family === pick.key}
                    onClick={() => setFamily(pick.key)}
                  >
                    {pick.label}
                  </button>
                ))}
              </div>
            </div>
            <div>
              <div className="clash-url-label">
                <b>订阅 URL</b>
                <span>地址中的 UUID 是访问凭据</span>
              </div>
              <div className="clash-url">
                <code title={revealed ? selectedUrl : undefined}>{shownUrl}</code>
                <button type="button" onClick={() => setRevealed(current => !current)}>
                  {revealed ? '隐藏' : '显示'}
                </button>
                <button type="button" onClick={() => void copyText(selectedUrl)}>
                  复制
                </button>
              </div>
            </div>
            <div className="clash-meta">
              <span>
                <small>剩余流量</small>
                <b>{value.remaining_bytes === null ? '不限量' : formatBytes(value.remaining_bytes)}</b>
              </span>
              <span>
                <small>重置时间</small>
                <b>{formatResetAt(value.reset_at)}</b>
              </span>
              {value.usage_has_gap && <span className="st st-warn">用量有缺口</span>}
            </div>
            <div className="clash-note">
              <i>●</i>
              <span>
                每次获取都按当前授权与全局 {value.template} 实时生成；<b>不保存结果，不使用缓存。</b>换 UUID
                后旧地址立即失效。
              </span>
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}

function SubscriptionHead({ user, suffix, onClose }: { user: string; suffix: string; onClose: () => void }) {
  return (
    <div className="sub-head">
      <span className="fw-kind">订阅</span>
      <span className="sub-title">
        {user}
        <span className="dim"> · {suffix}</span>
      </span>
      <button title="关闭" onClick={onClose}>
        ✕
      </button>
    </div>
  );
}

function maskSubscriptionUrl(url: string): string {
  return url.replace(/(\/sub\/v1\/)([^/]+)(\/clash\.yaml)/, (_match, prefix: string, token: string, suffix: string) => {
    const tail = token.slice(-4);
    return `${prefix}••••••••-••••-••••-••••-••••••••${tail}${suffix}`;
  });
}

function formatBytes(bytes: number): string {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
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
