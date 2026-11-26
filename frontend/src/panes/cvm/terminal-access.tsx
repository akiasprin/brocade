import { useState } from 'react';
import { Icon } from '../../ui/icons';

const pages = new Map<string, Window>();

export function cvmTerminalUrl(id: string) {
  const url = new URL(location.href);
  url.searchParams.set('webshell', id);
  url.hash = '';
  return url.href;
}

/** 每个容器的终端只开一个普通浏览器页面；已打开时聚焦。新页面被拦截时返回 false，由调用方给出链接。 */
export function openCvmTerminal(id: string) {
  for (const [instance, page] of pages) {
    if (page.closed) pages.delete(instance);
  }
  const previous = pages.get(id);
  if (previous && !previous.closed) {
    previous.focus();
    return true;
  }
  const page = window.open(cvmTerminalUrl(id), '_blank');
  if (!page) return false;
  pages.set(id, page);
  page.focus();
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
        title={reason || `在新页面打开 ${name} 的终端`}
      >
        <Icon of="nodes" size={14} />
        <span className="title-action-label">终端</span>
      </button>
      {blocked && (
        <a className="btn small" href={url.href} target="_blank" rel="noopener">
          新页面未能打开，请手动打开
        </a>
      )}
    </>
  );
}
