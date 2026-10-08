import { useEffect, useRef, useState, type ButtonHTMLAttributes, type ReactNode } from 'react';
import { Icon, type IconName } from './icons';
import { copyText } from './platform';

export type CopyState = 'idle' | 'done' | 'failed';

/** 复制按钮在各状态下的图标：空闲为复制、成功为对勾、失败为警示。 */
export const copyStateIcon = (state: CopyState): IconName =>
  state === 'done' ? 'check' : state === 'failed' ? 'warn' : 'copy';

export function CopyButton({
  text,
  label = '复制',
  successLabel = '已复制',
  failureLabel = '复制失败',
  iconOnly = false,
  className,
  disabled,
  children,
  ...buttonProps
}: Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'children' | 'onClick'> & {
  text: string;
  label?: string;
  successLabel?: string;
  failureLabel?: string;
  iconOnly?: boolean;
  /** 自定义按钮内容（例如「地址 + 复制图标」）。复制、反馈与复位仍由本组件负责。 */
  children?: (state: CopyState, stateLabel: string) => ReactNode;
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
      {children ? children(state, stateLabel) : iconOnly ? <Icon of={copyStateIcon(state)} size={16} /> : stateLabel}
    </button>
  );
}
