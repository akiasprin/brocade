import type { CvmHost, CvmImage, CvmInstance, CvmResources, CvmWorkspace } from '../../api';
import { reservationUsage, reservedInstances } from './cards';
import { initialInterfaces } from './interfaces';
import { GIB, MIB, stackHosts, supportsCreate } from './queries';

export function createHosts(workspace: CvmWorkspace, scoped: boolean) {
  const reserved = reservedInstances(workspace);
  return stackHosts(workspace).map(host => {
    const instances = reserved.filter(instance => instance.node_id === host.node_id);
    const used = reservationUsage(instances);
    // The workspace can include out-of-scope hosts with visible instances. The terminal
    // policy flag uses the same host-tenant manager check as instance creation.
    const reason =
      scoped && !host.can_manage_terminal
        ? '宿主不在当前账号的管理范围'
        : host.error
          ? '宿主运行能力不可用'
          : !host.fresh
            ? '能力报告已过期，暂停创建'
            : !supportsCreate(host) || !host.capabilities.images.length || !initialInterfaces(host).length
              ? '未开放创建'
              : null;
    return { host, used, reason };
  });
}

export function resourceInputsFor(host: CvmHost | undefined, resources?: CvmResources) {
  const limits = host?.capabilities.resource_limits;
  return {
    vcpus: String(resources?.vcpus ?? limits?.min_vcpus ?? 1),
    memoryMiB: String((resources?.memory_bytes ?? limits?.min_memory_bytes ?? 128 * MIB) / MIB),
    diskGiB: String((resources?.root_disk_bytes ?? limits?.min_root_disk_bytes ?? 3 * GIB) / GIB),
  };
}

export function validCreateResources(host: CvmHost | undefined, resources: CvmResources) {
  const limits = host?.capabilities.resource_limits;
  return (
    !!limits &&
    [resources.vcpus, resources.memory_bytes, resources.root_disk_bytes].every(Number.isSafeInteger) &&
    resources.vcpus >= limits.min_vcpus &&
    resources.vcpus <= limits.max_vcpus &&
    resources.memory_bytes >= limits.min_memory_bytes &&
    resources.memory_bytes <= limits.max_memory_bytes &&
    resources.memory_bytes % limits.memory_step_bytes === 0 &&
    resources.root_disk_bytes >= limits.min_root_disk_bytes &&
    resources.root_disk_bytes <= limits.max_root_disk_bytes &&
    resources.root_disk_bytes % limits.root_disk_step_bytes === 0
  );
}

function sameImage(a: CvmImage, b: CvmImage) {
  return a.id === b.id && a.architecture === b.architecture && a.boot_mode === b.boot_mode && a.sha256 === b.sha256;
}

export function copyHostProblem(source: CvmInstance, host: CvmHost) {
  const { image, resources } = source.desired.spec;
  if (!image || !host.capabilities.images.some(item => sameImage(item, image))) return '不提供源容器的系统镜像';
  if (!validCreateResources(host, resources)) return '不支持源容器的规格';
  return null;
}

export function canCopyInstance(instance: CvmInstance) {
  return (
    instance.reserved &&
    instance.desired.power !== 'deleted' &&
    instance.desired.spec.execution.kind === 'cube' &&
    !!instance.desired.spec.image
  );
}

export function copyHost(source: CvmInstance, usable: CvmHost[], preferred?: string) {
  const matches = usable.filter(host => !copyHostProblem(source, host));
  return (
    matches.find(host => host.node_id === preferred) ??
    matches.find(host => host.node_id === source.node_id) ??
    matches[0]
  );
}

export function copyName(source: CvmInstance) {
  return `${Array.from(source.desired.spec.name).slice(0, 75).join('')}-copy`;
}
