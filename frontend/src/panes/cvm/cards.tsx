import type { CvmInstance, CvmQuota, CvmWorkspace } from '../../api';
import { capacity, ownerKey } from './queries';

export const percent = (share: number | null) => (share === null ? '—' : `${Math.round(share * 100)}%`);
export const shareOf = (used: number | undefined, total: number | undefined) =>
  used !== undefined && total !== undefined && total > 0 && used >= 0 && used <= total ? used / total : null;
export const resourceTone = (share: number | null, warn = 0.85, bad = 0.95) =>
  share === null ? 'none' : share >= bad ? 'bad' : share >= warn ? 'warn' : 'ok';
export const reservedInstances = (workspace: CvmWorkspace) => workspace.instances.filter(item => item.reserved);
export function reservationUsage(instances: CvmInstance[]) {
  return instances.reduce(
    (used, item) => ({
      instances: used.instances + 1,
      vcpus: used.vcpus + item.desired.spec.resources.vcpus,
      memory: used.memory + item.desired.spec.resources.memory_bytes,
      disk: used.disk + item.desired.spec.resources.root_disk_bytes,
      interfaces: used.interfaces + item.desired.spec.interfaces.length,
    }),
    { instances: 0, vcpus: 0, memory: 0, disk: 0, interfaces: 0 },
  );
}
export function quotaRows(workspace: CvmWorkspace, quota: CvmQuota) {
  const used = workspace.usage?.find(
    item => ownerKey(item.tenant_id, item.user_id) === ownerKey(quota.tenant_id, quota.user_id),
  );
  if (!used) return [];
  return [
    { label: '实例', used: used.instances, total: quota.max_instances, format: String },
    { label: 'vCPU', used: used.vcpus, total: quota.max_vcpus, format: String },
    { label: '内存', used: used.memory_bytes, total: quota.max_memory_bytes, format: capacity },
    { label: '系统盘', used: used.disk_bytes, total: quota.max_disk_bytes, format: capacity },
    { label: '网卡', used: used.interfaces, total: quota.max_interfaces, format: String },
  ];
}
export function CapacityMeter({ used, total, add = 0 }: { used: number; total: number; add?: number }) {
  const base = total > 0 ? Math.min(1, Math.max(0, used / total)) : used > 0 ? 1 : 0;
  const ghost = total > 0 ? Math.min(1 - base, Math.max(0, add / total)) : add > 0 ? 1 : 0;
  return (
    <span className={`cvm-meter ${used + add > total ? 'bad' : ''}`} aria-hidden="true">
      <i style={{ width: `${base * 100}%` }} />
      <b style={{ left: `${base * 100}%`, width: `${ghost * 100}%` }} />
    </span>
  );
}
export function CapacityRows({
  rows,
}: {
  rows: { label: string; used: number; total: number; format: (value: number) => string }[];
}) {
  return (
    <div className="cvm-capacity-rows">
      {rows.map(row => (
        <div className="cvm-capacity-row" key={row.label}>
          <span>{row.label}</span>
          <CapacityMeter used={row.used} total={row.total} />
          <b>
            {row.format(row.used)} / {row.format(row.total)}
          </b>
        </div>
      ))}
    </div>
  );
}
