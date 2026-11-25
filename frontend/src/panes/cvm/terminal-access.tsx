import { useState } from 'react';
import { Icon } from '../../ui/icons';

const windows = new Map<string, Window>();

export function cvmTerminalUrl(id: string) {
  const url = new URL(location.href);
  url.searchParams.set('webshell', id);
  url.hash = '';
  return url.href;
}

/** 每个容器的终端只开一个窗口；已打开时聚焦。弹窗被拦截时返回 false，由调用方给出新标签页链接。 */
export function openCvmTerminal(id: string) {
  for (const [instance, child] of windows) {
    if (child.closed) windows.delete(instance);
  }
  const previous = windows.get(id);
  if (previous && !previous.closed) {
    previous.focus();
    return true;
  }
  const child = window.open(
    cvmTerminalUrl(id),
    `brocade-webshell-${encodeURIComponent(id)}`,
    'popup,width=1280,height=820,resizable=yes,scrollbars=yes',
  );
  if (!child) return false;
  windows.set(id, child);
  child.focus();
  return true;
}

export function CvmTerminalAccess({
  id,
  name,
  enabled,
  reason,
}: {
  id: string;
  name: string;
  enabled: boolean;
  reason: string;
}) {
  const [blocked, setBlocked] = useState(false);
  const url = { href: cvmTerminalUrl(id) };
  const open = () => setBlocked(!openCvmTerminal(id));
  return (
    <>
      <button
        className="btn title-action-compact"
        aria-label="终端"
        disabled={!enabled}
        onClick={open}
        title={reason || `打开 ${name} 的终端窗口`}
      >
        <Icon of="nodes" size={14} />
        <span className="title-action-label">终端</span>
      </button>
      {blocked && (
        <a className="btn small" href={url.href} target="_blank" rel="noopener">
          弹窗被拦截，改在新标签页打开
        </a>
      )}
    </>
  );
}
