import { useEffect, useRef, type ReactNode } from 'react';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Icon } from '../../ui/icons';
import { ApiError, type CvmNicAddressMode, type CvmNicTransport } from '../../api';
import { requestId } from './queries';

export const NIC_CATALOG = ['cvm-nic-catalog'] as const;
export const NIC_STATE = ['cvm-nic-state'] as const;
export const uncertainCvmResult = (error: unknown) =>
  error != null && !(error instanceof ApiError && error.status >= 400 && error.status < 500 && error.status !== 408);

export function useCvmErrorNotice<T extends HTMLElement = HTMLDivElement>(error: unknown) {
  const root = useRef<T>(null);
  useEffect(() => {
    if (!error) return;
    const notices = root.current?.querySelectorAll<HTMLElement>('[role="alert"], .callout.err');
    if (notices?.length) notices.item(notices.length - 1).scrollIntoView?.({ block: 'nearest' });
  }, [error]);
  return root;
}
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
  submit = '保存变更',
  disabled = false,
  dirty = false,
  uncertain = false,
  error,
}: {
  title: string;
  children: ReactNode;
  pending: boolean;
  onClose: () => void;
  onSubmit: () => void;
  submit?: string;
  disabled?: boolean;
  dirty?: boolean;
  uncertain?: boolean;
  error?: unknown;
}) {
  const body = useCvmErrorNotice(error);
  return (
    <DialogLayer label={title} onClose={onClose} canClose={() => !pending}>
      <section className="dialog-surface tunnel-dialog cvm-dialog">
        <header>
          <b>
            {title}
            {dirty && <span className="nd-tab-badge dot" aria-label="未保存的修改" title="未保存的修改" />}
          </b>
          <DialogClose className="btn icon" aria-label="关闭" disabled={pending}>
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="tunnel-dialog-body cvm-dialog-body" ref={body}>
          {uncertain && (
            <p className="cvm-notice" role="status">
              上次保存结果尚未确认。请重试原操作，或关闭后核查结果再修改配置。
            </p>
          )}
          <fieldset className="cvm-form-fieldset cvm-network-fields" disabled={pending || uncertain}>
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
