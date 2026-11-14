import { useMemo, useRef, useState, type ReactNode } from 'react';
import type { IpMaintenancePolicy, MaintenanceBehavior, MaintenanceRange, MaintenanceRegion } from '../../api';
import { ChoiceSelect, MenuLayer, menuArrowKey } from '../../ui/choice-menu';
import { SegmentedControl, ErrorBox } from '../../ui/bits';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Icon } from '../../ui/icons';
import { REGION_GROUPS } from '../../ui/region-groups';
import { Flag, Switch } from './kit';
import { useCare } from './state';
import { cityName, countryName, isoOf, placeName, stateName } from './regions-zh';

/* 分组卡沿用机器配置页的标题带，行内左标签右控件。 */
export function Group({ title, footer, children }: { title?: ReactNode; footer?: ReactNode; children: ReactNode }) {
  return (
    <div className="care-group">
      {title && <h4 className="care-group-title">{title}</h4>}
      <div className="care-group-rows">{children}</div>
      {footer && <p className="care-group-foot">{footer}</p>}
    </div>
  );
}
export function Row({
  label,
  children,
  onClick,
  sub,
}: {
  label: ReactNode;
  children?: ReactNode;
  onClick?: () => void;
  sub?: ReactNode;
}) {
  const body = (
    <>
      <span className="care-row-label">
        {label}
        {sub && <small>{sub}</small>}
      </span>
      <span className="care-row-value">{children}</span>
    </>
  );
  return onClick ? (
    <button type="button" className="care-row link" onClick={onClick}>
      {body}
      <Icon of="chevronLeft" size={12} className="care-chevron" />
    </button>
  ) : (
    <div className="care-row">{body}</div>
  );
}

function NumberField({
  value,
  onChange,
  min,
  max,
  label,
  unit,
  wide = false,
}: {
  value: number;
  onChange: (value: number) => void;
  min: number;
  max: number;
  label: string;
  unit?: string;
  wide?: boolean;
}) {
  return (
    <span className="care-num-wrap">
      <input
        className={`care-num${wide ? ' wide' : ''}`}
        type="number"
        inputMode="numeric"
        aria-label={label}
        min={min}
        max={max}
        value={value}
        onChange={event => onChange(Number(event.target.value))}
      />
      {unit && <span className="care-unit">{unit}</span>}
    </span>
  );
}
function RangeField({
  value,
  onChange,
  label,
  min,
  max,
  unit,
}: {
  value: MaintenanceRange;
  onChange: (value: MaintenanceRange) => void;
  label: string;
  min: number;
  max: number;
  unit?: string;
}) {
  return (
    <span className="care-num-wrap">
      <input
        className="care-num"
        type="number"
        aria-label={`${label}下限`}
        min={min}
        max={max}
        value={value.min}
        onChange={event => onChange({ ...value, min: Number(event.target.value) })}
      />
      <span className="care-dash">–</span>
      <input
        className="care-num"
        type="number"
        aria-label={`${label}上限`}
        min={min}
        max={max}
        value={value.max}
        onChange={event => onChange({ ...value, max: Number(event.target.value) })}
      />
      {unit && <span className="care-unit">{unit}</span>}
    </span>
  );
}

/* ── 目标地区：首版实底菜单，按大洲分节，可搜索中英文城市名。 ── */
const CONTINENT_OF = new Map(REGION_GROUPS.flatMap(group => group.codes.map(code => [code, group] as const)));
export function RegionPicker({ value, onChange }: { value: string; onChange: (id: string) => void }) {
  const care = useCare();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const anchor = useRef<HTMLButtonElement>(null);
  const current = care.regions.find(r => r.id === value);
  const sections = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const hits = care.regions.filter(r => {
      if (!needle) return true;
      const place = placeName(r);
      return [place.city, place.country, r.city.replaceAll('_', ' '), r.country, r.name, stateName(r) ?? '']
        .join(' ')
        .toLowerCase()
        .includes(needle);
    });
    const groups = new Map<string, { label: string; order: number; items: MaintenanceRegion[] }>();
    for (const r of hits) {
      const group = CONTINENT_OF.get(isoOf(r.country));
      const key = group?.id ?? 'special';
      const entry = groups.get(key) ?? {
        label: group?.label ?? '其他',
        order: REGION_GROUPS.findIndex(g => g.id === key),
        items: [],
      };
      entry.items.push(r);
      groups.set(key, entry);
    }
    return [...groups.values()]
      .sort((a, b) => a.order - b.order)
      .map(g => ({
        ...g,
        items: g.items.sort(
          (a, b) =>
            countryName(a.country).localeCompare(countryName(b.country), 'zh') ||
            cityName(a).localeCompare(cityName(b), 'zh'),
        ),
      }));
  }, [care.regions, query]);
  const place = current ? placeName(current) : null;
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className="care-value-button"
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        <Flag code={current?.country} size={16} />
        <span>{place?.city ?? (value || '选择养护地区')}</span>
        {place && place.city !== place.country && <small>{place.country}</small>}
        <span className="choice-chevron" aria-hidden="true">
          ⌃<br />⌄
        </span>
      </button>
      {open && (
        <MenuLayer anchor={anchor} label="目标地区" onClose={() => setOpen(false)}>
          <div
            className="dialog-surface choice-menu care-region-menu"
            role="listbox"
            aria-label="目标地区"
            onKeyDown={menuArrowKey}
          >
            <div className="care-region-search">
              <Icon of="search" size={13} />
              <input
                autoFocus
                placeholder="搜索城市或国家"
                value={query}
                onChange={event => setQuery(event.target.value)}
                onKeyDown={event => {
                  if (event.key !== 'Enter') return;
                  event.preventDefault();
                  const first = sections[0]?.items[0];
                  if (first) {
                    onChange(first.id);
                    setOpen(false);
                  }
                }}
              />
              <span className="care-region-count">{care.regions.length} 个城市</span>
            </div>
            {sections.map(section => (
              <div role="group" aria-label={section.label} key={section.label}>
                <div className="choice-menu-section">{section.label}</div>
                {section.items.map(r => {
                  const p = placeName(r);
                  const on = r.id === value;
                  return (
                    <button
                      key={r.id}
                      type="button"
                      role="option"
                      aria-selected={on}
                      className={on ? 'on' : ''}
                      onClick={() => {
                        onChange(r.id);
                        setOpen(false);
                      }}
                    >
                      <Flag code={r.country} size={16} />
                      <span className="choice-menu-copy">
                        {p.city}
                        {p.city !== p.country && <em>{p.country}</em>}
                      </span>
                      {stateName(r) && <small>{stateName(r)}</small>}
                    </button>
                  );
                })}
              </div>
            ))}
            {!sections.length && <p className="care-region-none">没有匹配的城市</p>}
          </div>
        </MenuLayer>
      )}
    </>
  );
}

const INTERVALS = [10, 15, 20, 30, 45, 60, 90, 120, 180, 360, 720, 1440];
const COOLDOWNS = [15, 30, 60, 120, 240, 480];
const minutes = (n: number) => (n % 60 === 0 && n >= 60 ? `${n / 60} 小时` : `${n} 分钟`);

/* 模块比例：一根两色条，拖动分界。两个模块各自的开关在上一行。 */
function SplitSlider({
  policy,
  set,
}: {
  policy: IpMaintenancePolicy;
  set: (next: Partial<IpMaintenancePolicy>) => void;
}) {
  const both = policy.google && policy.trust;
  const google = both ? policy.google_percent : policy.google ? 100 : 0;
  return (
    <div className="care-split" data-disabled={!both || undefined}>
      <div className="care-split-bar" style={{ ['--g' as string]: `${google}%` }}>
        <input
          type="range"
          min="0"
          max="100"
          step="5"
          aria-label="Google 模块比例"
          disabled={!both}
          value={google}
          onChange={event => set({ google_percent: Number(event.target.value) })}
        />
      </div>
      <div className="care-split-legend">
        <span>
          <i data-module="google" />
          Google <b>{google}%</b>
        </span>
        <span>
          地区站点 <b>{100 - google}%</b>
          <i data-module="trust" />
        </span>
      </div>
    </div>
  );
}

export function PolicyGroups({ wide = false }: { wide?: boolean }) {
  const care = useCare();
  const [behavior, setBehavior] = useState(false);
  const [reset, setReset] = useState(false);
  const policy = care.policy;
  if (!policy) return null;
  const set = care.edit.set;
  const original = care.view?.default_policy;
  const custom =
    !!original &&
    (JSON.stringify(policy.behavior) !== JSON.stringify(original.behavior) ||
      policy.google_percent !== original.google_percent);
  const intervalOptions = INTERVALS.includes(policy.interval_minutes)
    ? INTERVALS
    : [...INTERVALS, policy.interval_minutes].sort((a, b) => a - b);
  const cooldownOptions = COOLDOWNS.includes(policy.cooldown_minutes)
    ? COOLDOWNS
    : [...COOLDOWNS, policy.cooldown_minutes].sort((a, b) => a - b);
  return (
    <div className={`care-policy${wide ? ' wide' : ''}`}>
      <fieldset className="care-policy-controls" disabled={!care.editable}>
        <Group title="目标">
          <Row label="地区">
            <RegionPicker value={policy.region} onChange={region => set({ region })} />
          </Row>
          <Row label="地址族">
            <SegmentedControl
              className="care-seg"
              value={policy.family}
              ariaLabel="养护地址族"
              options={[
                { value: 'v4', label: 'IPv4' },
                { value: 'v6', label: 'IPv6' },
                { value: 'dual', label: 'IPv4 + IPv6', disabled: !care.view?.dual_stack_supported },
              ]}
              onChange={family =>
                set({
                  family,
                  source_ip: null,
                  expected_public_ip: null,
                  source_ipv6: null,
                  expected_public_ipv6: null,
                })
              }
            />
          </Row>
          <Row label={policy.family === 'dual' ? 'IPv4 来源' : '来源地址'}>
            <input
              className="care-text"
              aria-label={policy.family === 'dual' ? 'IPv4 本机来源地址' : '本机来源地址'}
              placeholder="自动"
              value={policy.source_ip ?? ''}
              onChange={event => set({ source_ip: event.target.value.trim() || null })}
            />
          </Row>
          <Row label={policy.family === 'dual' ? 'IPv4 出口' : '预期出口'}>
            <input
              className="care-text"
              aria-label={policy.family === 'dual' ? 'IPv4 预期公网出口' : '预期公网出口'}
              placeholder="不检查"
              value={policy.expected_public_ip ?? ''}
              onChange={event => set({ expected_public_ip: event.target.value.trim() || null })}
            />
          </Row>
          {policy.family === 'dual' && (
            <>
              <Row label="IPv6 来源">
                <input
                  className="care-text"
                  aria-label="IPv6 本机来源地址"
                  placeholder="自动"
                  value={policy.source_ipv6 ?? ''}
                  onChange={event => set({ source_ipv6: event.target.value.trim() || null })}
                />
              </Row>
              <Row label="IPv6 出口">
                <input
                  className="care-text"
                  aria-label="IPv6 预期公网出口"
                  placeholder="不检查"
                  value={policy.expected_public_ipv6 ?? ''}
                  onChange={event => set({ expected_public_ipv6: event.target.value.trim() || null })}
                />
              </Row>
              <p className="care-note">两个出口并发执行，共用每日请求和流量额度。</p>
            </>
          )}
          {!care.view?.dual_stack_supported && <p className="care-note">升级 Agent 后可使用 IPv4 + IPv6。</p>}
        </Group>
        <Group title="节奏">
          <Row label="频率">
            <ChoiceSelect
              className="f care-choice"
              aria-label="执行频率"
              value={policy.interval_minutes}
              onChange={event => set({ interval_minutes: Number(event.target.value) })}
            >
              {intervalOptions.map(n => (
                <option
                  key={n}
                  value={n}
                  data-detail={`${policy.family === 'dual' ? '每出口' : ''}每天约 ${Math.round(1440 / n)} 轮`}
                >
                  每 {minutes(n)}
                </option>
              ))}
            </ChoiceSelect>
          </Row>
          <Row label="Google" sub="搜索、新闻、地图">
            <Switch label="Google 模块" checked={policy.google} onChange={google => set({ google })} />
          </Row>
          <Row label="地区站点" sub={care.region ? `${placeName(care.region).city}本地网站` : '本地网站'}>
            <Switch label="地区站点模块" checked={policy.trust} onChange={trust => set({ trust })} />
          </Row>
          <div className="care-row care-row-split">
            <SplitSlider policy={policy} set={set} />
          </div>
        </Group>
        <Group title="限额">
          <Row label="每日请求">
            <NumberField
              label="每日请求上限"
              value={policy.daily_requests}
              min={10}
              max={10000}
              unit="次"
              wide
              onChange={daily_requests => set({ daily_requests })}
            />
          </Row>
          <Row label="每日流量">
            <NumberField
              label="每日流量上限"
              value={policy.daily_download_mib}
              min={1}
              max={1024}
              unit="MiB"
              onChange={daily_download_mib => set({ daily_download_mib })}
            />
          </Row>
          <Row label="异常冷却">
            <ChoiceSelect
              className="f care-choice"
              aria-label="异常冷却"
              value={policy.cooldown_minutes}
              onChange={event => set({ cooldown_minutes: Number(event.target.value) })}
            >
              {cooldownOptions.map(n => (
                <option key={n} value={n}>
                  {minutes(n)}
                </option>
              ))}
            </ChoiceSelect>
          </Row>
          <Row label="失联后">
            <ChoiceSelect
              className="f care-choice"
              aria-label="失联后"
              value={policy.continue_offline ? 'continue' : 'pause'}
              onChange={event => set({ continue_offline: event.target.value === 'continue' })}
            >
              <option value="pause" data-detail="120 秒后">
                暂停
              </option>
              <option value="continue" data-detail="按最后确认的策略">
                继续执行
              </option>
            </ChoiceSelect>
          </Row>
        </Group>
      </fieldset>
      <Group>
        <Row label="行为参数" onClick={() => setBehavior(true)}>
          <span className="care-row-hint">{custom ? '已自定义' : 'IP-Sentinel 原版'}</span>
        </Row>
        <button
          type="button"
          className="care-row care-row-action"
          disabled={
            !care.editable ||
            !care.resetSupported ||
            care.edit.dirty ||
            !care.confirmed ||
            care.anyActive ||
            !care.dualIdleConfirmed ||
            care.pending.session ||
            care.first
          }
          title={!care.resetSupported ? '升级 Agent 后可清空养护会话和记录' : undefined}
          onClick={() => setReset(true)}
        >
          {care.pending.session ? '正在重置会话…' : '重置养护会话'}
        </button>
      </Group>
      {behavior && <BehaviorSheet onClose={() => setBehavior(false)} />}
      {reset && (
        <DialogLayer label="重置养护会话" onClose={() => setReset(false)}>
          <div className="dialog-surface sheet care-reset-sheet">
            <h3>重置养护会话</h3>
            <p>将清空这台机器的养护记录、检测结果、会话、冷却和养护计数。</p>
            <p>保留已保存的策略，并关闭自动养护。清空后无法撤销。</p>
            <div className="care-reset-actions">
              <button className="btn" onClick={() => setReset(false)}>
                取消
              </button>
              <button
                className="btn danger"
                onClick={() => {
                  setReset(false);
                  care.actions.resetSession();
                }}
              >
                确认重置
              </button>
            </div>
          </div>
        </DialogLayer>
      )}
    </div>
  );
}

/** 策略弹层的保存页脚，和滚动正文独立排布。 */
export function SaveBar() {
  const care = useCare();
  if (!care.edit.dirty && !care.edit.error) return null;
  return (
    <div className="care-savebar sheet">
      {care.edit.error ? <ErrorBox error={care.edit.error} /> : <span>有未保存的改动</span>}
      <button className="btn" disabled={care.edit.saving} onClick={care.edit.reset}>
        还原
      </button>
      <button
        className="btn primary"
        disabled={care.edit.saving || !care.edit.dirty || !care.region}
        onClick={care.edit.save}
      >
        {care.edit.saving ? '保存中…' : '保存'}
      </button>
    </div>
  );
}

/* ── 行为参数：深层设置收进一张单独的纸，改动仍进同一份草稿。 ── */
const PLATFORM_ACTIONS = ['搜索', '新闻', '地图', '生态'];
function BehaviorSheet({ onClose }: { onClose: () => void }) {
  const care = useCare();
  const policy = care.policy!;
  const b = policy.behavior;
  const set = <K extends keyof MaintenanceBehavior>(key: K, value: MaintenanceBehavior[K]) =>
    care.edit.set({ behavior: { ...b, [key]: value } });
  const original = care.view?.default_policy;
  const dwellTotal = b.trust_dwell.reduce((sum, band) => sum + band.weight, 0);
  return (
    <DialogLayer label="行为参数" onClose={onClose}>
      <section className="dialog-surface care-sheet care-behavior">
        <header className="care-sheet-head plain">
          <div>
            <h2>行为参数</h2>
            <span className="care-muted">与保存按钮一同生效</span>
          </div>
          <DialogClose className="care-sheet-close" aria-label="关闭">
            <Icon of="close" size={15} />
          </DialogClose>
        </header>
        <div className="care-sheet-body">
          <fieldset className="care-policy-controls" disabled={!care.editable}>
            <Group title="Google">
              <Row label="每轮动作">
                <RangeField
                  label="Google 动作"
                  value={b.google_actions}
                  min={1}
                  max={20}
                  onChange={v => set('google_actions', v)}
                  unit="个"
                />
              </Row>
              <Row label="页面停留">
                <RangeField
                  label="Google 停留"
                  value={b.google_dwell_secs}
                  min={5}
                  max={600}
                  onChange={v => set('google_dwell_secs', v)}
                  unit="秒"
                />
              </Row>
            </Group>
            <Group title="地区站点">
              <Row label="每轮动作">
                <RangeField
                  label="站点动作"
                  value={b.trust_actions}
                  min={1}
                  max={20}
                  onChange={v => set('trust_actions', v)}
                  unit="个"
                />
              </Row>
              {b.trust_dwell.map((band, index) => (
                <Row key={index} label={`停留 ${['短', '中', '长', '很长'][index] ?? index + 1}`}>
                  <RangeField
                    label={`站点停留 ${index + 1}`}
                    value={band.seconds}
                    min={1}
                    max={600}
                    unit="秒"
                    onChange={seconds =>
                      set(
                        'trust_dwell',
                        b.trust_dwell.map((item, n) => (n === index ? { ...item, seconds } : item)),
                      )
                    }
                  />
                  <NumberField
                    label={`站点停留 ${index + 1} 占比`}
                    value={band.weight}
                    min={0}
                    max={100}
                    unit="%"
                    onChange={weight =>
                      set(
                        'trust_dwell',
                        b.trust_dwell.map((item, n) => (n === index ? { ...item, weight } : item)),
                      )
                    }
                  />
                </Row>
              ))}
            </Group>
            {dwellTotal !== 100 && <p className="care-invalid">四档占比合计 {dwellTotal}%，需要等于 100%。</p>}
            <Group title="平台动作占比">
              <div className="care-matrix">
                <span />
                {PLATFORM_ACTIONS.map(name => (
                  <b key={name}>{name}</b>
                ))}
                {(['android_actions', 'apple_actions', 'desktop_actions'] as const).map((key, row) => (
                  <MatrixRow
                    key={key}
                    name={['Android', 'Apple', '桌面'][row]}
                    values={b[key]}
                    onChange={values => set(key, values)}
                  />
                ))}
              </div>
            </Group>
            <Group title="会话与时序">
              <Row label="Referer">
                <NumberField
                  label="Referer 比例"
                  value={b.referer_percent}
                  min={0}
                  max={100}
                  unit="%"
                  onChange={v => set('referer_percent', v)}
                />
              </Row>
              <Row label="会话保留">
                <NumberField
                  label="会话保留天数"
                  value={b.cookie_idle_days}
                  min={1}
                  max={30}
                  unit="天"
                  onChange={v => set('cookie_idle_days', v)}
                />
              </Row>
              <Row label="请求超时">
                <NumberField
                  label="请求超时"
                  value={b.request_timeout_secs}
                  min={3}
                  max={60}
                  unit="秒"
                  onChange={v => set('request_timeout_secs', v)}
                />
              </Row>
              <Row label="定时错峰">
                <NumberField
                  label="定时错峰"
                  value={b.timer_jitter_secs}
                  min={0}
                  max={180}
                  unit="秒"
                  onChange={v => set('timer_jitter_secs', v)}
                />
              </Row>
              <Row label="轮次错峰">
                <NumberField
                  label="轮次错峰"
                  value={b.runner_jitter_secs}
                  min={0}
                  max={179}
                  unit="秒"
                  onChange={v => set('runner_jitter_secs', v)}
                />
              </Row>
            </Group>
          </fieldset>
        </div>
        <footer className="care-sheet-actions">
          <button
            className="btn"
            disabled={!care.editable || !original}
            onClick={() =>
              original &&
              care.edit.set({ behavior: structuredClone(original.behavior), google_percent: original.google_percent })
            }
          >
            恢复原版
          </button>
          <span className="sp" />
          <DialogClose className="btn primary">完成</DialogClose>
        </footer>
      </section>
    </DialogLayer>
  );
}
function MatrixRow({
  name,
  values,
  onChange,
}: {
  name: string;
  values: number[];
  onChange: (values: number[]) => void;
}) {
  const total = values.reduce((sum, v) => sum + v, 0);
  return (
    <>
      <span className={total === 100 ? '' : 'care-invalid-text'}>{name}</span>
      {values.map((value, index) => (
        <input
          key={index}
          className="care-num"
          type="number"
          min="0"
          max="100"
          aria-label={`${name}${PLATFORM_ACTIONS[index]}占比`}
          value={value}
          onChange={event => onChange(values.map((v, n) => (n === index ? Number(event.target.value) : v)))}
        />
      ))}
    </>
  );
}
