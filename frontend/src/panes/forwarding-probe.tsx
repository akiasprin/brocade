import { useMutation, useQueryClient } from '@tanstack/react-query';
import {
  startForwardingProbe,
  type ForwardingEntitlement,
  type ForwardingProbe,
  type ForwardingRule,
  type ForwardingTcpPingResult,
} from '../api';
import { ErrorBox } from '../ui/bits';
import './forwarding-probe.css';

export const forwardingProbeKey = (tenant: string, user: string) => ['forwarding-tcp-pings', tenant, user] as const;
export const forwardingProbePending = (ping: ForwardingProbe) => ping.state === 'queued' || ping.state === 'running';

const LABELS: Record<ForwardingTcpPingResult['code'], string> = {
  connected: 'TCP 连通',
  'invalid-target': '目标地址无效',
  'dns-failed': 'DNS 解析失败',
  'dns-busy': 'DNS 正忙，请重试',
  'dns-empty': 'DNS 未返回地址',
  'address-blocked': '目标不符合入口的网络授权',
  'connect-failed': 'TCP 建连失败，请检查远程 TCP 监听和网络连通性',
  timeout: '超时或远程地址非 TCP 监听',
  'agent-upgrade-required': '入口 Agent 需升级后才能执行 TCP Ping',
};

export function ForwardingRuleProbe({
  rule,
  entry,
  result,
  queryError,
  canManage,
}: {
  rule: ForwardingRule;
  entry: ForwardingEntitlement | undefined;
  result: ForwardingProbe | undefined;
  queryError?: Error | null;
  canManage: boolean;
}) {
  const qc = useQueryClient();
  const key = forwardingProbeKey(rule.tenant_id, rule.user_id);
  const test = useMutation({
    mutationFn: () => startForwardingProbe(rule.tenant_id, rule.user_id, rule.id),
    onSuccess: value =>
      qc.setQueryData<ForwardingProbe[]>(key, current => [
        ...(current ?? []).filter(item => item.rule_id !== rule.id),
        value,
      ]),
  });
  const disabled =
    !rule.enabled ||
    !entry?.enabled ||
    entry.quota_suspended ||
    ['service-paused', 'disabled'].includes(rule.runtime_status);
  const current =
    result?.rule_updated_at === rule.updated_at && result.generation === entry?.desired_generation && !disabled
      ? result
      : undefined;
  const pending = !!current && forwardingProbePending(current);
  const reading = current?.state === 'complete' ? current.result : null;
  const status = !current
    ? '未测试'
    : {
        queued: '等待入口机器',
        running: '连接中…',
        complete: '',
        obsolete: '规则或授权已变化，请重新测试',
        expired: '机器未及时返回，请重试',
      }[current.state];
  return (
    <section className="fwd-tcp-ping" aria-label={`入口 ${rule.listen_port} TCP Ping`}>
      <div className="fwd-tcp-ping-row">
        <span className="fwd-dim">TCP Ping</span>
        {canManage && (
          <button
            type="button"
            className="btn"
            disabled={disabled || pending || test.isPending}
            onClick={() => test.mutate()}
          >
            {test.isPending || pending ? '连接中…' : '测试延迟'}
          </button>
        )}
        <span role="status">
          {reading ? (
            <span
              className={`fwd-st ${reading.status === 'reachable' ? 'ok' : reading.status === 'failed' ? 'bad' : 'warn'}`}
            >
              {LABELS[reading.code] ?? '未知结果，请重试'}
              {reading.connect_ms !== null && <b className="mono"> · {reading.connect_ms} ms</b>}
            </span>
          ) : (
            status
          )}
        </span>
        {reading?.dns_ms != null && <span className="fwd-dim">DNS {reading.dns_ms} ms</span>}
        {reading?.resolved_ip && <code>{reading.resolved_ip}</code>}
        {current?.state === 'complete' && (
          <time dateTime={new Date(current.created_at * 1000).toISOString()}>
            {new Date(current.created_at * 1000).toLocaleTimeString()}
          </time>
        )}
      </div>
      {test.error && <ErrorBox error={test.error} />}
      {queryError && <ErrorBox error={queryError} />}
      <p className="fwd-tcp-ping-note">
        {entry?.node_name ?? '转发入口机器'} → 目标端口 · 单次 TCP 建连，不发送业务数据，不定时重测
        {rule.protocol !== 'tcp' ? '；不代表 UDP 连通性' : ''}。
      </p>
    </section>
  );
}
