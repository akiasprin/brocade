import { useEffect, useRef, useState, type ButtonHTMLAttributes } from 'react';
import { Icon } from './icons';
import { copyText } from './platform';

type CopyState = 'idle' | 'done' | 'failed';

export function CopyButton({
  text,
  label = '复制',
  successLabel = '已复制',
  failureLabel = '复制失败',
  iconOnly = false,
  className,
  disabled,
  ...buttonProps
}: Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'children' | 'onClick'> & {
  text: string;
  label?: string;
  successLabel?: string;
  failureLabel?: string;
  iconOnly?: boolean;
}) {
  const [state, setState] = useState<CopyState>('idle');
  const timer = useRef<number | undefined>(undefined);

  useEffect(
    () => () => {
      if (timer.current !== undefined) window.clearTimeout(timer.current);
    },
    [],
  );

  const stateLabel = state === 'done' ? successLabel : state === 'failed' ? failureLabel : label;
  const stateClass = state === 'idle' ? '' : ` ${state}`;

  return (
    <button
      {...buttonProps}
      className={`copy-button${className ? ` ${className}` : ''}${stateClass}`}
      type="button"
      disabled={disabled || !text}
      aria-label={iconOnly ? stateLabel : buttonProps['aria-label']}
      title={iconOnly ? stateLabel : buttonProps.title}
      data-copy-state={state}
      aria-live="polite"
      onClick={async () => {
        const copied = await copyText(text);
        setState(copied ? 'done' : 'failed');
        if (timer.current !== undefined) window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setState('idle'), 1_600);
      }}
    >
      {iconOnly ? (
        <Icon of={state === 'done' ? 'check' : state === 'failed' ? 'warn' : 'copy'} size={16} />
      ) : (
        stateLabel
      )}
    </button>
  );
}
