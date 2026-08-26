import type { HopIr } from '../topo/model';

export interface HopPathCopy {
  endpointLabel: string;
  badge: string;
  detail: string;
  plaintextDetail: string;
  publicNetwork: boolean;
}

const HOP_PATH_COPY = {
  overlay: {
    endpointLabel: '目标',
    badge: 'overlay',
    detail: '走 WireGuard',
    plaintextDetail: '外层 WireGuard 已加密',
    publicNetwork: false,
  },
  direct: {
    endpointLabel: '目标',
    badge: 'direct',
    detail: '绕过 overlay，走公网',
    plaintextDetail: '走公网，UUID 和目标地址是明文的',
    publicNetwork: true,
  },
  reverse: {
    endpointLabel: '反向拨入',
    badge: 'reverse',
    detail: '下游拨入上游，流量沿隧道反向传输',
    plaintextDetail: '公网反向接入，UUID 和拨入地址是明文的',
    publicNetwork: true,
  },
} satisfies Record<HopIr['path'], HopPathCopy>;

export function hopPathCopy(path: HopIr['path']): HopPathCopy {
  return HOP_PATH_COPY[path];
}
