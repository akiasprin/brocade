import {
  Children,
  isValidElement,
  useId,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import { DialogLayer } from './dialog';
import { useNarrow } from './viewport';
import { Icon } from './icons';

type OptionProps = {
  value?: string | number;
  children?: ReactNode;
  disabled?: boolean;
  label?: string;
  'data-group'?: string;
  'data-badge'?: string;
  'data-detail'?: string;
};

function optionText(children: ReactNode): string {
  return Children.toArray(children)
    .map(child => (isValidElement<OptionProps>(child) ? optionText(child.props.children) : String(child)))
    .join('');
}

function optionsOf(children: ReactNode, group?: string): Array<OptionProps & { text: string; value: string }> {
  return Children.toArray(children).flatMap(child => {
    if (!isValidElement<OptionProps>(child)) return [];
    if (child.type !== 'option') return optionsOf(child.props.children, child.props.label ?? group);
    return [
      {
        ...child.props,
        'data-group': child.props['data-group'] ?? group,
        value: String(child.props.value ?? optionText(child.props.children)),
        text: optionText(child.props.children),
      },
    ];
  });
}

/** Arrow navigation is shared by choice lists and richer, searchable target menus. */
export function menuArrowKey(event: KeyboardEvent<HTMLElement>) {
  const root = event.currentTarget;
  const buttons = [...root.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')];
  if (!buttons.length) return;
  const typing = event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement;
  if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key) || (typing && ['Home', 'End'].includes(event.key)))
    return;
  event.preventDefault();
  const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
  const next =
    event.key === 'Home'
      ? 0
      : event.key === 'End'
        ? buttons.length - 1
        : event.key === 'ArrowDown'
          ? (index + 1) % buttons.length
          : index < 0
            ? buttons.length - 1
            : (index - 1 + buttons.length) % buttons.length;
  buttons[next].focus();
  buttons[next].scrollIntoView?.({ block: 'nearest' });
}

export function MenuLayer({
  anchor,
  label,
  children,
  onClose,
  action = false,
  align = 'end',
}: {
  anchor: RefObject<HTMLElement | null>;
  label: string;
  children: ReactNode;
  onClose: () => void;
  action?: boolean;
  /** 桌面弹出位置：行尾触发器右缘对齐（默认），行首触发器用 `below` 左缘对齐。手机一律从底部升起。 */
  align?: 'end' | 'below';
}) {
  const phone = useNarrow();
  return (
    <DialogLayer
      {...(phone ? { mode: 'drawer' as const } : { mode: 'popover' as const, anchor, align })}
      label={label}
      className={`choice-layer${action ? ' action-layer' : ''}`}
      onClose={onClose}
    >
      {children}
    </DialogLayer>
  );
}

/** A controlled choice with the same option data as a select and a shared custom popup. */
export function ChoiceSelect({
  children,
  value,
  onChange,
  className,
  menuAlign,
  ...props
}: Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'onChange' | 'value'> & {
  children: ReactNode;
  value: string | number;
  onChange: (event: { target: { value: string } }) => void;
  menuAlign?: 'end' | 'below';
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const id = useId();
  const options = optionsOf(children);
  const selected = options.find(option => option.value === String(value));
  const label = props['aria-label'] ?? props.title ?? '选择';
  const groups = [...new Set(options.map(option => option['data-group'] ?? label))];
  return (
    <>
      <button
        {...props}
        ref={anchor}
        value={String(value)}
        type="button"
        className={`choice-select ${className ?? ''}`}
        role="combobox"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        onClick={() => setOpen(true)}
        onKeyDown={event => {
          if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            event.preventDefault();
            setOpen(true);
          }
        }}
      >
        <span>{selected?.text ?? String(value)}</span>
        <span className="choice-chevron" aria-hidden="true">
          ⌃<br />⌄
        </span>
      </button>
      {open && (
        <MenuLayer anchor={anchor} label={label} align={menuAlign} onClose={() => setOpen(false)}>
          <div
            id={id}
            className="dialog-surface choice-menu"
            role="listbox"
            aria-label={label}
            onKeyDown={menuArrowKey}
          >
            {groups.map(group => (
              <div role="group" aria-label={group} key={group}>
                <div className="choice-menu-section">{group}</div>
                {options
                  .filter(option => (option['data-group'] ?? label) === group)
                  .map(option => (
                    <button
                      key={option.value}
                      data-value={option.value}
                      type="button"
                      role="option"
                      aria-selected={option.value === String(value)}
                      disabled={option.disabled}
                      autoFocus={option.value === String(value) && !option.disabled}
                      className={option.value === String(value) ? 'on' : ''}
                      onClick={() => {
                        if (anchor.current?.matches(':disabled')) return;
                        onChange({ target: { value: option.value } });
                        setOpen(false);
                      }}
                    >
                      {option['data-badge'] && <span className="choice-menu-badge">{option['data-badge']}</span>}
                      <span className="choice-menu-copy">{option.text}</span>
                      {option['data-detail'] && <small>{option['data-detail']}</small>}
                    </button>
                  ))}
              </div>
            ))}
          </div>
        </MenuLayer>
      )}
    </>
  );
}

export function ActionMenu({
  label,
  items,
}: {
  label: string;
  items: Array<{ label: string; note: string; disabled?: boolean | string; danger?: boolean; onSelect: () => void }>;
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className="rule-machine-more"
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        <Icon of="more" size={14} />
      </button>
      {open && (
        <MenuLayer anchor={anchor} label={label} action onClose={() => setOpen(false)}>
          <div
            className="dialog-surface choice-menu machine-action-menu"
            role="menu"
            aria-label={label}
            onKeyDown={menuArrowKey}
          >
            {items.map(item => (
              <button
                type="button"
                role="menuitem"
                key={item.label}
                className={item.danger ? 'danger' : ''}
                disabled={!!item.disabled}
                onClick={() => {
                  setOpen(false);
                  item.onSelect();
                }}
              >
                <b>{item.label}</b>
                <small>{typeof item.disabled === 'string' ? item.disabled : item.note}</small>
              </button>
            ))}
          </div>
        </MenuLayer>
      )}
    </>
  );
}
