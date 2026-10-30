// Mockup data: 机器详情「运行状态」面板的读数。Synthetic only; never imported by the app.
// 字段、顺序与生产 RuntimeCard（src/panes/nodes.tsx）一致；判定文案取自 runtimeFindings。
import type { ReactNode } from 'react';
import type { IconName } from '../../src/ui/icons';

export type Scene = 'issues' | 'calm';
export type ItemState = 'present' | 'disabled' | 'dirty' | 'unknown';

export interface Reading {
  label: string;
  value: string;
  /** 同一字段的第二个取值：核数、虚拟化、架构、修订号 */
  sub?: string;
  unit?: string;
  /** 附加读数，例如上报积压后的「丢弃 0」 */
  tail?: string;
  hot?: boolean;
  /** 哈希一类的标识，保留等宽 */
  code?: boolean;
  dim?: boolean;
  link?: boolean;
  /** 生产 8 栏网格的跨栏 */
  span?: 'w2' | 'w3';
  /** 规格表方案 4 栏网格的跨栏 */
  wide?: boolean;
}

export interface StateItem {
  name: string;
  /** 生产状态片上的代号 */
  code: string;
  state: ItemState;
}

export interface Source {
  id: 'agent' | 'host' | 'config';
  name: string;
  code: string;
  icon: IconName;
  ago: string;
  verb: string;
  readings: Reading[];
  artifacts?: StateItem[];
  extensions?: StateItem[];
}

export interface Finding {
  tone: 'warn' | 'bad';
  body: ReactNode;
}

export const STATE_WORD: Record<ItemState, string> = {
  present: '已应用',
  disabled: '已关闭',
  dirty: '已变化',
  unknown: '未知',
};

export const EXTENSION_WORD: Record<ItemState, string> = {
  present: '已安装',
  disabled: '未安装',
  dirty: '已变化',
  unknown: '未知',
};

// 数字与中文单位之间用窄不换行空格，与生产 Ago 相同。
const gap = ' ';

export function runtimeOf(scene: Scene): { sources: Source[]; findings: Finding[] } {
  const issues = scene === 'issues';
  const sources: Source[] = [
    {
      id: 'agent',
      name: 'Agent',
      code: 'AGENT',
      icon: 'agent',
      ago: `11${gap}秒前`,
      verb: '来拉',
      readings: [
        { label: '上次用量', value: `33${gap}秒前` },
        { label: '用量明目', value: '96', unit: '项' },
        { label: '已运行', value: '6 天 4 小时' },
        { label: '构建', value: '3f9a1c7e02b4', code: true },
        { label: '协议', value: 'v43' },
        { label: '上报积压', value: '0', unit: '条', tail: '丢弃 0' },
        { label: '状态巡检', value: '正常', dim: true },
        { label: 'IP 变更记录', value: '2', unit: '条', link: true },
      ],
    },
    {
      id: 'host',
      name: '主机',
      code: 'HOST',
      icon: 'nodes',
      ago: `23${gap}秒前`,
      verb: '上报',
      readings: [
        { label: 'CPU', value: 'AMD EPYC 7B13', sub: '4 核', span: 'w3', wide: true },
        { label: '内存', value: '4.00 GiB' },
        { label: '磁盘', value: '40.00 GiB' },
        { label: '发行版', value: 'Debian GNU/Linux 12 (bookworm)', sub: 'KVM', span: 'w3', wide: true },
        { label: '内核', value: '6.8.0-45-generic', sub: 'x86_64', span: 'w3', wide: true },
        { label: '时钟偏移', value: issues ? '+7 秒' : '+0 秒', hot: issues },
        { label: '收发缓冲', value: '64.0 MiB / 64.0 MiB', span: 'w2' },
        { label: '监听队列', value: '4,096' },
        { label: '连接表上限', value: '262,144' },
      ],
    },
    {
      id: 'config',
      name: '配置',
      code: 'CONFIG',
      icon: 'artifacts',
      ago: `2${gap}分钟前`,
      verb: '观察',
      readings: [{ label: '来自', value: '#128', sub: '修订 57' }],
      artifacts: [
        { name: 'Xray', code: 'XRAY', state: 'present' },
        { name: 'WireGuard', code: 'WG', state: issues ? 'dirty' : 'present' },
        { name: 'Phantun', code: 'PHANTUN', state: 'disabled' },
        { name: 'HY2 端口跳跃', code: 'HY2 端口跳跃', state: 'present' },
        { name: '授权同步', code: '授权同步', state: 'present' },
      ],
      extensions: [{ name: 'OpenVPN', code: 'OPENVPN', state: 'disabled' }],
    },
  ];
  const findings: Finding[] = issues
    ? [
        {
          tone: 'bad',
          body: (
            <>
              xray 低于 <b>26.4</b>：<code>geodata</code> 自动更新 2026-04-25 才进主线，这一版会<b>静默忽略</b>
              那段配置——规则库不会更新，而配置里写着每天更新。升级 xray 才能修，重新发布没用。
            </>
          ),
        },
        {
          tone: 'warn',
          body: (
            <>
              内核里没有 wireguard 模块，<code>wg-quick</code> 回落到 wireguard-go。
              <code>wg show</code> 的输出跟内核态一模一样，吞吐差一个数量级。
            </>
          ),
        },
      ]
    : [];
  return { sources, findings };
}
