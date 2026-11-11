import { useRef, type ReactNode } from 'react';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Icon } from '../../ui/icons';
import type { CvmNicAddressMode, CvmNicTransport } from '../../api';
import { requestId } from './queries';

export const NIC_CATALOG = ['cvm-nic-catalog'] as const;
export const NIC_STATE = ['cvm-nic-state'] as const;
export const transportName: Record<CvmNicTransport, string> = { xray: 'Xray', wireguard: 'WireGuard' };
export const addressModeName: Record<CvmNicAddressMode, string> = {
  shared_nat: '共享 NAT 出口',
  dedicated_routed: '独立地址 · 路由直达',
  dedicated_nat: '独立地址 · 一对一映射',
};
export function useNicRequestId() {
  const attempt = useRef<{ digest: string; id: string } | null>(null);
  return (payload: unknown) => {
    const digest = JSON.stringify(payload);
    if (attempt.current?.digest !== digest) attempt.current = { digest, id: requestId() };
    return attempt.current.id;
  };
}
export function NicDialog({
  title,
  children,
  pending,
  onClose,
  onSubmit,
  submit = '保存',
  disabled = false,
}: {
  title: string;
  children: ReactNode;
  pending: boolean;
  onClose: () => void;
  onSubmit: () => void;
  submit?: string;
  disabled?: boolean;
}) {
  return (
    <DialogLayer label={title} onClose={onClose} canClose={() => !pending}>
      <section className="dialog-surface tunnel-dialog cvm-dialog">
        <header>
          <b>{title}</b>
          <DialogClose className="btn icon" aria-label="关闭" disabled={pending}>
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="tunnel-dialog-body cvm-dialog-body">
          <fieldset className="cvm-form-fieldset cvm-network-fields" disabled={pending}>
            {children}
          </fieldset>
        </div>
        <footer>
          <DialogClose className="btn" disabled={pending}>
            取消
          </DialogClose>
          <button className="btn primary" disabled={pending || disabled} onClick={onSubmit}>
            {pending ? '正在保存…' : submit}
          </button>
        </footer>
      </section>
    </DialogLayer>
  );
}
export const networkLines = (value: string) =>
  value
    .split(/[\s,，]+/)
    .map(value => value.trim())
    .filter(Boolean);
