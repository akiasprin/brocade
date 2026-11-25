import type { QueryClient } from '@tanstack/react-query';
import { randomKey } from '../../ui/platform';
import { type CvmCapabilities, type CvmHost, type CvmInstance, type CvmStatus, type CvmWorkspace } from '../../api';

export const QUERY = ['cvm'] as const;
export const ENTRY_BOARD = ['cvm-entries'] as const;
export const MIB = 1024 ** 2;
export const GIB = 1024 ** 3;
export const requestId = randomKey;
export const hasPendingCvmChange = (instance: CvmInstance) =>
  instance.reserved && instance.revision_id != null && instance.revision_id > (instance.published_revision_id ?? 0);
export const operationText: Record<string, string> = {
  entries: '修改访问入口',
  create: '创建容器',
  adopt: '纳管裸机',
  start: '开机',
  stop: '关机',
  restart: '重启',
  reinstall: '重装系统',
  configure: '修改配置',
  assign: '使用授权',
  delete: '删除容器',
  release_host: '解除纳管',
  force_node_retirement: '随机器强制退役',
  detach: '卸载网卡',
  routes: '修改路由',
  mount: '挂载网卡',
  network_edit: '修改入口、网卡与路由',
  reset_password: '重置登录密码',
  nic_version: '更新网卡规格',
  nic_transport: '更换网卡连接方式',
  undo_pending: '撤回变更',
};
export const supportsGuestTerminal = (caps: CvmCapabilities | undefined) =>
  caps?.guest_terminal === true && caps.guest_terminal_persistent === true;

/** Detail header and board inspector expose the same terminal availability. */
export function guestTerminalAccess(instance: CvmInstance, host: CvmHost | undefined, canWrite: boolean) {
  const running =
    instance.report?.power === 'running' && (instance.desired.power === 'running' || hasPendingCvmChange(instance));
  const supported = supportsGuestTerminal(host?.capabilities);
  return {
    enabled: canWrite && running && supported && !!host?.terminal_enabled,
    reason: !canWrite
      ? '当前账号无权连接终端'
      : !host?.terminal_enabled
        ? '该宿主的 WebShell 已关闭，可在宿主页签开启'
        : !supported
          ? '宿主运行栈尚不支持终端'
          : !running
            ? '运行中的实例才能连接终端'
            : '连接实例终端',
  };
}
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
};
export const powerText = { running: '运行', stopped: '停止', deleted: '删除' };

// Mutations can change both the resource ledger and the global release revision.
export function invalidateCvmChanges(client: QueryClient) {
  for (const key of [
    ['cvm'],
    ['cvm-nic-state'],
    ['cvm-nic-catalog'],
    ['cvm-entries'],
    ['cvm-instance-entries'],
    ['cvm-chain-egress'],
    ['revisions'],
    ['plan'],
    ['plan-artifacts'],
    ['deployment-verify'],
  ])
    void client.invalidateQueries({ queryKey: key });
}
