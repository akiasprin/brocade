import { useRouting } from './context';
/* Subscription routing uses the shared choice menu and service icons. */
import { useRef, useState, type ReactNode, type RefObject } from 'react';
import { ChoiceSelect, MenuLayer, menuArrowKey } from '../../ui/choice-menu';
import { Icon } from '../../ui/icons';
import { Glyph, KindMark, TargetMark } from './glyphs';
import {
  KIND,
  KINDS,
  detectRule,
  validFor,
  nextId,
  type CustomRule,
  type ModuleDef,
  type RuleKind,
  type Rules,
  type Target,
} from '../../clash-rules';

/* ── 草稿 ── */

export type RuleSetter = (update: (rules: Rules) => Rules) => void;

export const ops = {
  target: (id: string, target: Target | 'off') => (rules: Rules) => {
    if (id === 'final') rules.final = target as Target;
    else if (target === 'off') rules.modules[id] = { ...rules.modules[id], on: false };
    else rules.modules[id] = { on: true, target };
    return rules;
  },
  ruleTarget: (id: string, target: Target) => (rules: Rules) => {
    rules.custom = rules.custom.map(rule => (rule.id === id ? { ...rule, target } : rule));
    return rules;
  },
  addRule: (kind: RuleKind, value: string, target: Target) => (rules: Rules) => {
    rules.custom.push({ id: nextId(), kind, value, target });
    return rules;
  },
  removeRule: (id: string) => (rules: Rules) => {
    rules.custom = rules.custom.filter(rule => rule.id !== id);
    return rules;
  },
  moveRule: (id: string, delta: number) => (rules: Rules) => {
    const index = rules.custom.findIndex(rule => rule.id === id);
    const next = index + delta;
    if (index < 0 || next < 0 || next >= rules.custom.length) return rules;
    const [rule] = rules.custom.splice(index, 1);
    rules.custom.splice(next, 0, rule);
    return rules;
  },
  addService:
    (id: string, target: Target = 'select') =>
    (rules: Rules) => {
      if (!rules.extra.includes(id)) rules.extra.push(id);
      rules.modules[id] = { on: true, target };
      return rules;
    },
  removeService: (id: string) => (rules: Rules) => {
    rules.extra = rules.extra.filter(item => item !== id);
    delete rules.modules[id];
    return rules;
  },
};

/* ── 去向选项 ── */

interface TargetOption {
  group: string;
  value: Target | 'off';
  label: string;
  badge: string;
  detail?: string;
}

function useTargetOptions(module?: ModuleDef, offable = !!module && !module.locked): TargetOption[] {
  const { LINES, targetName } = useRouting();
  const standard = module?.standard;
  const mark = (target: Target, fallback?: string) => (module && target === standard ? '标准' : fallback);
  const list: TargetOption[] = module?.blockOnly
    ? [
        { group: '动作', value: 'reject', label: '拦截', badge: 'REJECT', detail: mark('reject') },
        { group: '动作', value: 'direct', label: '直连', badge: 'DIRECT', detail: mark('direct') },
      ]
    : [
        {
          group: '策略组',
          value: 'select',
          label: targetName('select'),
          badge: 'GROUP',
          detail: mark('select', '客户端当前选项'),
        },
        {
          group: '策略组',
          value: 'auto',
          label: targetName('auto'),
          badge: 'GROUP',
          detail: mark('auto', '按延迟选择'),
        },
        ...LINES.map(line => ({
          group: '线路',
          value: line.id,
          label: line.name,
          badge: line.region,
          detail: mark(line.id),
        })),
        { group: '动作', value: 'direct', label: '直连', badge: 'DIRECT', detail: mark('direct') },
        { group: '动作', value: 'reject', label: '拦截', badge: 'REJECT', detail: mark('reject') },
      ];
  if (offable) list.push({ group: '分流', value: 'off', label: '关闭', badge: 'OFF', detail: '交给后面的规则' });
  return list;
}

/** 行尾的去向：生产 ChoiceSelect，触发器是一段灰字与上下箭头（与 Sub-Store 设置页「代理认证」同款）。 */
export function TargetSelect({
  value,
  onChange,
  module,
  className = 'substore-row-pick',
  label = '去向',
  menuAlign,
}: {
  value: Target | 'off';
  onChange: (target: Target | 'off') => void;
  module?: ModuleDef;
  className?: string;
  label?: string;
  menuAlign?: 'end' | 'below';
}) {
  const options = useTargetOptions(module);
  const groups = [...new Set(options.map(option => option.group))];
  return (
    <ChoiceSelect
      className={`${className}${module && value !== module.standard ? ' clash-rule-changed' : ''}`}
      aria-label={label}
      menuAlign={menuAlign}
      value={value}
      onChange={event => onChange(event.target.value as Target | 'off')}
    >
      {groups.map(group => (
        <optgroup key={group} label={group}>
          {options
            .filter(option => option.group === group)
            .map(option => (
              <option key={option.value} value={option.value} data-badge={option.badge} data-detail={option.detail}>
                {option.label}
              </option>
            ))}
        </optgroup>
      ))}
    </ChoiceSelect>
  );
}

export interface MenuAction {
  label: string;
  danger?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}

/** 规则与分流共用的去向菜单，末尾包含上移、下移、删除或移除服务等操作。 */
export function TargetMenu({
  value,
  onChange,
  module,
  offable,
  label,
  title = '移到',
  actions = [],
  align = 'below',
  children,
}: {
  value: Target | 'off';
  onChange: (target: Target | 'off') => void;
  module?: ModuleDef;
  offable?: boolean;
  label: string;
  /** 菜单顶部的一行标题；行尾触发器上下文已经清楚，可以不给。 */
  title?: string | null;
  actions?: MenuAction[];
  align?: 'end' | 'below';
  children: (props: { ref: RefObject<HTMLButtonElement | null>; open: boolean; onClick: () => void }) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const options = useTargetOptions(module, offable ?? (!!module && !module.locked));
  const groups = [...new Set(options.map(option => option.group))];
  return (
    <>
      {children({ ref: anchor, open, onClick: () => setOpen(true) })}
      {open && (
        <MenuLayer anchor={anchor} label={label} align={align} onClose={() => setOpen(false)}>
          <div
            className="dialog-surface choice-menu clash-rule-target-menu"
            role="listbox"
            aria-label={label}
            onKeyDown={menuArrowKey}
          >
            {title && <div className="clash-rule-menu-title">{title}</div>}
            {groups.map(group => (
              <div role="group" aria-label={group} key={group}>
                <div className="choice-menu-section">{group}</div>
                {options
                  .filter(option => option.group === group)
                  .map(option => (
                    <button
                      key={option.value}
                      type="button"
                      role="option"
                      aria-selected={option.value === value}
                      autoFocus={option.value === value}
                      className={option.value === value ? 'on' : ''}
                      onClick={() => {
                        setOpen(false);
                        if (option.value !== value) onChange(option.value);
                      }}
                    >
                      <span className="choice-menu-badge">{option.badge}</span>
                      <span className="choice-menu-copy">{option.label}</span>
                      {option.detail && <small>{option.detail}</small>}
                    </button>
                  ))}
              </div>
            ))}
            {actions.length > 0 && (
              <div role="group" aria-label="操作">
                <div className="choice-menu-section">操作</div>
                {actions.map(action => (
                  <button
                    key={action.label}
                    type="button"
                    className={`clash-rule-menu-action${action.danger ? ' danger' : ''}`}
                    disabled={action.disabled}
                    onClick={() => {
                      setOpen(false);
                      action.onSelect();
                    }}
                  >
                    <span className="choice-menu-copy">{action.label}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        </MenuLayer>
      )}
    </>
  );
}

/* ── 行 ── */

export function ruleActions(rule: CustomRule, index: number, count: number, set: RuleSetter): MenuAction[] {
  return [
    { label: '上移', disabled: index === 0, onSelect: () => set(ops.moveRule(rule.id, -1)) },
    { label: '下移', disabled: index === count - 1, onSelect: () => set(ops.moveRule(rule.id, 1)) },
    { label: '删除规则', danger: true, onSelect: () => set(ops.removeRule(rule.id)) },
  ];
}

/** 行尾触发器：与生产 ChoiceSelect 的 substore-row-pick 同一外观（灰字 + 上下箭头）。 */
function RowPick({
  value,
  changed,
  ref: anchor,
  open,
  onClick,
}: {
  value: Target | 'off';
  changed?: boolean;
  ref: RefObject<HTMLButtonElement | null>;
  open: boolean;
  onClick: () => void;
}) {
  const { targetName } = useRouting();
  return (
    <button
      ref={anchor}
      type="button"
      className={`choice-select substore-row-pick${changed ? ' clash-rule-changed' : ''}`}
      aria-haspopup="listbox"
      aria-expanded={open}
      onClick={onClick}
    >
      <span>{value === 'off' ? '关闭' : targetName(value)}</span>
      <span className="choice-chevron" aria-hidden="true">
        ⌃<br />⌄
      </span>
    </button>
  );
}

export function RuleRow({
  rule,
  index,
  count,
  set,
  numbered = false,
}: {
  rule: CustomRule;
  index: number;
  count: number;
  set: RuleSetter;
  numbered?: boolean;
}) {
  return (
    <div className="substore-row clash-rule-rule-row" data-rule={rule.id}>
      {numbered && <span className="clash-rule-ord">{index + 1}</span>}
      <KindMark kind={rule.kind} />
      <code className="clash-rule-rule-value" title={`${KIND[rule.kind].clash},${rule.value}`}>
        {rule.value}
      </code>
      <TargetMenu
        value={rule.target}
        label={`${rule.value} 的去向`}
        title={null}
        align="end"
        onChange={target => set(ops.ruleTarget(rule.id, target as Target))}
        actions={ruleActions(rule, index, count, set)}
      >
        {trigger => <RowPick value={rule.target} {...trigger} />}
      </TargetMenu>
    </div>
  );
}

export function ModuleRow({
  def,
  rules,
  set,
  detail,
}: {
  def: ModuleDef;
  rules: Rules;
  set: RuleSetter;
  detail?: ReactNode;
}) {
  const state = def.id === 'final' ? { on: true, target: rules.final } : rules.modules[def.id];
  const value = state.on ? state.target : 'off';
  return (
    <div className="substore-row clash-rule-module-row" data-module={def.id} data-off={!state.on || undefined}>
      <Glyph of={def.glyph} size={18} />
      <span className="substore-row-copy">
        <b>{def.name}</b>
        {detail && <small>{detail}</small>}
      </span>
      <TargetMenu
        value={value}
        module={def}
        label={`${def.name}的去向`}
        title={null}
        align="end"
        onChange={target => set(ops.target(def.id, target))}
        actions={
          def.catalog ? [{ label: '移除服务', danger: true, onSelect: () => set(ops.removeService(def.id)) }] : []
        }
      >
        {trigger => <RowPick value={value} changed={value !== def.standard} {...trigger} />}
      </TargetMenu>
    </div>
  );
}

/* ── 添加一条自定义规则：一个输入框，自动认出类型 ── */

export function AddRuleRow({
  onAdd,
  defaultTarget = 'direct',
  fixedTarget,
  placeholder = '添加域名、IP 段或进程',
  autoFocus = false,
  compact = false,
  numbered,
  disabled = false,
}: {
  onAdd: (kind: RuleKind, value: string, target: Target) => void;
  disabled?: boolean;
  defaultTarget?: Target;
  /** 去向已定（按去向分组时在某张卡里添加），不再给去向选择。 */
  fixedTarget?: Target;
  placeholder?: string;
  autoFocus?: boolean;
  compact?: boolean;
  numbered?: number;
}) {
  const [text, setText] = useState('');
  const [kind, setKind] = useState<RuleKind | null>(null);
  const [target, setTarget] = useState<Target>(fixedTarget ?? defaultTarget);
  const found = detectRule(text);
  const effective = kind ?? found?.kind ?? null;
  const value =
    effective === 'keyword' ? text.trim().toLowerCase() : found && effective === found.kind ? found.value : text.trim();
  const valid = !!effective && validFor(effective, value);
  const add = () => {
    if (!valid || !effective) return;
    onAdd(effective, value, fixedTarget ?? target);
    setText('');
    setKind(null);
  };
  return (
    <div className={`substore-row clash-rule-add-row${compact ? ' compact' : ''}`} data-typing={text ? '' : undefined}>
      {numbered !== undefined && <span className="clash-rule-ord">{numbered}</span>}
      {text ? (
        <ChoiceSelect
          className="clash-rule-kind-pick"
          aria-label="规则类型"
          menuAlign="below"
          value={effective ?? 'domain'}
          onChange={event => setKind(event.target.value as RuleKind)}
        >
          {KINDS.map(item => (
            <option
              key={item}
              value={item}
              data-badge={KIND[item].clash.replace('DOMAIN-', '')}
              data-detail={KIND[item].hint}
            >
              {KIND[item].label}
            </option>
          ))}
        </ChoiceSelect>
      ) : (
        <span className="clash-rule-add-plus" aria-hidden="true">
          <Icon of="plus" size={13} />
        </span>
      )}
      <input
        className="clash-rule-add-input"
        disabled={disabled}
        value={text}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        autoFocus={autoFocus}
        aria-label={placeholder}
        aria-invalid={(text && !valid) || undefined}
        onChange={event => {
          setText(event.target.value);
          setKind(null);
        }}
        onKeyDown={event => {
          // 取消默认动作：否则随后的 keypress 会落到关菜单后拿回焦点的「＋」上，把菜单又打开。
          if (event.key === 'Enter') {
            event.preventDefault();
            add();
          }
          if (event.key === 'Escape' && text) {
            event.stopPropagation();
            setText('');
          }
        }}
      />
      {text && (
        <>
          {!fixedTarget && <TargetSelect value={target} onChange={next => setTarget(next as Target)} />}
          <button type="button" className="clash-rule-add-go" disabled={!valid || disabled} onClick={add}>
            添加
          </button>
        </>
      )}
    </div>
  );
}

/* ── 服务目录：首版菜单，图形放在小标那一栏 ── */

export function ServiceMenu({
  rules,
  onPick,
  trigger,
  label = '添加服务',
}: {
  rules: Rules;
  onPick: (id: string) => void;
  trigger: (props: { onClick: () => void; ref: RefObject<HTMLButtonElement | null>; open: boolean }) => ReactNode;
  label?: string;
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const { CATALOG } = useRouting();
  const left = CATALOG.filter(service => !rules.extra.includes(service.id));
  return (
    <>
      {trigger({ onClick: () => setOpen(true), ref: anchor, open })}
      {open && (
        <MenuLayer anchor={anchor} label={label} align="below" onClose={() => setOpen(false)}>
          <div
            className="dialog-surface choice-menu clash-rule-service-menu"
            role="listbox"
            aria-label={label}
            onKeyDown={menuArrowKey}
          >
            <div className="choice-menu-section">服务 · geosite</div>
            {left.map(service => (
              <button
                key={service.id}
                type="button"
                role="option"
                aria-selected={false}
                onClick={() => {
                  onPick(service.id);
                  setOpen(false);
                }}
              >
                <Glyph of={service.glyph} size={16} />
                <span className="choice-menu-copy">{service.name}</span>
                <small>{service.sets[0]}</small>
              </button>
            ))}
          </div>
        </MenuLayer>
      )}
    </>
  );
}

export function ServiceAddRow({ rules, set }: { rules: Rules; set: RuleSetter }) {
  return (
    <ServiceMenu
      rules={rules}
      onPick={id => set(ops.addService(id))}
      trigger={({ onClick, ref, open }) => (
        <button
          ref={ref}
          type="button"
          className="substore-row clash-rule-service-add"
          aria-expanded={open}
          onClick={onClick}
        >
          <span className="clash-rule-add-plus" aria-hidden="true">
            <Icon of="plus" size={13} />
          </span>
          <span className="substore-row-copy">
            <b>添加服务</b>
          </span>
        </button>
      )}
    />
  );
}

/* ── 去向：图形 + 名称 ── */

export function TargetLabel({
  target,
  size = 14,
  plain = false,
}: {
  target: Target | 'off';
  size?: number;
  plain?: boolean;
}) {
  const { targetName } = useRouting();
  if (target === 'off') return <span className="clash-rule-target off">关闭</span>;
  return (
    <span className="clash-rule-target">
      {(!plain || target.startsWith('line:')) && <TargetMark target={target} size={size} />}
      {targetName(target)}
    </span>
  );
}
