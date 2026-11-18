import { lazy, Suspense, useState } from 'react';
import { Loading } from '../../ui/bits';
import { Icon } from '../../ui/icons';

const CvmTerminal = lazy(() => import('./terminal'));

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
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        className="btn title-action-compact"
        aria-label="终端"
        disabled={!enabled}
        onClick={() => setOpen(true)}
        title={reason}
      >
        <Icon of="nodes" size={14} />
        <span className="title-action-label">终端</span>
      </button>
      {open && enabled && (
        <Suspense fallback={<Loading />}>
          <CvmTerminal id={id} name={name} onClose={() => setOpen(false)} />
        </Suspense>
      )}
    </>
  );
}
