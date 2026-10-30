import type { ChainUnlockProbeItem, NodeUnlockProbe, UnlockProbeSelection } from '../api';
import { unlockFlagRegion } from '../ui/unlock-region';
import { unlockReport, type Row } from './unlock-report';
import { verdictOf, type Verdict } from './unlock-status';

const FAMILY_NAMES = { v4: 'IPv4', v6: 'IPv6', auto: '不分族' };

function status(job: NodeUnlockProbe | null): string | null {
  if (!job) return '尚未检测';
  if (job.state === 'obsolete') return '线路已变更';
  if (job.state === 'queued') return '排队中';
  if (job.state === 'running') return '检测中';
  if (job.state === 'expired') return '超时';
  if (job.path_error === 'client_unavailable') return '无法启动检测客户端';
  if (job.path_error === 'family_unavailable') return '检测未就绪';
  if (job.path_error === 'configuration_changed') return '线路已变更';
  return null;
}

const failed = (job: NodeUnlockProbe | null) => job?.state === 'expired' || !!job?.path_error;
const ipv6Skip = (row: Row) => ['no_ipv6_dns', 'no_ipv6_egress'].includes((row.result ?? row.source)?.code ?? '');
const displayRegion = (row: Row) =>
  row.region && row.service === 'gemini' ? (unlockFlagRegion(row.service, row.region) ?? row.region) : row.region;
const reading = (family: UnlockProbeSelection, row: Row, job: NodeUnlockProbe | null) =>
  [
    `${FAMILY_NAMES[family]}：${!row.result && row.source && ipv6Skip(row) ? verdictOf(row.source).label : row.verdict.label}`,
    row.verdict.note,
    row.region && `地区 ${row.region}`,
    !row.result && row.source && ipv6Skip(row) ? verdictOf(row.source).reason : row.verdict.reason,
    status(job),
    row.route === 'fallback' && '实际连接经 DNS 回退取得，不参与双栈比较',
    row.dnsPart?.kind === 'partial' && '本项部分连接发生条件性 DNS 回退，保留可用计数',
    row.route === 'excluded' && 'DNS 失败处理为停止连接，不参与双栈比较',
    row.change,
  ]
    .filter(Boolean)
    .join(' · ');

export interface ChainUnlockCell {
  service: Row['service'];
  verdict: Verdict;
  region: string | null;
  /** The primary region comes from IPv6 only when IPv4 supplied none. */
  regionFamily: UnlockProbeSelection;
  ipv6Region: string | null;
  onlyFamily: 'v4' | 'v6' | null;
  change: string | null;
  title: string;
  dim: boolean;
}

/** Preserve each family's facts; only the list's verdict and region presentation are combined. */
export function summarizeChainUnlock(item: ChainUnlockProbeItem) {
  const automatic = item.capabilities.family_mode === 'automatic';
  const selections: UnlockProbeSelection[] = automatic ? ['auto'] : ['v4', 'v6'];
  const sources = selections.map(family => ({
    family,
    job: item.probes[family],
    report: unlockReport(item.probes[family]),
  }));
  const primary = sources[0];
  const secondary = sources[1];
  const cells = primary.report.rows.map<ChainUnlockCell>((v4, index) => {
    const v6 = secondary?.report.rows[index];
    // A missing AAAA answer or IPv6 exit is not a service restriction. A new failed
    // attempt must still remain visible when its previous report happened to be skipped.
    const ignoreV4 = v4.route !== 'normal';
    const ignoreV6 =
      !!v6 && (v6.route !== 'normal' || (ipv6Skip(v6) && !secondary.report.stale && !failed(secondary.job)));
    const comparison = !!v6 && !ignoreV4 && !ignoreV6;
    const chosen = !ignoreV4 ? v4 : v6 && !ignoreV6 ? v6 : null;
    const chosenFamily = chosen === v6 ? 'v6' : primary.family;
    const issue =
      comparison &&
      (failed(primary.job) ||
        failed(secondary.job) ||
        !v4.result ||
        !v6.result ||
        v4.verdict.tone !== 'ok' ||
        v6.verdict.tone !== 'ok');
    const bothUnavailable =
      comparison &&
      !failed(primary.job) &&
      !failed(secondary.job) &&
      !!v4.result &&
      !!v6.result &&
      v4.verdict.tone !== 'ok' &&
      v6.verdict.tone !== 'ok';
    // A failed connection in the other family does not turn a subscription-region
    // notice into a denial. The explanation still retains both families' facts.
    const notice =
      bothUnavailable && (v4.result?.status === 'failed' || v6?.result?.status === 'failed')
        ? [v4, v6].find(row => row?.verdict.tone === 'info')?.verdict
        : undefined;
    const verdict: Verdict =
      notice ??
      (bothUnavailable
        ? v4.verdict.label === v6!.verdict.label
          ? v4.verdict
          : v4.verdict.tone === 'part' || v6!.verdict.tone === 'part'
            ? { tone: 'part', label: '双栈受限', note: null, reason: 'IPv4 与 IPv6 各有访问限制' }
            : { tone: 'no', label: '双栈不可用', note: null, reason: 'IPv4 与 IPv6 均未解锁' }
        : issue && (v4.result || v6?.result)
          ? { tone: 'part', label: '双栈需留意', note: null, reason: '一族未解锁、未取得结果或检测异常' }
          : (chosen?.verdict ?? {
              tone: 'unknown',
              label: 'DNS 路径不一致',
              note: null,
              reason: '两族的常规 DNS 路径没有可比较的结果，请重新检测',
            }));
    const region4 = !ignoreV4 ? displayRegion(v4) : null;
    const region6 = v6 && !ignoreV6 ? displayRegion(v6) : null;
    const regionFamily = !region4 && region6 ? 'v6' : chosenFamily;
    const onlyFamily =
      !automatic && chosen && ((ignoreV4 && v4.route !== 'normal') || (ignoreV6 && v6?.route !== 'normal'))
        ? (chosenFamily as 'v4' | 'v6')
        : null;
    const change =
      [
        !ignoreV4 && v4.change && `${FAMILY_NAMES[primary.family]} ${v4.change}`,
        !ignoreV6 && v6?.change && `IPv6 ${v6.change}`,
      ]
        .filter(Boolean)
        .join('；') || null;
    const title = [
      v4.name,
      reading(primary.family, v4, primary.job),
      v6 && reading('v6', v6, secondary.job),
      onlyFamily && `常规流量仅 ${FAMILY_NAMES[onlyFamily]}，另一族不参与双栈比较`,
      ignoreV6 &&
        v6?.route === 'normal' &&
        (v6.source?.code === 'no_ipv6_dns' ? 'IPv6 无解析地址，采用 IPv4 结果' : 'IPv6 无可用出站，采用 IPv4 结果'),
    ]
      .filter(Boolean)
      .join('；');
    return {
      service: v4.service,
      verdict,
      region: region4 ?? region6,
      regionFamily,
      ipv6Region: region4 && region6 && region4 !== region6 ? region6 : null,
      onlyFamily,
      change,
      title,
      dim: (!ignoreV4 && primary.report.stale) || (!ignoreV6 && !!secondary?.report.stale),
    };
  });
  const states = sources.map(source => status(source.job));
  const jobs = sources.flatMap(source => (source.job ? [source.job] : []));
  const obsolete = jobs.some(job => job.state === 'obsolete');
  const running = jobs.some(job => job.state === 'running' || job.state === 'queued');
  const expired = jobs.some(job => job.state === 'expired');
  const pathError = jobs.some(job => !!job.path_error);
  const notReady = jobs.some(job => job.path_error === 'family_unavailable');
  const state = obsolete
    ? '线路已变更'
    : running
      ? jobs.some(job => job.state === 'running')
        ? '检测中'
        : '排队中'
      : expired
        ? '超时'
        : notReady
          ? '检测未就绪'
          : pathError
            ? '检测异常'
            : states.some(Boolean)
              ? jobs.length
                ? '部分未检测'
                : '尚未检测'
              : null;
  const at = sources.flatMap(source => (source.report.shownAt == null ? [] : [source.report.shownAt]));
  const detail = sources
    .map(source =>
      [
        FAMILY_NAMES[source.family],
        status(source.job),
        source.job?.state === 'obsolete' &&
          source.job.configuration_revision != null &&
          `结果来自修订 ${source.job.configuration_revision}`,
        source.report.shownAt != null &&
          `显示 ${new Date(source.report.shownAt * 1000).toLocaleString('zh-CN', { hour12: false })} 的结果`,
      ]
        .filter(Boolean)
        .join(' · '),
    )
    .join('；');
  return {
    cells,
    automatic,
    state,
    detail,
    at: at.length ? Math.min(...at) : null,
    hasResults: sources.some(source => source.report.hasResults),
    tone: obsolete
      ? 'obsolete'
      : running
        ? 'running'
        : expired || pathError
          ? 'stale'
          : jobs.length === 0
            ? 'never'
            : 'fresh',
  };
}
