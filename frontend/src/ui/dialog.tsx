import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';
import { usePresence } from './presence';

type DialogMode = 'modal' | 'drawer';

const DialogCloseContext = createContext<(() => void) | null>(null);
interface DialogEntry {
  identity: symbol;
  root: HTMLElement;
  returnTargets: HTMLElement[];
}

const dialogStack: DialogEntry[] = [];
let scrollLocks = 0;
let savedBodyOverflow = '';

const focusableSelector = [
  'button:not(:disabled)',
  'a[href]',
  'input:not(:disabled)',
  'select:not(:disabled)',
  'textarea:not(:disabled)',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

function focusableElements(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>(focusableSelector)].filter(
    element =>
      element.tabIndex >= 0 &&
      !element.hidden &&
      element.getAttribute('aria-hidden') !== 'true' &&
      !element.classList.contains('dialog-scrim'),
  );
}

/**
 * Shared modal/drawer boundary. It owns the scrim, Escape handling, focus containment and focus
 * restoration so feature dialogs cannot silently drift apart on those interaction contracts.
 */
export function DialogLayer({
  label,
  mode = 'modal',
  className,
  onClose,
  canClose,
  children,
}: {
  label: string;
  mode?: DialogMode;
  className?: string;
  onClose: () => void;
  /** Checked before the exit animation starts. Return false to keep the dialog fully interactive. */
  canClose?: () => boolean;
  children: ReactNode;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const identity = useRef(Symbol(label));
  const closeRef = useRef(onClose);
  const canCloseRef = useRef(canClose);
  const closed = useRef(false);
  const closeRequested = useRef(false);
  // Capture before the portal commits: a descendant with autoFocus is focused before layout
  // effects run. When one dialog replaces another, retain the older dialog's opener as a fallback
  // because the control that opened the replacement is about to leave the DOM.
  const activeOnOpen =
    typeof document !== 'undefined' && document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const inheritedTargets = activeOnOpen?.closest('.dialog-layer') ? (dialogStack.at(-1)?.returnTargets ?? []) : [];
  const returnTargetsRef = useRef(
    activeOnOpen ? [activeOnOpen, ...inheritedTargets.filter(target => target !== activeOnOpen)] : inheritedTargets,
  );
  const [closing, setClosing] = useState(false);
  const duration = mode === 'drawer' ? 300 : 260;
  const presence = usePresence(!closing, duration);

  useLayoutEffect(() => {
    closeRef.current = onClose;
    canCloseRef.current = canClose;
  }, [canClose, onClose]);

  const requestClose = useCallback(() => {
    if (closeRequested.current) return;
    if (canCloseRef.current?.() === false) return;
    closeRequested.current = true;
    setClosing(true);
  }, []);

  useLayoutEffect(() => {
    if (!closing || presence.present || closed.current) return;
    closed.current = true;
    closeRef.current();
  }, [closing, presence.present]);

  useLayoutEffect(() => {
    const dialog = rootRef.current;
    const dialogIdentity = identity.current;
    const returnTargets = returnTargetsRef.current;
    if (!dialog) return;
    dialogStack.push({ identity: dialogIdentity, root: dialog, returnTargets });

    if (scrollLocks === 0) {
      savedBodyOverflow = document.body.style.overflow;
      document.body.style.overflow = 'hidden';
    }
    scrollLocks += 1;

    const focusTarget =
      dialog?.querySelector<HTMLElement>('[autofocus]') ?? (dialog ? focusableElements(dialog)[0] : null);
    focusTarget?.focus();

    return () => {
      let index = -1;
      for (let candidate = dialogStack.length - 1; candidate >= 0; candidate -= 1) {
        if (dialogStack[candidate].identity !== dialogIdentity) continue;
        index = candidate;
        break;
      }
      if (index >= 0) dialogStack.splice(index, 1);
      scrollLocks = Math.max(0, scrollLocks - 1);
      if (scrollLocks === 0) document.body.style.overflow = savedBodyOverflow;
      // React removes an auto-focused child after layout-effect cleanup. Restore on the next
      // microtask so that removal cannot move focus back to <body> after this call.
      queueMicrotask(() => {
        const remainingDialog = dialogStack.at(-1);
        const target = returnTargets.find(
          candidate => candidate.isConnected && (!remainingDialog || remainingDialog.root.contains(candidate)),
        );
        target?.focus();
      });
    };
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (dialogStack.at(-1)?.identity !== identity.current) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        requestClose();
        return;
      }
      if (event.key !== 'Tab') return;

      const dialog = rootRef.current;
      if (!dialog) return;
      const focusable = focusableElements(dialog);
      if (focusable.length === 0) {
        event.preventDefault();
        dialog.focus();
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [requestClose]);

  if (!presence.present) return null;

  return createPortal(
    <DialogCloseContext.Provider value={requestClose}>
      <div
        ref={rootRef}
        className={`dialog-layer ${mode}${className ? ` ${className}` : ''}`}
        data-motion-state={presence.phase}
        role="dialog"
        aria-modal="true"
        aria-label={label}
        aria-hidden={closing || undefined}
        inert={closing}
        tabIndex={-1}
      >
        <button className="dialog-scrim" type="button" tabIndex={-1} aria-label="关闭" onClick={requestClose} />
        {children}
      </div>
    </DialogCloseContext.Provider>,
    document.body,
  );
}

/** A button that closes the nearest DialogLayer through the same animated close path. */
export function DialogClose({ onClick, type = 'button', ...props }: ButtonHTMLAttributes<HTMLButtonElement>) {
  const close = useContext(DialogCloseContext);
  if (!close) throw new Error('DialogClose 必须放在 DialogLayer 内');
  return (
    <button
      {...props}
      type={type}
      onClick={event => {
        onClick?.(event);
        if (!event.defaultPrevented) close();
      }}
    />
  );
}
