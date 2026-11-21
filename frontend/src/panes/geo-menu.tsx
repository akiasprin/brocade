import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from 'react';
import { useQuery, type UseQueryResult } from '@tanstack/react-query';
import { getDomain } from 'tldts';
import { geoSuggestions, lookupGeosite, type GeoCatalog, type GeodataKind } from '../api/geodata';
import { DialogClose } from '../ui/dialog';
import { Icon } from '../ui/icons';
import { GeoGlyph } from './geo-glyph';

export { GeoGlyph } from './geo-glyph';
import {
  GEO_PAGE,
  GEO_WIDE_COUNT,
  attrHits,
  geoBlockReason,
  geoFamilies,
  geoIndex,
  letterOf,
  parseGeoQuery,
  regionName,
  searchGeo,
  type GeoFamily,
} from './geo-menu-model';

const fmt = (n: number) => n.toLocaleString('en-US');

type Row =
  | {
      t: 'option';
      value: string;
      /** 行内属性：点击直接加入 `code@attr`。 */
      attrs?: string[];
      count?: number;
      via?: string;
      tag?: { text: string; wide?: boolean };
      note?: string;
    }
  | { t: 'family'; family: GeoFamily };
type Section = { key: string; title: string; note?: string; rows: Row[]; after?: ReactNode };

/**
 * 按常用、已用、地区汇总、主题和 A–Z 浏览；窄屏用贴附浮卡，直接在原字段输入。
 * 搜索时侧栏显示各组命中数，输入域名时按条目数由少到多列出包含它的分类。
 */
export function GeoMenu({
  kind,
  catalog,
  values,
  blockedValues,
  disabled,
  query,
  onQuery,
  onToggle,
  onRemove,
  onPreview,
  inputRef,
  listId,
  compact = false,
  selectionRequest = 0,
}: {
  kind: GeodataKind;
  catalog: UseQueryResult<GeoCatalog>;
  values: string[];
  blockedValues: string[];
  disabled: boolean;
  query: string;
  onQuery: (query: string) => void;
  onToggle: (value: string) => void;
  onRemove: (value: string) => void;
  onPreview: (value: string) => void;
  inputRef: RefObject<HTMLInputElement | null>;
  /** 字段输入框的 aria-controls 指向这里。 */
  listId: string;
  compact?: boolean;
  selectionRequest?: number;
}) {
  const listRef = useRef<HTMLDivElement>(null);
  const data = catalog.data;
  const index = useMemo(() => (data ? geoIndex(kind, data, values) : null), [kind, data, values]);
  const parsed = index ? parseGeoQuery(index, query) : ({ mode: 'empty' } as const);
  const [group, setGroup] = useState<string | null>(selectionRequest ? 'used' : null);
  const [seenSelectionRequest, setSeenSelectionRequest] = useState(selectionRequest);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [limit, setLimit] = useState(GEO_PAGE);
  const [letter, setLetter] = useState<string | null>(null);
  const [active, setActive] = useState(0);
  // Enter 只接受完全一致的输入，或用方向键明确移到的选项；相近项必须由操作者自己挑。
  const [navigated, setNavigated] = useState(false);
  const keyboardRef = useRef(false);
  // 查询变化时回到「全部结果」第一页，在渲染中调整而不是在 effect 里补一次渲染。
  const [seenQuery, setSeenQuery] = useState(query);
  if (seenQuery !== query) {
    setSeenQuery(query);
    setGroup(null);
    setLimit(GEO_PAGE);
    setActive(0);
    setNavigated(false);
  }
  if (seenSelectionRequest !== selectionRequest) {
    setSeenSelectionRequest(selectionRequest);
    setGroup('used');
    setLimit(GEO_PAGE);
    setActive(0);
  }
  const domain = parsed.mode === 'domain' ? parsed.domain : '';
  const [settled, setSettled] = useState('');
  useEffect(() => {
    const timer = setTimeout(() => setSettled(domain), domain ? 250 : 0);
    return () => clearTimeout(timer);
  }, [domain]);
  const lookup = useQuery({
    queryKey: ['geosite-lookup', data?.source, settled],
    queryFn: ({ signal }) => lookupGeosite(settled, signal),
    enabled: !!settled && settled === domain,
    staleTime: 60000,
    retry: false,
  });

  if (!data || !index) {
    return (
      <section
        className="dialog-surface geo-menu"
        onPointerDown={event => {
          if (compact && event.target instanceof Element && event.target.closest('button')) event.preventDefault();
        }}
      >
        {!compact && <MenuSearch kind={kind} query={query} onQuery={onQuery} inputRef={inputRef} />}
        <div className="geo-menu-state">
          {catalog.isError ? '规则集无法加载' : '规则集加载中…'}
          {catalog.isError && (
            <button className="btn" onClick={() => void catalog.refetch()}>
              重试
            </button>
          )}
        </div>
      </section>
    );
  }

  const selectedSet = new Set(values.map(v => v.toLowerCase()));
  const isSelected = (value: string) => selectedSet.has(value.toLowerCase());
  const reason = (value: string) => geoBlockReason(index, value, values, blockedValues);
  const groupOf = (id: string) => index.groups.find(g => g.id === id);
  const inGroup = (code: string, id: string) =>
    id === 'az' ||
    (id === 'featured' || id === 'used' ? !!groupOf(id)?.codes.includes(code) : index.home.get(code) === id);
  const searching = parsed.mode !== 'empty';
  const current =
    group ?? (searching ? 'all' : (index.groups.find(g => g.id !== 'used' || g.codes.length) ?? index.groups[0]).id);

  const sections: Section[] = [];
  let railCounts: Map<string, number> | null = null;
  let total = 0;
  let more = 0;
  if (parsed.mode === 'list') {
    const matches = searchGeo(index, parsed);
    total = matches.length;
    railCounts = new Map(index.groups.map(g => [g.id, matches.filter(m => inGroup(m.entry.code, g.id)).length]));
    const scoped = current === 'all' ? matches : matches.filter(m => inGroup(m.entry.code, current));
    const shown = scoped.slice(0, limit);
    more = scoped.length - shown.length;
    const toRows = (items: typeof shown): Row[] =>
      items.flatMap(({ entry, attrs }): Row[] =>
        attrs
          ? attrs.map(a => ({ t: 'option', value: `${entry.code}@${a}`, count: entry.attr_counts?.[a] }))
          : [{ t: 'option', value: entry.code, attrs: entry.attrs }],
      );
    if (!matches.length) {
      const candidates = index.entries.flatMap(e => [e.code, ...e.attrs.map(a => `${e.code}@${a}`)]);
      const near = parsed.text.length > 1 ? geoSuggestions(candidates, parsed.text) : [];
      sections.push({
        key: 'none',
        title: near.length ? '相近' : `${parsed.text} 不存在`,
        rows: near.map(value => ({ t: 'option', value })),
      });
    } else if (current === 'all') {
      const buckets = new Map<string, typeof shown>();
      for (const match of shown) {
        const home = index.home.get(match.entry.code) ?? 'other';
        buckets.set(home, [...(buckets.get(home) ?? []), match]);
      }
      for (const g of index.groups)
        if (buckets.has(g.id))
          sections.push({
            key: g.id,
            title: g.label,
            note: `${fmt(railCounts.get(g.id) ?? 0)} 个`,
            rows: toRows(buckets.get(g.id)!),
          });
    } else {
      sections.push({
        key: current,
        title: groupOf(current)?.label ?? '',
        note: `${fmt(scoped.length)} 个`,
        rows: toRows(shown),
      });
    }
    const extra = parsed.attr === null ? attrHits(index, parsed.base) : 0;
    if (extra && sections.length)
      sections[sections.length - 1].after = (
        <p className="geo-menu-hint">
          另有 {fmt(extra)} 个分类带 <code>@{parsed.base}</code> 属性 ·{' '}
          <button type="button" onClick={() => onQuery(`@${parsed.base}`)}>
            只看 @{parsed.base}
          </button>
        </p>
      );
  } else if (parsed.mode === 'domain') {
    // The response domain may be masked; the query key and settled input identify this request.
    const result = settled === parsed.domain ? lookup.data : undefined;
    const hits = result?.hits ?? [];
    total = hits.length;
    railCounts = new Map(index.groups.map(g => [g.id, hits.filter(h => inGroup(h.code, g.id)).length]));
    const scoped = current === 'all' ? hits : hits.filter(h => inGroup(h.code, current));
    const suffix = getDomain(parsed.domain) ?? parsed.domain;
    if (lookup.isError && settled === parsed.domain)
      sections.push({ key: 'error', title: `反查失败：${lookup.error.message}`, rows: [] });
    else if (!result) sections.push({ key: 'pending', title: `正在反查 ${parsed.domain}…`, rows: [] });
    else
      sections.push({
        key: 'domain',
        title: hits.length ? `包含 ${parsed.domain} 的分类` : `没有分类包含 ${parsed.domain}`,
        note: hits.length ? `${fmt(scoped.length)} 个 · 由窄到宽` : undefined,
        rows: scoped.map(hit => ({
          t: 'option',
          value: hit.code,
          count: hit.count,
          via: `命中 ${hit.kind}:${hit.value}`,
          tag:
            hit === hits[0]
              ? { text: '最精确' }
              : hit.count >= GEO_WIDE_COUNT
                ? { text: '范围很宽', wide: true }
                : undefined,
        })),
        after: (
          <p className="geo-menu-hint">
            {result.unknown.length > 0 && <>另有 {fmt(result.unknown.length)} 个分类含需 Xray 确认的条目。</>}
            分类会随规则集更新而变化；只想匹配这个站点，可把匹配类型改为「域名后缀」<code>{suffix}</code>。
          </p>
        ),
      });
  } else if (current === 'used') {
    const others = (groupOf('used')?.codes ?? []).filter(code => !isSelected(code));
    if (values.length)
      sections.push({
        key: 'selected',
        title: '本条已选',
        note: `${values.length} 个`,
        rows: values.map(value => ({ t: 'option', value })),
      });
    if (others.length)
      sections.push({
        key: 'used',
        title: '其他规则已用',
        note: `${others.length} 个`,
        rows: others.map(value => ({ t: 'option', value })),
      });
  } else if (current === 'az') {
    const codes = groupOf('az')!.codes;
    const letters = [...new Set(codes.map(letterOf))];
    const chosen = letter && letters.includes(letter) ? letter : letters[0];
    const inLetter = codes.filter(code => letterOf(code) === chosen);
    more = inLetter.length - limit;
    sections.push({
      key: `az-${chosen}`,
      title: chosen.toUpperCase(),
      note: `${fmt(inLetter.length)} 个`,
      rows: inLetter.slice(0, limit).map(code => ({ t: 'option', value: code, attrs: index.byCode.get(code)?.attrs })),
    });
  } else {
    const codes = groupOf(current)?.codes ?? [];
    // 常用保持产品给定的顺序，不拆出整类。
    const whole =
      kind === 'geosite' && current !== 'featured' ? codes.filter(code => code.startsWith('category-')) : [];
    const members = codes.filter(code => !whole.includes(code));
    if (whole.length)
      sections.push({
        key: 'families',
        title: current === 'regional' ? '汇总集' : '整类',
        note: `${whole.length} 个 · 按地区折叠`,
        rows: geoFamilies(index, whole).map(family => ({ t: 'family', family })),
      });
    more = members.length - limit;
    if (members.length)
      sections.push({
        key: 'members',
        title:
          current === 'featured'
            ? '常用服务'
            : kind === 'geoip'
              ? '地区'
              : current === 'regional'
                ? '地区与列表'
                : '服务',
        note: current === 'featured' ? '条目数' : `${fmt(members.length)} 个 · 按条目数`,
        rows: members.slice(0, limit).map(code => ({ t: 'option', value: code, attrs: index.byCode.get(code)?.attrs })),
      });
  }

  // 键盘顺序与 DOM 顺序一致：每行先主选项，再行内属性（最多三个）。先收集，再按同样顺序渲染。
  const PILLS = compact ? Number.MAX_SAFE_INTEGER : 3;
  const shownAttrs = (row: Extract<Row, { t: 'option' }>) =>
    !compact || expanded === row.value ? (row.attrs ?? []).slice(0, PILLS) : [];
  const options: { value: string; disabled: boolean }[] = [];
  const collect = (value: string) =>
    options.push({ value, disabled: !isSelected(value) && (disabled || !!reason(value)) });
  for (const section of sections)
    for (const row of section.rows) {
      if (row.t === 'family') row.family.variants.forEach(variant => collect(variant.code));
      else {
        collect(row.value);
        const code = row.value.split('@')[0];
        shownAttrs(row).forEach(a => collect(`${code}@${a}`));
      }
    }
  const activeIndex = Math.min(active, Math.max(0, options.length - 1));
  let cursor = 0;
  const optionFor = () => cursor++;
  const optionId = (i: number) => `${listId}-o${i}`;

  const renderOption = (row: Extract<Row, { t: 'option' }>) => {
    const [code, attr] = row.value.split('@');
    const entry = index.byCode.get(code.replace(/^!+/, ''));
    const main = optionFor();
    const selected = isSelected(row.value);
    const why = selected ? null : reason(row.value);
    const extra = (row.attrs ?? []).slice(PILLS);
    const pills = shownAttrs(row).map(a => ({ value: `${code}@${a}`, attr: a, at: optionFor() }));
    const count = row.count ?? (attr ? entry?.attr_counts?.[attr] : entry?.count);
    const missing = !entry;
    const region = kind === 'geoip' ? regionName(code.replace(/^!+/, '')) : '';
    return (
      <div
        key={`${row.value}:${main}`}
        className={`geo-row${row.via ? ' tall' : ''}${selected ? ' selected' : ''}${main === activeIndex ? ' active' : ''}`}
        onMouseMove={() => main !== activeIndex && setActive(main)}
      >
        <button
          type="button"
          role="option"
          id={optionId(main)}
          className="geo-row-hit"
          aria-label={row.value}
          aria-selected={selected}
          disabled={options[main].disabled}
          title={why ?? (count != null ? `${row.value} · ${fmt(count)} 条` : row.value)}
          onClick={() => onToggle(row.value)}
        />
        <GeoGlyph kind={kind} code={code} />
        <span className="geo-row-name">
          <code>
            <Highlight text={code} part={parsed.mode === 'list' ? parsed.base : ''} />
            {attr && <span className="geo-row-attr">@{attr}</span>}
            {region && <small>{region}</small>}
          </code>
          {row.via && <small className="geo-row-via">{row.via}</small>}
        </span>
        {row.tag && <span className={`geo-row-tag${row.tag.wide ? ' wide' : ''}`}>{row.tag.text}</span>}
        {compact && !!row.attrs?.length && (
          <button
            type="button"
            className="geo-row-attrs-toggle"
            aria-label={`${code} 的 ${row.attrs.length} 个属性`}
            aria-expanded={expanded === row.value}
            onClick={() => setExpanded(expanded === row.value ? null : row.value)}
          >
            {row.attrs.length} 属性 <Icon of="chevronDown" size={11} />
          </button>
        )}
        {pills.length > 0 && (
          <span className="geo-row-attrs">
            {pills.map(pill => (
              <button
                key={pill.value}
                type="button"
                role="option"
                id={optionId(pill.at)}
                aria-label={pill.value}
                aria-selected={isSelected(pill.value)}
                disabled={options[pill.at].disabled}
                className={`geo-attr${isSelected(pill.value) ? ' on' : ''}${pill.at === activeIndex ? ' active' : ''}`}
                title={`${pill.value} · ${fmt(entry?.attr_counts?.[pill.attr] ?? 0)} 条`}
                onClick={() => onToggle(pill.value)}
              >
                @{pill.attr}
              </button>
            ))}
            {extra.length > 0 && (
              <span className="geo-attr-more" title={extra.map(a => `@${a}`).join(' ')}>
                +{extra.length}
              </span>
            )}
          </span>
        )}
        <span className={`geo-row-count${why || missing ? ' reason' : ''}`}>
          {why ?? (missing ? '不存在' : count != null ? fmt(count) : '')}
        </span>
        <span className="geo-row-mark" aria-hidden="true">
          {selected && <Icon of="check" size={13} />}
        </span>
      </div>
    );
  };
  const renderFamily = (family: GeoFamily) => (
    <div key={family.name} className="geo-row geo-family">
      <GeoGlyph kind={kind} code={family.name} />
      <span className="geo-row-name">
        <code>{family.name}</code>
      </span>
      <span className="geo-row-attrs">
        {family.variants.map(variant => {
          const at = optionFor();
          return (
            <button
              key={variant.code}
              type="button"
              role="option"
              id={optionId(at)}
              aria-label={variant.code}
              aria-selected={isSelected(variant.code)}
              disabled={options[at].disabled}
              className={`geo-variant${isSelected(variant.code) ? ' on' : ''}${at === activeIndex ? ' active' : ''}`}
              title={reason(variant.code) ?? variant.code}
              onMouseMove={() => at !== activeIndex && setActive(at)}
              onClick={() => onToggle(variant.code)}
            >
              {variant.label}
              <span>{fmt(index.byCode.get(variant.code)?.count ?? 0)}</span>
            </button>
          );
        })}
      </span>
    </div>
  );

  const body = sections.map(section => (
    <div key={section.key} role="group" aria-label={section.title}>
      <div className="geo-menu-sec" aria-hidden="true">
        <b>{section.title}</b>
        {section.note && <span>{section.note}</span>}
      </div>
      {section.rows.map(row => (row.t === 'family' ? renderFamily(row.family) : renderOption(row)))}
      {section.after}
    </div>
  ));

  const move = (step: number) => {
    if (!options.length) return;
    let next = activeIndex;
    for (let tries = 0; tries < options.length; tries++) {
      next = (next + step + options.length) % options.length;
      if (!options[next].disabled) break;
    }
    keyboardRef.current = true;
    setNavigated(true);
    setActive(next);
  };
  const enter = () => {
    if (parsed.mode === 'list') {
      const entry = index.byCode.get(parsed.base);
      const exact = entry && (parsed.attr === null || entry.attrs.includes(parsed.attr));
      if (exact) return onToggle(parsed.text);
    }
    const option = options[activeIndex];
    if (navigated && option && !option.disabled) onToggle(option.value);
  };

  const azLetters = current === 'az' && !searching ? [...new Set(groupOf('az')!.codes.map(letterOf))] : [];
  const file = data.source.split(/[/?#]/).filter(Boolean).at(-1) ?? `${kind}.dat`;
  const loaded = new Date(data.loaded_at * 1000).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });

  return (
    <section
      className="dialog-surface geo-menu"
      onPointerDown={event => {
        if (compact && event.target instanceof Element && event.target.closest('button')) event.preventDefault();
      }}
    >
      {compact ? (
        <AnchorSearch
          inputRef={inputRef}
          activeId={options.length ? optionId(activeIndex) : undefined}
          onMove={move}
          onEnter={enter}
        />
      ) : (
        <MenuSearch
          kind={kind}
          query={query}
          onQuery={onQuery}
          inputRef={inputRef}
          listId={listId}
          activeId={options.length ? optionId(activeIndex) : undefined}
          onKey={event => {
            if (event.nativeEvent.isComposing) return;
            if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
              event.preventDefault();
              move(event.key === 'ArrowDown' ? 1 : -1);
            }
            if (event.key === 'Enter') {
              event.preventDefault();
              enter();
            }
          }}
        />
      )}
      {!compact && values.length > 0 && (
        <div className="geo-menu-picked">
          <b>本条已选 {values.length}</b>
          {values.map(value => (
            <span
              className={`geo-chip${index.byCode.has(value.split('@')[0].replace(/^!+/, '')) ? '' : ' invalid'}`}
              key={value}
            >
              <span className="geo-chip-name">
                <GeoGlyph kind={kind} code={value.split('@')[0]} />
                <span>{value}</span>
              </span>
              <button type="button" disabled={disabled} aria-label={`移除 ${value}`} onClick={() => onRemove(value)}>
                ×
              </button>
            </span>
          ))}
        </div>
      )}
      <div className="geo-menu-body">
        <nav className="geo-menu-rail" aria-label="分组">
          {searching && (
            <button
              type="button"
              aria-pressed={current === 'all'}
              onClick={() => {
                setGroup('all');
                setLimit(GEO_PAGE);
                setActive(0);
              }}
            >
              全部结果<span>{fmt(total)}</span>
            </button>
          )}
          {index.groups.map(g => {
            const n = railCounts ? (railCounts.get(g.id) ?? 0) : g.codes.length;
            return (
              <RailItem
                key={g.id}
                heading={g.heading}
                label={g.label}
                count={n}
                pressed={current === g.id}
                disabled={searching && n === 0}
                onClick={() => {
                  setGroup(g.id);
                  setLimit(GEO_PAGE);
                  setActive(0);
                  listRef.current?.scrollTo?.({ top: 0 });
                }}
              />
            );
          })}
          {compact && (
            <button
              type="button"
              className="geo-menu-preview"
              onClick={() => onPreview(parsed.mode === 'list' && index.byCode.has(parsed.base) ? parsed.base : '')}
            >
              <Icon of="search" size={13} /> 查看条目…
            </button>
          )}
        </nav>
        <div
          ref={listRef}
          id={listId}
          role="listbox"
          aria-label={kind}
          aria-multiselectable="true"
          className="geo-menu-list"
        >
          {azLetters.length > 0 && (
            <div className="geo-menu-letters" role="presentation">
              {azLetters.map(l => (
                <button
                  key={l}
                  type="button"
                  aria-label={`首字母 ${l.toUpperCase()}`}
                  aria-pressed={(letter ?? azLetters[0]) === l}
                  onClick={() => {
                    setLetter(l);
                    setLimit(GEO_PAGE);
                    setActive(0);
                  }}
                >
                  {l.toUpperCase()}
                </button>
              ))}
            </div>
          )}
          {body}
          {more > 0 && (
            <button type="button" className="geo-menu-more" onClick={() => setLimit(limit + GEO_PAGE)}>
              再显示 {fmt(Math.min(GEO_PAGE, more))} 个 · 还有 {fmt(more)} 个
            </button>
          )}
        </div>
      </div>
      {!compact && (
        <footer className="geo-menu-foot">
          <span title={data.source}>
            {file} · {fmt(data.entries.length)} 类 · {loaded} 加载
          </span>
          <button
            type="button"
            className="geo-menu-browse"
            onClick={() => onPreview(parsed.mode === 'list' && index.byCode.has(parsed.base) ? parsed.base : '')}
          >
            <Icon of="search" size={14} />
            查看条目…
          </button>
        </footer>
      )}
      <ScrollActive id={options.length ? optionId(activeIndex) : ''} keyboardRef={keyboardRef} />
    </section>
  );
}

function RailItem({
  heading,
  label,
  count,
  pressed,
  disabled,
  onClick,
}: {
  heading?: string;
  label: string;
  count: number;
  pressed: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <>
      {heading && <small>{heading}</small>}
      <button type="button" aria-pressed={pressed} disabled={disabled} onClick={onClick}>
        {label}
        <span>{fmt(count)}</span>
      </button>
    </>
  );
}

function AnchorSearch({
  inputRef,
  activeId,
  onMove,
  onEnter,
}: {
  inputRef: RefObject<HTMLInputElement | null>;
  activeId?: string;
  onMove: (step: number) => void;
  onEnter: () => void;
}) {
  useEffect(() => {
    const input = inputRef.current;
    if (!input) return;
    if (activeId) input.setAttribute('aria-activedescendant', activeId);
    else input.removeAttribute('aria-activedescendant');
    const handleKey = (event: globalThis.KeyboardEvent) => {
      if (event.isComposing) return;
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault();
        onMove(event.key === 'ArrowDown' ? 1 : -1);
      } else if (event.key === 'Enter') {
        event.preventDefault();
        onEnter();
      }
    };
    input.addEventListener('keydown', handleKey);
    return () => {
      input.removeAttribute('aria-activedescendant');
      input.removeEventListener('keydown', handleKey);
    };
  }, [inputRef, activeId, onMove, onEnter]);
  return null;
}

function MenuSearch({
  kind,
  query,
  onQuery,
  inputRef,
  listId,
  activeId,
  onKey,
}: {
  kind: GeodataKind;
  query: string;
  onQuery: (query: string) => void;
  inputRef: RefObject<HTMLInputElement | null>;
  listId?: string;
  activeId?: string;
  onKey?: (event: KeyboardEvent<HTMLInputElement>) => void;
}) {
  return (
    <div className="geo-menu-search">
      <Icon of="search" />
      <input
        className="f"
        aria-label={`搜索 ${kind}`}
        role="combobox"
        aria-expanded="true"
        aria-controls={listId}
        aria-activedescendant={activeId}
        aria-autocomplete="list"
        ref={inputRef}
        maxLength={253}
        autoFocus
        spellCheck={false}
        placeholder={kind === 'geosite' ? '名称、名称@属性，或域名' : '代码、地区名或 代码@ipv4'}
        value={query}
        onChange={event => onQuery(event.target.value)}
        onKeyDown={onKey}
      />
      <DialogClose className="geo-close" aria-label="关闭">
        ×
      </DialogClose>
    </div>
  );
}

function Highlight({ text, part }: { text: string; part: string }) {
  const at = part ? text.indexOf(part) : -1;
  if (at < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, at)}
      <mark>{text.slice(at, at + part.length)}</mark>
      {text.slice(at + part.length)}
    </>
  );
}

/** 只在键盘移动时把当前项滚入视野；鼠标悬停已在视野内，不应引起跳动。 */
function ScrollActive({ id, keyboardRef }: { id: string; keyboardRef: RefObject<boolean> }) {
  useEffect(() => {
    if (!keyboardRef.current || !id) return;
    keyboardRef.current = false;
    document.getElementById(id)?.closest('.geo-row')?.scrollIntoView?.({ block: 'nearest' });
  }, [id, keyboardRef]);
  return null;
}
