import type { CvmMonitoringSample } from '../../api';

export function cvmNetworkRate(
  sample: CvmMonitoringSample | null | undefined,
  direction: 'rx_bps' | 'tx_bps',
  id = '',
) {
  const nics = sample?.interfaces.filter(nic => !id || nic.id === id);
  if (!nics?.length || nics.some(nic => nic[direction] === null)) return null;
  return nics.reduce((sum, nic) => sum + nic[direction]!, 0);
}

export const cvmEventLabel = (kind: string) =>
  ({
    reboot: 'Guest 重启',
    oom: 'OOM 终止',
    guest_unreachable: '采样通道无响应',
    guest_recovered: '采样通道恢复',
    service_unreachable: '服务连接失败',
    service_recovered: '服务连接恢复',
    create: '创建',
    start: '开机',
    stop: '关机',
    restart: '重启',
    configure: '修改配置',
    configure_network: '配置出口',
    attach: '挂载网卡',
    detach: '卸载网卡',
    reinstall: '重装',
    delete: '删除',
    retry: '核查重试',
  })[kind] ?? '实例操作';
