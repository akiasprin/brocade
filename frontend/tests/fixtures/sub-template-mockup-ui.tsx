// Option parts for the subscription-template mockup (round 4). Synthetic data only; never imported by the app.
import { Fragment, useId, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from 'react';
import { SegmentedControl } from '../../src/ui/bits';
import { DialogClose, DialogLayer } from '../../src/ui/dialog';
import { Icon } from '../../src/ui/icons';
import { RegionFlag } from '../../src/ui/region-flag';
import { SiteIcon } from '../../src/ui/site-icon';
import { useNarrow } from '../../src/ui/viewport';
import {
  APPS,
  CHAINS,
  GROUPS,
  MODE_LABEL,
  SELECTED_USER,
  SPECIAL_LABEL,
  USERS,
  baseTemplate,
  chainKey,
  chainOfKey,
  groupById,
  groupYaml,
  isBase,
  isChainKey,
  sameConfig,
  sectionsFor,
  type Chain,
  type Glyph,
  type GroupConfig,
  type GroupDef,
  type GroupId,
  type Mode,
  type Section,
  type Special,
  type TargetKey,
  type TemplateConfig,
} from './sub-template-mockup-data';

export type Option = 'a' | 'b' | 'c';

/** 三个关键分组固定在最上面。 */
const SECTIONS = sectionsFor('third');
const KEY_GROUPS = SECTIONS[0].groups;
const OTHER_SECTIONS = SECTIONS.slice(1);

/* ══ 共用部件 ══ */

/** DialogLayer focuses `[autofocus]` first; React keeps the prop to itself for non-form elements, so write the
 *  attribute. Opening then lands on the content instead of drawing a focus ring on the first control. */
const autofocus = (element: HTMLElement | null) => element?.setAttribute('autofocus', '');

/* (!) 注释：与用户页「计量口径」同一画法——标签后跟信息图形，点一下在下方展开小字说明，再点收起。
   落地时应把两处收成一个共用部件。 */
function useHelp() {
  const [open, setOpen] = useState(false);
  const id = useId();
  return {
    toggle: (label: ReactNode, name: string) => (
      <button
        type="button"
        className="stp-help"
        aria-label={`${name}说明`}
        aria-expanded={open}
        aria-controls={id}
        onClick={event => {
          event.stopPropagation();
          setOpen(shown => !shown);
        }}
      >
        {label}
        <Icon of="info" size={13} />
      </button>
    ),
    note: (children: ReactNode) => (
      <div id={id} className="dim stp-note" hidden={!open}>
        {children}
      </div>
    ),
  };
}

const TEMPLATE_NOTE = (
  <>
    <p>所有用户共用一份模板，每次拉取订阅时按该用户的授权生成。</p>
    <p>没有修改的分组与 SubBoost 标准版一致。</p>
  </>
);
const MODE_NOTE = (
  <>
    <p>手动：默认使用第一条，用户可在客户端里切换。</p>
    <p>最快：每 5 分钟测一次延迟，自动使用最快的一条。</p>
    <p>按顺序：使用第一条可用的线路，不可用时依次切到下一条。</p>
  </>
);
const TARGET_NOTE = (
  <>
    <p>订阅里只出现该用户已授权的线路；一条都没有时，这一组回到默认去向。</p>
    <p>手动分组会在末尾附上节点选择和直连，所选线路都不可用时仍可在客户端里切换。</p>
  </>
);
const AUTO_NOTE = <p>默认全部线路参与测速。移出的线路不参与自动选择，仍可在节点选择里手动使用。</p>;
const SECTION_NOTE: Partial<Record<Section['id'], ReactNode>> = {
  service: (
    <>
      <p>客户端按规则顺序匹配，命中第一条即停；都没有命中的流量交给漏网之鱼。</p>
      <p>关闭的分组连同规则一起去掉，这些流量继续往下匹配。</p>
    </>
  ),
  direct: <p>广告默认拦截，私有网络与国内服务默认直连；私有网络与国内服务不能关闭。</p>,
};

const BRAND: Partial<Record<Glyph, string>> = {
  youtube: 'youtube',
  google: 'google',
  apple: 'apple',
  telegram: 'telegram',
  github: 'github',
};

// 自绘的单色图形，与 Icon 同为 24 栅格、1.6 描边；颜色由 data-glyph 决定。
const PATHS: Partial<Record<Glyph, ReactNode>> = {
  rocket: (
    <>
      <path d="M12 2.8c2.9 2.3 4.4 5.6 4.4 9.6l-1.9 3.9h-5l-1.9-3.9c0-4 1.5-7.3 4.4-9.6Z" />
      <circle cx="12" cy="9.4" r="1.6" />
      <path d="M7.7 12.2 5.2 15l2.6 1.6M16.3 12.2l2.5 2.8-2.6 1.6M10.6 19.2 12 21.6l1.4-2.4" />
    </>
  ),
  sparkle: (
    <>
      <path d="M11 3.5 12.7 8.3 17.5 10 12.7 11.7 11 16.5 9.3 11.7 4.5 10 9.3 8.3Z" />
      <path d="M17.8 14.6 18.6 16.9 20.9 17.7 18.6 18.5 17.8 20.8 17 18.5 14.7 17.7 17 16.9Z" />
    </>
  ),
  house: (
    <>
      <path d="M4 10.8 12 4l8 6.8V20H4Z" />
      <path d="M10 20v-5.2h4V20" />
    </>
  ),
  globe: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <ellipse cx="12" cy="12" rx="3.8" ry="8.5" />
      <path d="M3.8 9.2h16.4M3.8 14.8h16.4" />
    </>
  ),
  fish: (
    <>
      <path d="M21 12c-2 3.4-4.9 5.4-8.4 5.4S6.6 15.4 5.2 12c1.4-3.4 3.9-5.4 7.4-5.4S19 8.6 21 12Z" />
      <path d="M5.2 12 2.4 8.6v6.8Z" />
      <circle cx="16.6" cy="11" r="0.9" fill="currentColor" stroke="none" />
    </>
  ),
};

/** 分组图形：透明底，各自一种颜色——系统色，或服务自己的品牌色；国内服务用国旗。 */
export function GroupGlyph({ glyph }: { glyph: Glyph }) {
  if (glyph === 'cn')
    return (
      <span className="stp-glyph flag" data-glyph={glyph} aria-hidden="true">
        <RegionFlag code="CN" square />
      </span>
    );
  if (glyph === 'microsoft')
    return (
      <span className="stp-glyph" data-glyph={glyph} aria-hidden="true">
        <svg viewBox="0 0 24 24" width="16" height="16">
          <rect x="2.5" y="2.5" width="9" height="9" fill="#f25022" />
          <rect x="12.5" y="2.5" width="9" height="9" fill="#7fba00" />
          <rect x="2.5" y="12.5" width="9" height="9" fill="#00a4ef" />
          <rect x="12.5" y="12.5" width="9" height="9" fill="#ffb900" />
        </svg>
      </span>
    );
  if (glyph === 'bolt' || glyph === 'ban')
    return (
      <span className="stp-glyph" data-glyph={glyph} aria-hidden="true">
        <Icon of={glyph} size={16} />
      </span>
    );
  const brand = BRAND[glyph];
  if (brand)
    return (
      <span className="stp-glyph brand" data-glyph={glyph} aria-hidden="true">
        <SiteIcon siteId={brand} />
      </span>
    );
  return (
    <span className="stp-glyph" data-glyph={glyph} aria-hidden="true">
      <svg
        viewBox="0 0 24 24"
        width="16"
        height="16"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.7"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        {PATHS[glyph]}
      </svg>
    </span>
  );
}

export function ChainName({ chain }: { chain: Chain }) {
  return (
    <span className="stp-chain">
      <RegionFlag code={chain.country} />
      <span>{chain.name}</span>
    </span>
  );
}

function TargetName({ target }: { target: TargetKey }) {
  return isChainKey(target) ? (
    <ChainName chain={chainOfKey(target)} />
  ) : (
    <span className="stp-special">{SPECIAL_LABEL[target]}</span>
  );
}

function Switch({ checked, label, onChange }: { checked: boolean; label: string; onChange: (next: boolean) => void }) {
  return (
    <label className="switch stp-switch" onClick={event => event.stopPropagation()}>
      <input type="checkbox" checked={checked} aria-label={label} onChange={event => onChange(event.target.checked)} />
      <span className="switch-ui" aria-hidden="true">
        <span />
      </span>
    </label>
  );
}

/** 弹出菜单的标记：上下两个小箭头，与 macOS 弹出按钮相同。 */
function PopupGlyph() {
  return (
    <svg
      className="stp-popup-glyph"
      viewBox="0 0 10 16"
      width="7"
      height="11"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M2 6 5 3l3 3M2 10l3 3 3-3" />
    </svg>
  );
}

/** 选线路时唯一保留的事实：AI 与油管两组标出不能解锁的线路。 */
function UnlockNote({ chain, unlock }: { chain: Chain; unlock?: GroupDef['unlock'] }) {
  if (unlock === 'ai' && chain.ai === 'no') return <span className="stp-unlock">AI 不可用</span>;
  if (unlock === 'youtube' && chain.premium.verdict === 'no') return <span className="stp-unlock">无 Premium</span>;
  return <span />;
}

/* 选择规则：未改动的组加一条线路，就以它为首选；移到一条不剩时回到默认去向。
   自动选择不同：空表示全部线路参与测速，移出一条就是「其余全部」，选回全部时重新记为空。 */
const ALL_CHAIN_KEYS = CHAINS.map(item => chainKey(item.id));

const currentTargets = (def: GroupDef, config: GroupConfig) =>
  def.id === 'auto' && config.targets.length === 0 ? ALL_CHAIN_KEYS : config.targets;

function toggleTarget(def: GroupDef, config: GroupConfig, key: TargetKey): GroupConfig {
  if (def.id === 'auto') {
    const current = currentTargets(def, config);
    const next = current.includes(key) ? current.filter(target => target !== key) : [...current, key];
    const ordered = ALL_CHAIN_KEYS.filter(target => next.includes(target));
    return { ...config, targets: ordered.length === ALL_CHAIN_KEYS.length ? [] : ordered };
  }
  if (config.targets.includes(key)) {
    const targets = config.targets.filter(target => target !== key);
    return targets.length ? { ...config, targets } : { ...def.base, on: config.on, targets: [...def.base.targets] };
  }
  return { ...config, targets: isBase(def.id, config) ? [key] : [...config.targets, key] };
}

/** 菜单里的单选：一个去向、手动切换；选回默认去向就是恢复默认。 */
const only = (def: GroupDef, config: GroupConfig, key: TargetKey): GroupConfig =>
  def.base.targets.length === 1 && def.base.targets[0] === key
    ? { ...def.base, on: config.on, targets: [...def.base.targets] }
    : { ...config, targets: [key], mode: 'select' };

const SPECIALS_FOR = (def: GroupDef): Special[] =>
  def.id === 'auto' ? [] : (['select', 'auto', 'direct', 'reject'] as Special[]).filter(item => item !== def.id);

const restore = (def: GroupDef): GroupConfig => ({ ...def.base, targets: [...def.base.targets] });

const matchText = (def: GroupDef) =>
  def.id === 'final'
    ? '其余流量'
    : def.id === 'select'
      ? '其他分组默认跟随'
      : def.id === 'auto'
        ? '按延迟测速'
        : def.rules.join(' · ');

/** 去向读数。hero 写全整条去向；其余只写首选，后面跟 +N。 */
export function Destination({
  def,
  config,
  variant = 'row',
}: {
  def: GroupDef;
  config: GroupConfig;
  variant?: 'hero' | 'row';
}) {
  if (!config.on) return <span className={`stp-dest ${variant} quiet`}>关闭</span>;
  if (def.id === 'auto')
    return (
      <span className={`stp-dest ${variant}${config.targets.length ? '' : ' quiet'}`}>
        {config.targets.length ? `${config.targets.length} 条线路` : '全部线路'}
      </span>
    );
  const [first] = config.targets;
  if (config.targets.length === 1 && !isChainKey(first))
    return (
      <span className={`stp-dest ${variant} quiet`}>
        {first === 'select' && def.id !== 'select' ? '跟随节点选择' : SPECIAL_LABEL[first]}
      </span>
    );
  const shown = variant === 'hero' ? config.targets : config.targets.slice(0, 1);
  const more = config.targets.length - shown.length;
  return (
    <span className={`stp-dest ${variant}`}>
      {variant !== 'hero' && config.mode !== 'select' && <span className="stp-mode">{MODE_LABEL[config.mode]}</span>}
      {shown.map((target, index) => (
        <Fragment key={target}>
          {index > 0 && (
            <span className="stp-sep" aria-hidden="true">
              {config.mode === 'fallback' ? '→' : '·'}
            </span>
          )}
          <TargetName target={target} />
        </Fragment>
      ))}
      {more > 0 && <span className="stp-more">+{more}</span>}
    </span>
  );
}

const Dirty = ({ on }: { on: boolean }) => (on ? <i className="stp-dirty" title="未保存" /> : null);

/* ── 去向编辑：上面是已选（可拖动排序），下面是可加入的去向 ── */

function GripIcon() {
  return (
    <svg viewBox="0 0 10 16" width="8" height="13" fill="currentColor" aria-hidden="true">
      {[3, 8, 13].flatMap(y => [
        <circle key={`l${y}`} cx="2.5" cy={y} r="1.3" />,
        <circle key={`r${y}`} cx="7.5" cy={y} r="1.3" />,
      ])}
    </svg>
  );
}

function ChosenList({
  def,
  config,
  onChange,
}: {
  def: GroupDef;
  config: GroupConfig;
  onChange: (next: GroupConfig) => void;
}) {
  const [dragging, setDragging] = useState<number | null>(null);
  const chosen = currentTargets(def, config);
  const base = isBase(def.id, config);
  const ordered = def.id !== 'auto' && config.mode !== 'url-test' && chosen.length > 1;
  // 默认去向不能单独移出：它就是「没有设置」的状态。自动选择至少保留一条线路。
  const removable = def.id === 'auto' ? chosen.length > 1 : !base;
  const move = (from: number, to: number) => {
    const targets = [...config.targets];
    const [item] = targets.splice(from, 1);
    targets.splice(to, 0, item);
    onChange({ ...config, targets });
  };
  return (
    <ol className="stp-cells stp-chosen">
      {chosen.map((target, index) => (
        <li
          key={target}
          draggable={ordered}
          className={`stp-cell${dragging === index ? ' dragging' : ''}`}
          onDragStart={event => {
            event.dataTransfer.effectAllowed = 'move';
            setDragging(index);
          }}
          onDragEnd={() => setDragging(null)}
          onDragOver={event => {
            event.preventDefault();
            if (dragging !== null && dragging !== index) {
              move(dragging, index);
              setDragging(index);
            }
          }}
        >
          <span className="stp-order" aria-hidden="true">
            {ordered ? index + 1 : <Icon of="check" size={11} />}
          </span>
          <TargetName target={target} />
          {isChainKey(target) ? <UnlockNote chain={chainOfKey(target)} unlock={def.unlock} /> : <span />}
          {ordered ? (
            <span className="stp-grip" title="拖动排序">
              <GripIcon />
            </span>
          ) : (
            <span />
          )}
          {removable ? (
            <button
              type="button"
              className="stp-remove"
              aria-label={`移出 ${isChainKey(target) ? chainOfKey(target).name : SPECIAL_LABEL[target]}`}
              onClick={() => onChange(toggleTarget(def, config, target))}
            >
              <Icon of="close" size={11} />
            </button>
          ) : (
            <span />
          )}
        </li>
      ))}
    </ol>
  );
}

function AvailableList({
  def,
  config,
  onChange,
}: {
  def: GroupDef;
  config: GroupConfig;
  onChange: (next: GroupConfig) => void;
}) {
  const chosen = currentTargets(def, config);
  const specials = SPECIALS_FOR(def).filter(key => !chosen.includes(key));
  const sections = [
    ...(specials.length ? [{ id: 'special', label: '其他去向', keys: specials as TargetKey[] }] : []),
    ...APPS.map(app => ({
      id: app.id,
      label: app.label,
      keys: app.chains.map(item => chainKey(item.id)).filter(key => !chosen.includes(key)),
    })),
  ].filter(section => section.keys.length > 0);
  return (
    <>
      {sections.map(section => (
        <Fragment key={section.id}>
          <h4 className="stp-cells-label">{section.label}</h4>
          <div className="stp-cells">
            {section.keys.map(key => (
              <button
                key={key}
                type="button"
                className="stp-cell stp-add"
                onClick={() => onChange(toggleTarget(def, config, key))}
              >
                <span className="stp-plus" aria-hidden="true">
                  <Icon of="plus" size={11} />
                </span>
                <TargetName target={key} />
                {isChainKey(key) ? <UnlockNote chain={chainOfKey(key)} unlock={def.unlock} /> : <span />}
              </button>
            ))}
          </div>
        </Fragment>
      ))}
    </>
  );
}

function ModeControl({ config, onChange }: { config: GroupConfig; onChange: (next: GroupConfig) => void }) {
  return (
    <SegmentedControl
      className="stp-modes"
      ariaLabel="切换方式"
      value={config.mode}
      options={(['select', 'url-test', 'fallback'] as Mode[]).map(mode => ({ value: mode, label: MODE_LABEL[mode] }))}
      onChange={mode => onChange({ ...config, mode })}
    />
  );
}

/** 「切换方式 (!)」一行：与计量口径一样，左边标签，右边分段控件，说明展开在下方。 */
function ModeRow({
  config,
  onChange,
  boxed,
}: {
  config: GroupConfig;
  onChange: (next: GroupConfig) => void;
  boxed?: boolean;
}) {
  const help = useHelp();
  const row = (
    <div className={`stp-labeled${boxed ? ' stp-cell stp-field' : ''}`}>
      {help.toggle('切换方式', '切换方式')}
      <ModeControl config={config} onChange={onChange} />
    </div>
  );
  return (
    <>
      {boxed ? <div className="stp-cells">{row}</div> : row}
      {help.note(MODE_NOTE)}
    </>
  );
}

/** 「去向 (!)」标签与说明。 */
function TargetsLabel({ def }: { def: GroupDef }) {
  const help = useHelp();
  return (
    <>
      <h4 className="stp-cells-label">{help.toggle(def.id === 'auto' ? '参与测速' : '去向', '去向')}</h4>
      {help.note(def.id === 'auto' ? AUTO_NOTE : TARGET_NOTE)}
    </>
  );
}

/** 弹出面板里的完整编辑：分组说明、启用、切换方式、已选、加入。 */
function TargetPicker({
  def,
  config,
  onChange,
}: {
  def: GroupDef;
  config: GroupConfig;
  onChange: (next: GroupConfig) => void;
}) {
  const help = useHelp();
  const hasChain = config.targets.some(isChainKey);
  return (
    <div className="stp-picker" tabIndex={-1} ref={autofocus}>
      <header className="stp-picker-head">
        <GroupGlyph glyph={def.glyph} />
        <span className="stp-picker-title">
          <b>{def.name}</b>
          {help.toggle(matchText(def), def.name)}
        </span>
        {def.toggle && (
          <Switch checked={config.on} label={`启用${def.name}`} onChange={on => onChange({ ...config, on })} />
        )}
      </header>
      <div className="stp-picker-note">{help.note(<p>{def.note}</p>)}</div>
      {config.on && (
        <div className="stp-picker-body">
          {def.id !== 'auto' && hasChain && <ModeRow config={config} onChange={onChange} />}
          <TargetsLabel def={def} />
          <ChosenList def={def} config={config} onChange={onChange} />
          <AvailableList def={def} config={config} onChange={onChange} />
        </div>
      )}
      {!isBase(def.id, config) && (
        <footer className="stp-picker-foot">
          <button type="button" className="sub-text-action" onClick={() => onChange(restore(def))}>
            恢复默认
          </button>
        </footer>
      )}
    </div>
  );
}

/* ── 弹出面板定位：点谁贴着谁；pick= 参数在挂载后自动打开一组，便于截图 ── */

type PopKind = 'picker' | 'menu' | 'add';

function usePopover(initialPick: GroupId | null, kind: PopKind = 'picker') {
  const anchor = useRef<HTMLElement | null>(null);
  const [state, setState] = useState<{ id: GroupId; kind: PopKind } | null>(null);
  const open = (id: GroupId, element: HTMLElement, as: PopKind = kind) => {
    anchor.current = element;
    setState({ id, kind: as });
  };
  useLayoutEffect(() => {
    if (!initialPick) return;
    const frame = requestAnimationFrame(() => {
      const element = document.querySelector<HTMLElement>(`[data-stp-open="${initialPick}"]`);
      if (!element) return;
      anchor.current = element;
      setState({ id: initialPick, kind });
    });
    return () => cancelAnimationFrame(frame);
  }, [initialPick, kind]);
  return { anchor: anchor as RefObject<HTMLElement | null>, state, open, close: () => setState(null) };
}

type Popover = ReturnType<typeof usePopover>;

function PickerPopover({
  anchor,
  def,
  config,
  onChange,
  onClose,
}: {
  anchor: RefObject<HTMLElement | null>;
  def: GroupDef;
  config: GroupConfig;
  onChange: (next: GroupConfig) => void;
  onClose: () => void;
}) {
  return (
    <DialogLayer mode="popover" align="end" anchor={anchor} label={`${def.name}的去向`} onClose={onClose}>
      <div className="dialog-surface stp-pop">
        <TargetPicker def={def} config={config} onChange={onChange} />
      </div>
    </DialogLayer>
  );
}

function QuickMenu({
  anchor,
  def,
  config,
  onChange,
  onMore,
  onClose,
}: {
  anchor: RefObject<HTMLElement | null>;
  def: GroupDef;
  config: GroupConfig;
  onChange: (next: GroupConfig) => void;
  onMore: () => void;
  onClose: () => void;
}) {
  const single = config.on && config.targets.length === 1 ? config.targets[0] : null;
  const pickOne = (key: TargetKey) => {
    onChange(only(def, config, key));
    onClose();
  };
  const item = (key: TargetKey) => (
    <button
      key={key}
      type="button"
      role="menuitemradio"
      aria-checked={single === key}
      className="stp-menu-item"
      onClick={() => pickOne(key)}
    >
      <span className="stp-menu-check" aria-hidden="true">
        {single === key ? (
          <Icon of="check" size={12} />
        ) : config.on && config.targets.length > 1 && config.targets.includes(key) ? (
          <i className="stp-menu-n">{config.targets.indexOf(key) + 1}</i>
        ) : null}
      </span>
      <TargetName target={key} />
      {isChainKey(key) ? <UnlockNote chain={chainOfKey(key)} unlock={def.unlock} /> : <span />}
    </button>
  );
  return (
    <DialogLayer mode="popover" align="end" anchor={anchor} label={`${def.name}的去向`} onClose={onClose}>
      <div className="dialog-surface stp-pop stp-menu" role="menu" tabIndex={-1} ref={autofocus}>
        {def.toggle && (
          <div className="stp-menu-switch">
            <span>启用</span>
            <Switch checked={config.on} label={`启用${def.name}`} onChange={on => onChange({ ...config, on })} />
          </div>
        )}
        {config.on &&
          (def.id === 'auto' ? (
            <div className="stp-menu-group">
              <button type="button" className="stp-menu-item" onClick={onMore}>
                <span className="stp-menu-check" />
                <span>选择参与测速的线路…</span>
                <span />
              </button>
            </div>
          ) : (
            <>
              <div className="stp-menu-group">{SPECIALS_FOR(def).map(item)}</div>
              {APPS.map(app => (
                <div key={app.id} className="stp-menu-group">
                  <span className="stp-menu-label">{app.label}</span>
                  {app.chains.map(chain => item(chainKey(chain.id)))}
                </div>
              ))}
              <div className="stp-menu-group">
                <button type="button" className="stp-menu-item" onClick={onMore}>
                  <span className="stp-menu-check" aria-hidden="true">
                    {config.targets.length > 1 && <Icon of="check" size={12} />}
                  </span>
                  <span>多条线路…</span>
                  <span />
                </button>
              </div>
            </>
          ))}
      </div>
    </DialogLayer>
  );
}

/** 点去向先出快速菜单（单选一条）；「多条线路…」再换成完整编辑。 */
function Popovers({
  pop,
  template,
  onChange,
}: {
  pop: Popover;
  template: TemplateConfig;
  onChange: BodyProps['onChange'];
}) {
  if (!pop.state) return null;
  const { id, kind } = pop.state;
  const def = groupById(id);
  return kind === 'menu' ? (
    <QuickMenu
      anchor={pop.anchor}
      def={def}
      config={template[id]}
      onChange={next => onChange(id, next)}
      onMore={() => pop.open(id, pop.anchor.current!, 'picker')}
      onClose={pop.close}
    />
  ) : (
    <PickerPopover
      anchor={pop.anchor}
      def={def}
      config={template[id]}
      onChange={next => onChange(id, next)}
      onClose={pop.close}
    />
  );
}

/* ══ 编辑器外壳 ══ */

export interface EditorProps {
  option: Option;
  saved: TemplateConfig;
  onSave: (next: TemplateConfig) => void;
  onClose: () => void;
  initialPick: GroupId | null;
}

interface BodyProps {
  template: TemplateConfig;
  changed: (id: GroupId) => boolean;
  onChange: (id: GroupId, next: GroupConfig) => void;
  initialPick: GroupId | null;
}

export function TemplateEditor({ option, saved, onSave, onClose, initialPick }: EditorProps) {
  const [draft, setDraft] = useState(saved);
  const help = useHelp();
  const dirty = GROUPS.some(group => !sameConfig(draft[group.id], saved[group.id]));
  const props: BodyProps = {
    template: draft,
    changed: id => !sameConfig(draft[id], saved[id]),
    onChange: (id, next) => setDraft(current => ({ ...current, [id]: next })),
    initialPick,
  };
  const body: Record<Option, ReactNode> = {
    a: <ListBody {...props} />,
    b: <ListBody {...props} twoColumns />,
    c: <SplitBody {...props} />,
  };
  return (
    <DialogLayer label="订阅模板" onClose={onClose} className="stp-layer">
      <section className={`dialog-surface stp-dialog stp-${option}`}>
        <header className="stp-head">
          <b>订阅模板</b>
          {help.toggle('SubBoost 标准版 · 所有用户共用', '订阅模板')}
          {help.note(TEMPLATE_NOTE)}
        </header>
        <div className="stp-body" tabIndex={-1} ref={autofocus}>
          {body[option]}
        </div>
        <footer className="stp-foot">
          <button
            type="button"
            className="sub-text-action"
            disabled={GROUPS.every(group => isBase(group.id, draft[group.id]))}
            onClick={() => setDraft(baseTemplate())}
          >
            全部恢复默认
          </button>
          <span className="stp-foot-gap" />
          <DialogClose className="btn">取消</DialogClose>
          <button
            type="button"
            className="btn primary"
            disabled={!dirty}
            onClick={() => {
              onSave(draft);
              onClose();
            }}
          >
            保存
          </button>
        </footer>
      </section>
    </DialogLayer>
  );
}

/* ══ 读数栏：三个关键分组，与用户页读数栏同一写法——小字标签在上，去向在下 ══ */

function KeyCell({
  def,
  config,
  changed,
  open,
  onOpen,
}: {
  def: GroupDef;
  config: GroupConfig;
  changed: boolean;
  open: boolean;
  onOpen: (element: HTMLElement) => void;
}) {
  const help = useHelp();
  return (
    <div className={`stp-key stp-key-${def.id}`}>
      <span className="stp-key-label">
        <GroupGlyph glyph={def.glyph} />
        {help.toggle(def.name, def.name)}
        <Dirty on={changed} />
      </span>
      <button
        type="button"
        data-stp-open={def.id}
        className={`stp-key-value${open ? ' open' : ''}`}
        aria-haspopup="menu"
        onClick={event => onOpen(event.currentTarget)}
      >
        <Destination def={def} config={config} variant="hero" />
        <PopupGlyph />
      </button>
      {help.note(<p>{def.note}</p>)}
    </div>
  );
}

function KeyBand({
  template,
  changed,
  pop,
}: {
  template: TemplateConfig;
  changed: BodyProps['changed'];
  pop: Popover;
}) {
  return (
    <div className="stp-keys">
      {KEY_GROUPS.map(id => (
        <KeyCell
          key={id}
          def={groupById(id)}
          config={template[id]}
          changed={changed(id)}
          open={pop.state?.id === id}
          onOpen={element => pop.open(id, element, 'menu')}
        />
      ))}
    </div>
  );
}

/* ══ A 读数栏 + 列表 / B 读数栏 + 两列 ══
   上面三个关键分组；下面每组一行：彩色图形、名称 (!)、去向。点去向弹出菜单，单选一条即完成，多条线路在菜单末尾。
   B 把同样的行排成两列，整页不用滚动。 */

function GroupRow({
  def,
  config,
  changed,
  open,
  onOpen,
}: {
  def: GroupDef;
  config: GroupConfig;
  changed: boolean;
  open: boolean;
  onOpen: (element: HTMLElement) => void;
}) {
  const help = useHelp();
  return (
    <div className="stp-row">
      <GroupGlyph glyph={def.glyph} />
      <span className="stp-row-name">
        {help.toggle(def.name, def.name)}
        <Dirty on={changed} />
      </span>
      <button
        type="button"
        data-stp-open={def.id}
        className={`stp-row-value${open ? ' open' : ''}`}
        aria-haspopup="menu"
        onClick={event => onOpen(event.currentTarget)}
      >
        <Destination def={def} config={config} />
        <PopupGlyph />
      </button>
      {help.note(<p>{def.note}</p>)}
    </div>
  );
}

function SectionTitle({ section }: { section: Section }) {
  const help = useHelp();
  const note = SECTION_NOTE[section.id];
  return (
    <>
      <h3>{note ? help.toggle(section.label, section.label) : section.label}</h3>
      {note && help.note(note)}
    </>
  );
}

function ListBody({
  template,
  changed,
  onChange,
  initialPick,
  twoColumns = false,
}: BodyProps & { twoColumns?: boolean }) {
  const pop = usePopover(initialPick, 'menu');
  return (
    <>
      <KeyBand template={template} changed={changed} pop={pop} />
      <div className={`stp-groups${twoColumns ? ' two' : ''}`}>
        {OTHER_SECTIONS.map(section => (
          <section key={section.id} className={`stp-section stp-section-${section.id}`}>
            <SectionTitle section={section} />
            <div className="stp-rows">
              {section.groups.map(id => (
                <GroupRow
                  key={id}
                  def={groupById(id)}
                  config={template[id]}
                  changed={changed(id)}
                  open={pop.state?.id === id}
                  onOpen={element => pop.open(id, element, 'menu')}
                />
              ))}
            </div>
          </section>
        ))}
      </div>
      <Popovers pop={pop} template={template} onChange={onChange} />
    </>
  );
}

/* ══ C 两栏 ══
   macOS 设置式：左栏按同样的顺序列出分组（三个关键分组在最上），右栏是所选分组的设置框。 */

function SplitBody({ template, changed, onChange, initialPick }: BodyProps) {
  const narrow = useNarrow();
  const [selected, setSelected] = useState<GroupId>(initialPick ?? 'final');
  const [detail, setDetail] = useState(!narrow || initialPick !== null);
  const showList = !narrow || !detail;
  const showDetail = !narrow || detail;
  return (
    <div className="stp-split">
      {showList && (
        <nav className="stp-side" aria-label="代理组">
          {SECTIONS.map(section => (
            <div key={section.id} className="stp-side-group">
              <h3>{section.label}</h3>
              {section.groups.map(id => {
                const item = groupById(id);
                return (
                  <button
                    key={id}
                    type="button"
                    className={`stp-side-row${id === selected && !narrow ? ' on' : ''}`}
                    aria-current={id === selected ? 'true' : undefined}
                    onClick={() => {
                      setSelected(id);
                      setDetail(true);
                    }}
                  >
                    <GroupGlyph glyph={item.glyph} />
                    <span className="stp-side-name">
                      {item.name}
                      <Dirty on={changed(id)} />
                    </span>
                    {narrow && (
                      <>
                        <Destination def={item} config={template[id]} />
                        <span className="stp-chevron" aria-hidden="true">
                          <Icon of="chevronDown" size={14} />
                        </span>
                      </>
                    )}
                  </button>
                );
              })}
            </div>
          ))}
        </nav>
      )}
      {showDetail && (
        <GroupDetail
          key={selected}
          def={groupById(selected)}
          config={template[selected]}
          onBack={narrow ? () => setDetail(false) : undefined}
          onChange={next => onChange(selected, next)}
        />
      )}
    </div>
  );
}

function GroupDetail({
  def,
  config,
  onBack,
  onChange,
}: {
  def: GroupDef;
  config: GroupConfig;
  onBack?: () => void;
  onChange: (next: GroupConfig) => void;
}) {
  const help = useHelp();
  const pop = usePopover(null, 'add');
  const hasChain = config.targets.some(isChainKey);
  return (
    <div className="stp-detail">
      {onBack && (
        <button type="button" className="stp-back" onClick={onBack}>
          <Icon of="back" size={14} />
          代理组
        </button>
      )}
      <header className="stp-detail-head">
        <GroupGlyph glyph={def.glyph} />
        <span className="stp-detail-title">
          <b>{def.name}</b>
          {help.toggle(matchText(def), def.name)}
        </span>
        {!isBase(def.id, config) && (
          <button type="button" className="sub-text-action" onClick={() => onChange(restore(def))}>
            恢复默认
          </button>
        )}
      </header>
      {help.note(<p>{def.note}</p>)}
      {def.toggle && (
        <div className="stp-cells">
          <div className="stp-cell stp-field">
            <span>启用</span>
            <Switch checked={config.on} label={`启用${def.name}`} onChange={on => onChange({ ...config, on })} />
          </div>
        </div>
      )}
      {config.on && (
        <>
          {def.id !== 'auto' && hasChain && <ModeRow config={config} onChange={onChange} boxed />}
          <TargetsLabel def={def} />
          <ChosenList def={def} config={config} onChange={onChange} />
          {!(def.id === 'auto' && config.targets.length === 0) && (
            <button
              type="button"
              className="stp-add-button"
              onClick={event => pop.open(def.id, event.currentTarget, 'add')}
            >
              <Icon of="plus" size={12} />
              添加
            </button>
          )}
        </>
      )}
      <details className="stp-yaml">
        <summary>生成结果</summary>
        <YamlPreview def={def} config={config} />
      </details>
      {pop.state && (
        <DialogLayer mode="popover" anchor={pop.anchor} label={`添加到${def.name}`} onClose={pop.close}>
          <div className="dialog-surface stp-pop">
            <div className="stp-picker" tabIndex={-1} ref={autofocus}>
              <div className="stp-picker-body">
                <AvailableList
                  def={def}
                  config={config}
                  onChange={next => {
                    onChange(next);
                    pop.close();
                  }}
                />
              </div>
            </div>
          </div>
        </DialogLayer>
      )}
    </div>
  );
}

function YamlPreview({ def, config }: { def: GroupDef; config: GroupConfig }) {
  const [user, setUser] = useState(SELECTED_USER);
  return (
    <div className="stp-yaml-body">
      <label className="stp-yaml-user">
        用户
        <select value={user} onChange={event => setUser(event.target.value)}>
          {USERS.map(item => (
            <option key={item} value={item}>
              {item}
            </option>
          ))}
        </select>
      </label>
      <pre>{groupYaml(def, config, user).join('\n')}</pre>
    </div>
  );
}

/* ══ 订阅弹窗里的入口 ══ */

export function TemplateLink({ saved, onOpen }: { saved: TemplateConfig; onOpen: () => void }) {
  const changed = GROUPS.filter(group => !isBase(group.id, saved[group.id])).length;
  return (
    <span className="stp-sub-slot">
      模板
      <button type="button" className="stp-template-link" onClick={onOpen}>
        SubBoost 标准版{changed > 0 && ` · 已调整 ${changed} 组`}
        <Icon of="chevronDown" size={12} />
      </button>
      · 适用 Mihomo / Clash Meta
    </span>
  );
}
