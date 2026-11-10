import { randomKey } from '../../ui/platform';
import { type CvmCapabilities, type CvmHost, type CvmStatus, type CvmWorkspace } from '../../api';

export const QUERY = ['cvm'] as const;
export const MIB = 1024 ** 2;
export const GIB = 1024 ** 3;
export const requestId = randomKey;
export const supportsGuestTerminal = (caps: CvmCapabilities | undefined) =>
  caps?.guest_terminal === true && caps.guest_terminal_persistent === true;
// Only machines with the container stack report an execution backend; ordinary
// proxy machines report an empty one and are never container hosts.
export const hasStack = (host: CvmHost) => !!host.capabilities.backend;
/** Container hosts: the stack is installed, or a reserved container still lives there. */
export const stackHosts = (workspace: CvmWorkspace) =>
  workspace.hosts.filter(
    host =>
      hasStack(host) || workspace.instances.some(instance => instance.reserved && instance.node_id === host.node_id),
  );
export const supportsCreate = (host: CvmHost) =>
  !host.error &&
  host.fresh &&
  host.capabilities.create &&
  host.capabilities.managed_networks &&
  !!host.capabilities.resource_limits;
export const capacity = (bytes: number) =>
  bytes >= GIB ? `${Number((bytes / GIB).toFixed(2))} GiB` : `${Number((bytes / MIB).toFixed(1))} MiB`;
export const ownerKey = (tenant: string, user: string) => JSON.stringify([tenant, user]);
export const statusText: Record<CvmStatus, string> = {
  pending: '待执行',
  applied: '已应用',
  failed: '执行失败',
  unknown: '待核查',
  restart_required: '需停机应用',
};
export const powerText = { running: '运行', stopped: '停止', deleted: '删除' };
