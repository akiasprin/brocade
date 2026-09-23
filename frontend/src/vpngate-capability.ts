import { MIN_AGENT_PROTOCOL_VERSION, type NodeAgentStateItem } from './api';

export interface VpngateNodeEligibility {
  eligible: boolean;
  version: string | null;
  reason: string;
}

/**
 * One shared UI interpretation of the server facts used by the release gate. OpenVPN is not a
 * permanent service: an eligible node merely has a current, executable binary and starts one
 * process per selected VPN Gate pool on demand.
 */
export function vpngateNodeEligibility(node: NodeAgentStateItem | undefined): VpngateNodeEligibility {
  if (!node) return { eligible: false, version: null, reason: '机器状态尚未加载' };
  if (node.lifecycle_phase && node.lifecycle_phase !== 'active') {
    return { eligible: false, version: null, reason: '机器不在 active 生命周期' };
  }
  if (node.operationally_isolated) {
    return { eligible: false, version: null, reason: '机器已隔离' };
  }
  if (node.agent_protocol_version == null || node.agent_protocol_version < MIN_AGENT_PROTOCOL_VERSION) {
    const current = node.agent_protocol_version == null ? '未知' : `v${node.agent_protocol_version}`;
    return {
      eligible: false,
      version: null,
      reason: `Agent 协议 ${current}，需要升级到 v${MIN_AGENT_PROTOCOL_VERSION} 或更高版本`,
    };
  }
  if (!node.runtime_report_fresh) {
    return { eligible: false, version: null, reason: '最近 2 分钟没有运行时上报' };
  }
  const version = node.runtime_versions?.openvpn?.trim() || null;
  if (!version) {
    return { eligible: false, version: null, reason: '未安装或无法执行 OpenVPN' };
  }
  return { eligible: true, version, reason: 'OpenVPN 按需启动' };
}
