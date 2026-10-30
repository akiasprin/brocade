// Mockup: 单个面板的规范样式，以机器详情「运行状态」为例。Synthetic data only; never imported by the app.
// CurrentPanel 按生产 RuntimeCard 的结构和类名复刻，样式全部来自 src/styles.css；方案 A–D 只用本文件的结构
// 与 panel-spec-mockup.css 的 ps- 样式。
import type { ReactNode } from 'react';
import { Icon, PanelTitle } from '../../src/ui/icons';
import {
  EXTENSION_WORD,
  STATE_WORD,
  type Finding,
  type ItemState,
  type Reading,
  type Source,
  type StateItem,
} from './panel-spec-mockup-data';

export type Option = 'a' | 'b' | 'c' | 'd';

type GlyphKind = 'ok' | 'off' | 'warn' | 'bad' | 'chev';

/* SF Symbols 里对应的几个符号：checkmark.circle.fill、minus.circle.fill、exclamationmark.triangle.fill、
   exclamationmark.octagon.fill、chevron.right。 */
export function Glyph({ kind, size = 14 }: { kind: GlyphKind; size?: number }) {
  return (
    <svg className={`ps-glyph ps-glyph-${kind}`} width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
      {kind === 'ok' && (
        <>
          <circle cx="8" cy="8" r="7.2" fill="currentColor" />
          <path
            d="M4.8 8.3 L7 10.4 L11.2 5.9"
            fill="none"
            stroke="#fff"
            strokeWidth="1.7"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </>
      )}
      {kind === 'off' && (
        <>
          <circle cx="8" cy="8" r="7.2" fill="currentColor" />
          <path d="M4.9 8 H11.1" stroke="#fff" strokeWidth="1.7" strokeLinecap="round" />
        </>
      )}
      {kind === 'warn' && (
        <>
          <path
            d="M6.7 2.2 C7.3 1.2 8.7 1.2 9.3 2.2 L15.1 12.3 C15.7 13.3 15 14.5 13.8 14.5 H2.2 C1 14.5 0.3 13.3 0.9 12.3 Z"
            fill="currentColor"
          />
          <path d="M8 5.6 V9.3" stroke="#fff" strokeWidth="1.7" strokeLinecap="round" />
          <circle cx="8" cy="11.8" r="1" fill="#fff" />
        </>
      )}
      {kind === 'bad' && (
        <>
          <path
            d="M5.3 1.3 H10.7 L14.7 5.3 V10.7 L10.7 14.7 H5.3 L1.3 10.7 V5.3 Z"
            fill="currentColor"
            stroke="currentColor"
            strokeWidth="1.2"
            strokeLinejoin="round"
          />
          <path d="M8 4.6 V8.9" stroke="#fff" strokeWidth="1.7" strokeLinecap="round" />
          <circle cx="8" cy="11.4" r="1" fill="#fff" />
        </>
      )}
      {kind === 'chev' && (
        <path
          d="M6 3.2 L10.6 8 L6 12.8"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.9"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      )}
    </svg>
  );
}

const GLYPH_OF: Record<ItemState, GlyphKind> = { present: 'ok', disabled: 'off', dirty: 'warn', unknown: 'off' };

function titleOf(r: Reading) {
  return [`${r.value}${r.unit ? ` ${r.unit}` : ''}`, r.sub, r.tail].filter(Boolean).join(' · ');
}

/** 一个读数：主值、单位、第二取值、附加读数。各方案只改样式，不改这层结构。 */
function Val({ r, glyph = 12 }: { r: Reading; glyph?: number }) {
  const cls = ['ps-val', r.hot && 'is-hot', r.dim && 'is-dim', r.code && 'is-code', r.link && 'is-link']
    .filter(Boolean)
    .join(' ');
  return (
    <span className={cls} title={titleOf(r)}>
      {r.hot && <Glyph kind="warn" size={glyph} />}
      <span className="ps-main">{r.value}</span>
      {r.unit && <span className="ps-unit">{r.unit}</span>}
      {r.sub && <span className="ps-sub">{r.sub}</span>}
      {r.tail && <span className="ps-tail">{r.tail}</span>}
      {r.link && <Glyph kind="chev" size={Math.max(9, glyph - 2)} />}
    </span>
  );
}

/** 产物与扩展的状态。mark：word 只写状态词，异常加三角；glyph 每项带系统色符号；dot 用状态点。 */
function StateText({
  item,
  words,
  mark,
}: {
  item: StateItem;
  words: Record<ItemState, string>;
  mark: 'word' | 'glyph' | 'dot';
}) {
  return (
    <span className={`ps-state is-${item.state}`}>
      {mark === 'glyph' && <Glyph kind={GLYPH_OF[item.state]} size={13} />}
      {mark === 'word' && item.state === 'dirty' && <Glyph kind="warn" size={12} />}
      {mark === 'dot' && <i className="ps-dot" />}
      {words[item.state]}
    </span>
  );
}

interface Row {
  label: string;
  value: ReactNode;
}

function rowsOf(source: Source, mark: 'word' | 'glyph' | 'dot', glyph = 12, skip: string[] = []): Row[] {
  const rows: Row[] = source.readings
    .filter(r => !skip.includes(r.label))
    .map(r => ({ label: r.label, value: <Val r={r} glyph={glyph} /> }));
  for (const item of source.artifacts ?? [])
    rows.push({ label: item.name, value: <StateText item={item} words={STATE_WORD} mark={mark} /> });
  for (const item of source.extensions ?? [])
    rows.push({ label: item.name, value: <StateText item={item} words={EXTENSION_WORD} mark={mark} /> });
  return rows;
}

const reading = (source: Source, label: string) => source.readings.find(r => r.label === label)!;

function Notes({ list, className, glyph }: { list: Finding[]; className: string; glyph: number }) {
  return (
    <>
      {list.map((finding, index) => (
        <p key={index} className={`${className} is-${finding.tone}`}>
          <Glyph kind={finding.tone} size={glyph} />
          <span>{finding.body}</span>
        </p>
      ))}
    </>
  );
}

/* ══ A 分组表单：系统设置 ══ */

function PanelA({ sources, findings }: { sources: Source[]; findings: Finding[] }) {
  return (
    <section className="ps-a">
      <h3 className="ps-a-title">运行状态</h3>
      {findings.length > 0 && (
        <div className="ps-a-box ps-a-alerts">
          <Notes list={findings} className="ps-a-alert" glyph={15} />
        </div>
      )}
      <div className="ps-a-groups">
        {sources.map(source => (
          <section key={source.id} className="ps-a-group">
            <header className="ps-a-gh">
              <h4>{source.name}</h4>
              <span>
                {source.ago}
                {source.verb}
              </span>
            </header>
            <ul className="ps-a-box">
              {rowsOf(source, 'word').map(row => (
                <li key={row.label} className="ps-a-row">
                  <span className="ps-a-k">{row.label}</span>
                  {row.value}
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </section>
  );
}

/* ══ B 规格表：apple.com 技术规格 ══ */

function PanelB({ sources, findings }: { sources: Source[]; findings: Finding[] }) {
  return (
    <section className="ps-b">
      <h3 className="ps-b-title">运行状态</h3>
      {sources.map(source => (
        <section key={source.id} className="ps-b-sec">
          <header className="ps-b-sh">
            <h4>{source.name}</h4>
            <p>
              {source.ago}
              {source.verb}
            </p>
          </header>
          <dl className="ps-b-cells">
            {source.readings.map(r => (
              <div key={r.label} className={r.wide ? 'ps-b-c is-wide' : 'ps-b-c'}>
                <dt>{r.label}</dt>
                <dd>
                  <Val r={r} glyph={14} />
                </dd>
              </div>
            ))}
            {source.artifacts && (
              <div className="ps-b-c is-wide">
                <dt>产物</dt>
                <dd className="ps-b-items">
                  {source.artifacts.map(item => (
                    <span key={item.name} className={`ps-b-item is-${item.state}`} title={STATE_WORD[item.state]}>
                      <Glyph kind={GLYPH_OF[item.state]} size={14} />
                      {item.name}
                    </span>
                  ))}
                </dd>
              </div>
            )}
            {source.extensions && (
              <div className="ps-b-c">
                <dt>扩展应用</dt>
                <dd className="ps-b-items">
                  {source.extensions.map(item => (
                    <span key={item.name} className={`ps-b-item is-${item.state}`} title={EXTENSION_WORD[item.state]}>
                      <Glyph kind={GLYPH_OF[item.state]} size={14} />
                      {item.name}
                    </span>
                  ))}
                </dd>
              </div>
            )}
          </dl>
        </section>
      ))}
      {findings.length > 0 && (
        <footer className="ps-b-notes">
          <Notes list={findings} className="ps-b-note" glyph={15} />
        </footer>
      )}
    </section>
  );
}

/* ══ C 摘要卡：「健康」摘要 ══ */

function Stat({ value, unit, caption }: { value: ReactNode; unit?: string; caption: string }) {
  return (
    <div className="ps-c-stat">
      <span className="ps-c-num">
        {value}
        {unit && <small>{unit}</small>}
      </span>
      <span className="ps-c-cap">{caption}</span>
    </div>
  );
}

function heroOf(source: Source): { stats: ReactNode; skip: string[] } {
  if (source.id === 'agent') {
    const parts = [...reading(source, '已运行').value.matchAll(/(\d+)\s*(\S+)/g)];
    return {
      stats: (
        <Stat
          value={parts.map(([, n, u]) => (
            <span key={u}>
              {n}
              <small>{u}</small>
            </span>
          ))}
          caption="已运行"
        />
      ),
      skip: ['已运行'],
    };
  }
  if (source.id === 'host') {
    const cpu = reading(source, 'CPU');
    const [cores, coreUnit] = (cpu.sub ?? '').split(' ');
    const [mem, memUnit] = reading(source, '内存').value.split(' ');
    const [disk, diskUnit] = reading(source, '磁盘').value.split(' ');
    return {
      stats: (
        <>
          <Stat value={cores} unit={coreUnit} caption="CPU" />
          <Stat value={mem} unit={memUnit} caption="内存" />
          <Stat value={disk} unit={diskUnit} caption="磁盘" />
        </>
      ),
      skip: ['内存', '磁盘'],
    };
  }
  const from = reading(source, '来自');
  return { stats: <Stat value={from.value} caption={`发布 · ${from.sub}`} />, skip: ['来自'] };
}

function PanelC({ sources, findings }: { sources: Source[]; findings: Finding[] }) {
  return (
    <section className="ps-c">
      <h3 className="ps-c-title">运行状态</h3>
      {findings.length > 0 && (
        <div className="ps-c-card ps-c-alerts">
          <Notes list={findings} className="ps-c-alert" glyph={16} />
        </div>
      )}
      <div className="ps-c-grid">
        {sources.map(source => {
          const hero = heroOf(source);
          const rows = rowsOf(source, 'glyph', 12, hero.skip).map(row =>
            source.id === 'host' && row.label === 'CPU'
              ? { label: 'CPU', value: <Val r={{ ...reading(source, 'CPU'), sub: undefined }} /> }
              : row,
          );
          return (
            <article key={source.id} className="ps-c-card ps-c-src" data-src={source.id}>
              <header className="ps-c-head">
                <Icon of={source.icon} size={15} className="ps-c-ic" />
                <h4>{source.name}</h4>
                <span className="ps-c-meta">
                  {source.ago}
                  {source.verb}
                </span>
              </header>
              <div className="ps-c-hero">{hero.stats}</div>
              <dl className="ps-c-rows">
                {rows.map(row => (
                  <div key={row.label} className="ps-c-row">
                    <dt>{row.label}</dt>
                    <dd>{row.value}</dd>
                  </div>
                ))}
              </dl>
            </article>
          );
        })}
      </div>
    </section>
  );
}

/* ══ D 检查器：Xcode / Finder 简介 ══ */

function PanelD({ sources, findings }: { sources: Source[]; findings: Finding[] }) {
  return (
    <section className="ps-d">
      <header className="ps-d-head">
        <h3>运行状态</h3>
      </header>
      <div className="ps-d-cols">
        {sources.map(source => (
          <section key={source.id} className="ps-d-sec">
            <h4>
              {source.name}
              <span>
                {source.ago}
                {source.verb}
              </span>
            </h4>
            <dl className="ps-d-form">
              {rowsOf(source, 'dot', 11).map(row => (
                <div key={row.label} className="ps-d-row">
                  <dt>{row.label}</dt>
                  <dd>{row.value}</dd>
                </div>
              ))}
            </dl>
          </section>
        ))}
      </div>
      {findings.length > 0 && (
        <footer className="ps-d-issues">
          <Notes list={findings} className="ps-d-issue" glyph={12} />
        </footer>
      )}
    </section>
  );
}

export function OptionPanel({ option, sources, findings }: { option: Option; sources: Source[]; findings: Finding[] }) {
  if (option === 'a') return <PanelA sources={sources} findings={findings} />;
  if (option === 'b') return <PanelB sources={sources} findings={findings} />;
  if (option === 'c') return <PanelC sources={sources} findings={findings} />;
  return <PanelD sources={sources} findings={findings} />;
}

/* ══ 现状：按生产 RuntimeCard 的结构复刻 ══ */

const CHIP_CLASS: Record<ItemState, string> = {
  present: 'st-succeeded',
  disabled: 'st-skipped',
  dirty: 'st-halted',
  unknown: 'st-pending',
};
const CHIP_ICON = { present: 'check', disabled: 'dash', dirty: 'neq' } as const;

function Chip({ item }: { item: StateItem }) {
  const icon = item.state === 'unknown' ? null : CHIP_ICON[item.state];
  return (
    <span className={`st nd-rt-chip ${CHIP_CLASS[item.state]}`}>
      {icon && <Icon of={icon} size={9} className="nd-rt-chip-ic" />}
      {item.code}
    </span>
  );
}

function CurrentVal({ r }: { r: Reading }) {
  const main = r.hot ? (
    <span className="hot">
      <Icon of="warn" size={10} className="nd-rt-hot-ic" />
      {r.value}
    </span>
  ) : r.code ? (
    <span className="mono">{r.value}</span>
  ) : r.dim ? (
    <span className="dim">{r.value}</span>
  ) : (
    r.value
  );
  const [tailWord, tailValue] = (r.tail ?? '').split(' ');
  const body = (
    <>
      {main}
      {r.unit && <span className="nd-rt-u">{r.unit}</span>}
      {r.sub && <span className="nd-rt-q">{r.sub}</span>}
      {r.tail && (
        <>
          <span className="dim"> · {tailWord} </span>
          {tailValue}
        </>
      )}
    </>
  );
  if (r.link)
    return (
      <button type="button" className="nd-public-ip-metric" aria-expanded={false}>
        {body}
      </button>
    );
  return <span title={titleOf(r)}>{body}</span>;
}

export function CurrentPanel({ sources, findings }: { sources: Source[]; findings: Finding[] }) {
  return (
    <div className="nd-page">
      <section className="nd-tab-observed">
        <div className="panel nd-runtime">
          <header>
            <PanelTitle of="observe">运行状态</PanelTitle>
          </header>
          {sources.map(source => (
            <section key={source.id} className="nd-rt-band">
              <div className="nd-rt-bl">
                <b>
                  <Icon of={source.icon} size={12} className="nd-rt-bl-ic" />
                  {source.code}
                </b>
                <i>
                  <span className="ago">{source.ago}</span> {source.verb}
                </i>
              </div>
              <dl className="nd-rt-cells">
                {source.readings.map(r => (
                  <div key={r.label} className={r.span ? `nd-rt-c ${r.span}` : 'nd-rt-c'}>
                    <dt>{r.label}</dt>
                    <dd>
                      <CurrentVal r={r} />
                    </dd>
                  </div>
                ))}
                {source.artifacts && (
                  <div className="nd-rt-c w3">
                    <dt>产物</dt>
                    <dd>
                      <span className="nd-rt-chips">
                        {source.artifacts.map(item => (
                          <Chip key={item.code} item={item} />
                        ))}
                      </span>
                    </dd>
                  </div>
                )}
                {source.extensions && (
                  <div className="nd-rt-c">
                    <dt>扩展应用</dt>
                    <dd>
                      {source.extensions.map(item => (
                        <Chip key={item.code} item={item} />
                      ))}
                    </dd>
                  </div>
                )}
              </dl>
            </section>
          ))}
          {findings.map((finding, index) => (
            <p key={index} className={`rt-dx ${finding.tone}`}>
              {finding.body}
            </p>
          ))}
        </div>
      </section>
    </div>
  );
}
