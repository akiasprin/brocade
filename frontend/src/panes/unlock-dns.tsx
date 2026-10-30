import { Fragment, useRef, useState } from 'react';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { Icon } from '../ui/icons';
import { egressDnsEndpointText, egressDnsMembers, type DestMatch, type UnlockDnsRule } from '../api';
import type { Row } from './unlock-report';
import { connectionFailureReason } from './unlock-dial';

const STRATEGY = {
  use_ip: 'UseIP',
  use_ipv4: 'UseIPv4',
  use_ipv6: 'UseIPv6',
  use_ipv4v6: 'UseIPv4v6',
  use_ipv6v4: 'UseIPv6v4',
};
const selectorText = (selector: DestMatch) =>
  'v' in selector
    ? `${({ domain_suffix: 'domain', domain_keyword: 'keyword', domain_regex: 'regexp' } as Record<string, string>)[selector.t] ?? selector.t}:${Array.isArray(selector.v) ? selector.v.join(', ') : String(selector.v)}`
    : selector.t;

export function DnsRuleMembers({ rule }: { rule: UnlockDnsRule }) {
  if (!rule.members?.length) return null;
  const addresses = egressDnsMembers(rule.resolution.address);
  return (
    <small className="unlock-dns-members">
      {rule.members.map(member => (
        <span key={member.index}>
          {rule.selected_member === member.index ? '采用 · ' : ''}
          {addresses[member.index - 1] ?? `成员 ${member.index}`} ·{' '}
          {member.outcome === 'answer' ? '有地址' : member.outcome === 'empty' ? '无该族地址' : '请求失败'}
          {' · '}
          {member.attempts} 次 · {member.elapsed_ms}ms
          {member.cache === 'hit' ? ' · 缓存命中' : member.cache === 'stale' ? ' · 旧缓存' : ''}
          {member.rcode ? ` · RCODE ${member.rcode}` : ''}
        </span>
      ))}
    </small>
  );
}

export function UnlockDnsDetails({ row }: { row: Row }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const evidence = row.dnsPart;
  if (!evidence || ((evidence.kind === 'direct' || evidence.kind === 'unknown') && !evidence.paths.length)) return null;
  const family = row.family === 'v4' ? 'IPv4' : 'IPv6';
  const normalFamilies = [
    ...new Set(
      evidence.paths.flatMap(path => (path.normal_family ? [path.normal_family === 'v4' ? 'IPv4' : 'IPv6'] : [])),
    ),
  ];
  const stopped = evidence.paths.length > 0 && evidence.paths.every(path => path.fallback === 'stop');
  const tag =
    evidence.kind === 'partial'
      ? '部分回退'
      : evidence.kind === 'unknown'
        ? stopped
          ? '解析受限'
          : '回退待确认'
        : '回退';
  const label = `${row.name} · ${evidence.kind === 'fallback' ? '回退结果' : tag}的解析路径`;
  const connections = [
    ...new Map(
      (row.source?.connection_observations ?? [])
        .filter(connection => connection.part === evidence.part)
        .map(connection => [
          `${connection.domain}|${connection.exit}|${connection.family}|${connection.connected}|${connection.complete}`,
          connection,
        ]),
    ).values(),
  ];
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={row.route === 'excluded' ? 'unlock-info' : 'unlock-dns-tag'}
        aria-label={label}
        aria-expanded={open}
        onClick={event => {
          event.stopPropagation();
          setOpen(value => !value);
        }}
      >
        {row.route === 'excluded' ? <Icon of="info" size={14} /> : tag}
      </button>
      {open && (
        <DialogLayer mode="popover" anchor={anchor} align="end" label={label} onClose={() => setOpen(false)}>
          <section className="dialog-surface unlock-yt unlock-dns-path" aria-label={label}>
            <DialogClose className="sub-dialog-close unlock-yt-close" aria-label="关闭解析路径">
              <Icon of="close" size={14} />
            </DialogClose>
            <h3>
              {row.name}
              <small>{family} 解析路径</small>
            </h3>
            <dl>
              <div>
                <dt>常规流量</dt>
                <dd>
                  {evidence.kind === 'unknown'
                    ? '未完全确定'
                    : normalFamilies.length
                      ? `${evidence.kind === 'partial' ? '已确认部分走 ' : ''}${normalFamilies.join(' / ')}`
                      : '未能确定'}
                  <small>
                    {evidence.kind === 'partial'
                      ? '不同连接的解析路径不同'
                      : evidence.kind === 'unknown'
                        ? '常规地址族尚未确认，保留这一族的结果'
                        : evidence.kind === 'direct'
                          ? '常规出站策略依赖此回退路径'
                          : '已确认规则 DNS 可返回该族地址'}
                  </small>
                </dd>
              </div>
              <div>
                <dt>{family} 结果</dt>
                <dd>
                  {row.route === 'excluded'
                    ? '停止连接，不回退'
                    : evidence.kind === 'partial'
                      ? '部分连接经 DNS 回退取得'
                      : evidence.kind === 'unknown'
                        ? stopped
                          ? '规则要求停止连接'
                          : 'DNS 回退已确认，常规地址族尚未确认'
                        : '经 DNS 回退取得'}
                  <small>
                    {row.route === 'fallback' || row.route === 'excluded'
                      ? '不计入可用数和双栈比较'
                      : '保留可用计数和双栈比较'}
                  </small>
                </dd>
              </div>
              {evidence.paths.map((path, pathIndex) => (
                <Fragment key={`${path.exit}:${pathIndex}`}>
                  <div>
                    <dt>解析出口</dt>
                    <dd>
                      {path.exit}
                      <small>
                        {path.answer_from === 'policy'
                          ? '后续自定义 DNS 返回地址'
                          : path.answer_from === 'mixed'
                            ? '后续自定义／机器 DNS 返回地址'
                            : path.fallback === 'stop'
                              ? '规则要求停止连接'
                              : '机器 DNS 返回地址'}
                      </small>
                    </dd>
                  </div>
                  {path.reason && (
                    <div>
                      <dt>原因</dt>
                      <dd>
                        {path.reason === 'resolver_error' ? '规则 DNS 请求失败' : `规则 DNS 未返回 ${family} 地址`}
                      </dd>
                    </div>
                  )}
                  {path.rules.map((rule, index) => (
                    <Fragment key={index}>
                      <div>
                        <dt>DNS 策略{path.rules.length > 1 ? ` ${index + 1}` : ''}</dt>
                        <dd>
                          <code>{selectorText(rule.selector)}</code>
                        </dd>
                      </div>
                      <div>
                        <dt>解析器</dt>
                        <dd>
                          <code>{egressDnsEndpointText(rule.resolution)}</code>
                          <span className="unlock-yt-dim">{rule.resolution.transport.toUpperCase()}</span>
                          <DnsRuleMembers rule={rule} />
                        </dd>
                      </div>
                      <div>
                        <dt>地址策略</dt>
                        <dd>{STRATEGY[rule.resolution.address_strategy]}</dd>
                      </div>
                      <div>
                        <dt>失败处理</dt>
                        <dd>{rule.resolution.fallback === 'stop' ? '停止连接' : '继续后续 DNS（含机器 DNS）'}</dd>
                      </div>
                    </Fragment>
                  ))}
                </Fragment>
              ))}
              {!evidence.paths.length && evidence.exits.length > 0 && (
                <div>
                  <dt>连接出口</dt>
                  <dd>{evidence.exits.join('、')}</dd>
                </div>
              )}
              {connections.slice(0, 16).map((connection, index) => (
                <div key={index}>
                  <dt>实际连接{connections.length > 1 ? ` ${index + 1}` : ''}</dt>
                  <dd>
                    <code>{connection.domain}</code>
                    <small>
                      {evidence.exits.includes(connection.exit) ? connection.exit : '出口未验证'} ·{' '}
                      {connection.connected
                        ? `已连接 ${connection.family === 'v4' ? 'IPv4' : 'IPv6'}`
                        : connection.error === 'dns_empty'
                          ? '无解析地址，未建立连接'
                          : (connectionFailureReason(connection) ?? '未建立连接')}
                    </small>
                  </dd>
                </div>
              ))}
              <div>
                <dt>依据</dt>
                <dd>
                  本次实际连接的 DNS 解析与拨号
                  <small>已按连接编号关联，并核对出口身份、DNS 与出站配置及检测修订。</small>
                </dd>
              </div>
            </dl>
          </section>
        </DialogLayer>
      )}
    </>
  );
}
