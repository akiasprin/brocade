import { useEffect, useId, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  fetchGeoCatalog,
  fetchGeoPage,
  fetchGeoRelations,
  geoValueIssue,
  geoSuggestions,
  type GeodataKind,
} from '../api/geodata';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { Icon } from '../ui/icons';
import { RegionFlag } from '../ui/region-flag';
import { SiteIcon } from '../ui/site-icon';
import './geodata.css';

export function useGeoCatalog(kind: GeodataKind, enabled = true, source?: string) {
  return useQuery({
    queryKey: ['geodata', kind, source],
    queryFn: ({ signal }) => fetchGeoCatalog(kind, signal, source),
    enabled,
    staleTime: 60000,
    retry: false,
  });
}
export function useGeoRelations(kind: GeodataKind, values: string[], enabled = true) {
  const unique = [...new Set(values)].slice(0, 128).sort();
  return useQuery({
    queryKey: ['geodata-relations', kind, unique],
    queryFn: ({ signal }) => fetchGeoRelations(kind, unique, signal),
    enabled: enabled && unique.length > 1,
    staleTime: 60000,
    retry: false,
  });
}
function GeoGlyph({ kind, code }: { kind: GeodataKind; code: string }) {
  const base = kind === 'geoip' ? code.replace(/^!+/, '') : code;
  return kind === 'geoip' && /^[a-z]{2}$/i.test(base) ? <RegionFlag code={base} /> : <SiteIcon siteId={base} />;
}

export function GeoSelector({
  kind,
  values,
  onChange,
  label,
  issue,
  issueValue,
  blockedValues = [],
  disabled = false,
  unavailable = false,
}: {
  kind: GeodataKind;
  values: string[];
  onChange: (values: string[]) => void;
  label: string;
  issue?: string | null;
  issueValue?: string;
  blockedValues?: string[];
  disabled?: boolean;
  unavailable?: boolean;
}) {
  const catalog = useGeoCatalog(kind, !unavailable);
  const anchor = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const menuInput = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [preview, setPreview] = useState<string | null>(null);
  const [shown, setShown] = useState(1);
  const id = useId();
  useEffect(() => {
    const element = anchor.current;
    if (!element) return;
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(([entry]) => setShown(entry.contentRect.width > 420 ? 2 : 1));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const invalid = values.map(value => geoValueIssue(catalog.data, value)).find(Boolean);
  const error = issue || invalid;
  const unknown = !catalog.data && values.length > 0;
  const candidates = (catalog.data?.entries ?? []).flatMap(entry => [
    entry.code,
    ...entry.attrs.map(a => `${entry.code}@${a}`),
  ]);
  const regions = new Intl.DisplayNames(['zh-CN'], { type: 'region' });
  const matches = candidates
    .filter(
      value =>
        value.toLowerCase().includes(query.toLowerCase()) ||
        (kind === 'geoip' &&
          /^[a-z]{2}$/i.test(value.split('@')[0]) &&
          (regions.of(value.split('@')[0].toUpperCase()) ?? '').includes(query)),
    )
    .slice(0, 80);
  const suggestions = matches.length === 0 && query.length > 1 ? geoSuggestions(candidates, query) : [];
  const options = matches.length ? matches : suggestions;
  const invalidValue = values.find(v => geoValueIssue(catalog.data, v));
  const correction = invalidValue ? geoSuggestions(candidates, invalidValue)[0] : undefined;
  const optionRelations = useGeoRelations(
    kind,
    [...blockedValues, ...values, ...(query ? options.slice(0, 20) : [])],
    !disabled && !!catalog.data,
  );
  const blocked = (value: string) => {
    const prior = blockedValues.find(
      v => v.toLowerCase() === value.toLowerCase() || value.toLowerCase().startsWith(`${v.toLowerCase()}@`),
    );
    if (prior) return '前面规则已有';
    const cover = optionRelations.data?.covers.find(
      p => p.narrower === value && [...blockedValues, ...values].includes(p.wider),
    );
    if (cover) return values.includes(cover.wider) ? '本条已包含' : '前面规则已包含';
    return null;
  };
  const add = (value: string) => {
    if (disabled || !catalog.data || blocked(value) || values.some(v => v.toLowerCase() === value.toLowerCase()))
      return;
    onChange([...values, value]);
    setQuery('');
    menuInput.current?.focus();
  };
  return (
    <div className="geo-field">
      <div ref={anchor} className={`f geo-select${error ? ' invalid' : ''}`} aria-disabled={disabled}>
        {values.slice(0, shown).map(value => (
          <span className={`geo-chip${geoValueIssue(catalog.data, value) ? ' invalid' : ''}`} key={value} title={value}>
            <button
              type="button"
              className="geo-chip-name"
              onClick={() => setPreview(value)}
              aria-label={`预览 ${value}`}
            >
              <GeoGlyph kind={kind} code={value.split('@')[0]} />
              <span>{value}</span>
            </button>
            <button
              type="button"
              disabled={disabled}
              aria-label={`移除 ${value}`}
              onClick={() => onChange(values.filter(v => v !== value))}
            >
              ×
            </button>
          </span>
        ))}
        {values.length > shown && (
          <button type="button" className="geo-more-count" onClick={() => setOpen(true)}>
            +{values.length - shown}
          </button>
        )}
        <input
          ref={input}
          aria-label={label}
          maxLength={128}
          role="combobox"
          aria-expanded={open}
          aria-controls={open ? id : undefined}
          aria-autocomplete="list"
          value={query}
          disabled={disabled}
          placeholder={values.length ? '' : kind === 'geosite' ? '如 netflix' : '如 cn'}
          onClick={() => setOpen(true)}
          onChange={e => {
            setQuery(e.target.value);
            setOpen(true);
          }}
          onKeyDown={e => {
            if (e.key === 'Enter') {
              e.preventDefault();
              if (!open) {
                setOpen(true);
                return;
              }
              const exact = options.find(v => v.toLowerCase() === query.toLowerCase());
              if (exact) add(exact);
            }
            if (e.key === 'Backspace' && !query && values.length) onChange(values.slice(0, -1));
            if (e.key === 'Escape') setOpen(false);
            if (e.key === 'ArrowDown') {
              e.preventDefault();
              setOpen(true);
            }
          }}
        />
        <button
          type="button"
          disabled={disabled}
          className="geo-chevron"
          aria-label={`展开 ${kind}`}
          onClick={() => setOpen(v => !v)}
        >
          <Icon of="chevronDown" size={12} />
        </button>
      </div>
      {(error || unknown) && (
        <div className={`geo-message${error ? ' error' : ''}`} title={error || '规则集未加载，分类未确认'}>
          <span>{error || '未确认'}</span>
          {error && (
            <button
              type="button"
              disabled={disabled}
              onClick={() => {
                const bad = issueValue || invalidValue;
                if (bad)
                  onChange(correction ? values.map(v => (v === bad ? correction : v)) : values.filter(v => v !== bad));
              }}
            >
              {correction ? `改为 ${correction}` : '移除'}
            </button>
          )}
        </div>
      )}
      {open && (
        <DialogLayer
          mode="popover"
          anchor={anchor}
          align="start"
          className="geo-menu-layer"
          label={`${kind} 选项`}
          onClose={() => setOpen(false)}
        >
          <section className="dialog-surface geo-menu">
            <div className="geo-menu-search">
              <Icon of="search" />
              <input
                className="f"
                aria-label={`搜索 ${kind}`}
                ref={menuInput}
                maxLength={128}
                autoFocus
                value={query}
                onChange={e => setQuery(e.target.value)}
                onKeyDown={e => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    const exact = options.find(v => v.toLowerCase() === query.toLowerCase());
                    if (exact) add(exact);
                  }
                }}
              />
              <DialogClose className="geo-close" aria-label="关闭">
                ×
              </DialogClose>
            </div>
            {!catalog.data ? (
              <div className="geo-menu-state">
                {catalog.isFetching ? '规则集加载中…' : '规则集无法加载'}
                {catalog.isError && (
                  <button className="btn" onClick={() => void catalog.refetch()}>
                    重试
                  </button>
                )}
              </div>
            ) : (
              <>
                <div id={id} role="listbox" aria-label={kind} className="geo-options">
                  {values.length > 0 && (
                    <>
                      <small>已选 · {values.length}</small>
                      {values.map(value => (
                        <button
                          type="button"
                          role="option"
                          aria-selected="true"
                          key={`selected:${value}`}
                          onClick={() => onChange(values.filter(v => v !== value))}
                        >
                          <GeoGlyph kind={kind} code={value.split('@')[0]} />
                          <code>{value}</code>
                          <span>{geoValueIssue(catalog.data, value) || '✓'}</span>
                        </button>
                      ))}
                    </>
                  )}
                  <small>{query ? (suggestions.length ? '相近' : '搜索结果') : '可选'}</small>
                  {options.map(value => {
                    const selected = values.some(v => v.toLowerCase() === value.toLowerCase());
                    return (
                      <button
                        type="button"
                        role="option"
                        aria-selected={selected}
                        disabled={selected || disabled || !!blocked(value)}
                        key={value}
                        onClick={() => add(value)}
                      >
                        <GeoGlyph kind={kind} code={value.split('@')[0]} />
                        <code>{value}</code>
                        <span>{selected ? '已选' : blocked(value) || ''}</span>
                      </button>
                    );
                  })}
                  {!options.length && <div className="geo-menu-state">{query} 不存在</div>}
                </div>
                <button
                  type="button"
                  className="geo-browse"
                  onClick={() => {
                    setPreview(query.split('@')[0]);
                    setOpen(false);
                  }}
                >
                  <Icon of="search" />
                  <span className="geo-browse-label">更多…</span>
                  <span className="geo-browse-summary">
                    浏览 {kind}.dat · {catalog.data.entries.length.toLocaleString()} 类
                  </span>
                  <Icon of="chevronDown" size={12} />
                </button>
              </>
            )}
          </section>
        </DialogLayer>
      )}
      {preview !== null && (
        <GeodataBrowser
          kind={kind}
          initial={preview}
          selected={values}
          onAdd={disabled ? undefined : add}
          onClose={() => setPreview(null)}
        />
      )}
    </div>
  );
}

export function GeodataBrowser({
  kind,
  source,
  initial = '',
  selected = [],
  onAdd,
  onClose,
}: {
  kind: GeodataKind;
  source?: string;
  initial?: string;
  selected?: string[];
  onAdd?: (value: string) => void;
  onClose: () => void;
}) {
  const catalog = useGeoCatalog(kind, true, source);
  const [search, setSearch] = useState('');
  const inverse = kind === 'geoip' && initial.startsWith('!');
  const [code, setCode] = useState(initial.split('@')[0].replace(/^!+/, ''));
  const [attr, setAttr] = useState(initial.split('@').slice(1).join('@'));
  const [filter, setFilter] = useState('');
  const [offset, setOffset] = useState(0);
  const [target, setTarget] = useState('');
  const [checked, setChecked] = useState('');
  const [mobileDetail, setMobileDetail] = useState(!!initial);
  const [onlySelected, setOnlySelected] = useState(false);
  const references = catalog.data?.references ?? [];
  const referencedCodes = new Set([...selected, ...references.map(r => r.selector)].map(v => v.split('@')[0]));
  const missing = [...referencedCodes]
    .filter(code => !catalog.data?.entries.some(e => e.code === code))
    .map(code => ({ code, count: 0, attrs: [] }));
  const categories = [...missing, ...(catalog.data?.entries ?? [])]
    .filter(e => e.code.includes(search.toLowerCase()) && (!onlySelected || referencedCodes.has(e.code)))
    .sort(
      (a, b) =>
        Number(referencedCodes.has(b.code)) - Number(referencedCodes.has(a.code)) || a.code.localeCompare(b.code),
    );
  const category = catalog.data?.entries.find(e => e.code === code);
  const page = useQuery({
    queryKey: ['geodata-page', kind, catalog.data?.source, code, attr, filter, offset, checked, inverse],
    queryFn: ({ signal }) =>
      fetchGeoPage(
        kind,
        `${inverse ? '!' : ''}${code}`,
        { attr, q: filter, offset: String(offset), limit: '100', target: checked, ...(source ? { source } : {}) },
        signal,
      ),
    enabled: !!category,
    retry: false,
  });
  const choose = (next: string) => {
    setCode(next);
    setAttr('');
    setOffset(0);
    setFilter('');
    setChecked('');
    setMobileDetail(true);
  };
  const value = `${inverse ? '!' : ''}${code}${attr ? `@${attr}` : ''}`;
  return (
    <DialogLayer label="规则集预览" onClose={onClose} className="geo-browser-layer">
      <section className={`dialog-surface geo-browser${mobileDetail ? ' detail-open' : ''}`}>
        <header>
          <div>
            <h2>
              <Icon of="dns" />
              规则集预览 <code>{kind}.dat</code>
            </h2>
            <p title={catalog.data?.source}>
              {catalog.data ? '● 已加载' : '规则集未加载'} · {catalog.data?.entries.length.toLocaleString() ?? '—'} 类{' '}
              <span>{catalog.data?.source}</span>
            </p>
          </div>
          <DialogClose className="geo-close" aria-label="关闭">
            ×
          </DialogClose>
        </header>
        <div className="geo-browser-body">
          <aside>
            <div className="geo-menu-search">
              <Icon of="search" />
              <input
                className="f"
                aria-label="搜索分类"
                placeholder="搜索分类"
                value={search}
                onChange={e => setSearch(e.target.value)}
              />
            </div>
            <label className="geo-references">
              <input type="checkbox" checked={onlySelected} onChange={e => setOnlySelected(e.target.checked)} />
              仅看已引用
            </label>
            {catalog.isError && (
              <div className="geo-menu-state">
                规则集无法加载
                <button className="btn" onClick={() => void catalog.refetch()}>
                  重试
                </button>
              </div>
            )}
            {catalog.isPending && <p>规则集加载中…</p>}
            <nav aria-label="分类">
              {categories.map(entry => (
                <button
                  type="button"
                  key={entry.code}
                  className={`${code === entry.code ? 'active' : ''}${entry.count === 0 ? ' missing' : ''}`}
                  onClick={() => choose(entry.code)}
                >
                  <GeoGlyph kind={kind} code={entry.code} />
                  <code>{entry.code}</code>
                  <span>{entry.count ? entry.count.toLocaleString() : '不存在'}</span>
                </button>
              ))}
            </nav>
          </aside>
          <main>
            <button type="button" className="geo-back btn" onClick={() => setMobileDetail(false)}>
              ‹ 全部分类
            </button>
            {!category ? (
              <p>{code ? `${code} 不存在` : '选择一个分类'}</p>
            ) : (
              <>
                <h3>
                  <GeoGlyph kind={kind} code={code} />
                  {code}
                  <span>{category.count.toLocaleString()} 条</span>
                </h3>
                <div className="geo-attrs">
                  <button
                    className={!attr ? 'active' : ''}
                    onClick={() => {
                      setAttr('');
                      setOffset(0);
                    }}
                  >
                    全部
                  </button>
                  {category.attrs.map(a => (
                    <button
                      key={a}
                      className={attr === a ? 'active' : ''}
                      onClick={() => {
                        setAttr(a);
                        setOffset(0);
                      }}
                    >
                      @{a}
                    </button>
                  ))}
                </div>
                {page.data && (
                  <>
                    <div className="geo-type-bar" aria-label="条目构成">
                      {Object.entries(page.data.counts).map(([key, count], i) => (
                        <i key={key} style={{ flex: count, background: `var(--hue-${(i % 4) + 1})` }} />
                      ))}
                    </div>
                    <div className="geo-composition">
                      {Object.entries(page.data.counts).map(([key, count]) => (
                        <span key={key}>
                          {(
                            { domain: '后缀', full: '完整', keyword: '关键词', regexp: '正则' } as Record<
                              string,
                              string
                            >
                          )[key] ?? key}{' '}
                          <b>{count.toLocaleString()}</b>
                        </span>
                      ))}
                    </div>
                  </>
                )}
                <form
                  className="geo-check"
                  onSubmit={e => {
                    e.preventDefault();
                    setChecked(target.trim());
                  }}
                >
                  <Icon of="search" />
                  <input
                    className="f mono"
                    aria-label="检查命中"
                    placeholder={kind === 'geosite' ? '输入域名检查命中' : '输入 IP 检查命中'}
                    value={target}
                    onChange={e => setTarget(e.target.value)}
                  />
                  <button className="btn" type="submit">
                    检查
                  </button>
                </form>
                {checked && page.data && (
                  <p className="geo-hit">
                    {page.data?.hit === null ? '待确认' : page.data?.hit ? '✓ 命中' : '未命中'} · {checked}
                  </p>
                )}
                <input
                  className="f mono geo-entry-filter"
                  aria-label="筛选条目"
                  placeholder="筛选条目"
                  value={filter}
                  onChange={e => {
                    setFilter(e.target.value);
                    setOffset(0);
                  }}
                />
                {page.isPending && <p>加载中…</p>}
                {page.error && <p className="geo-message error">{page.error.message}</p>}
                <div className="geo-entries">
                  {page.data?.entries.map((entry, i) => (
                    <div key={`${offset}:${i}`}>
                      <small>{entry.kind}</small>
                      <code>{entry.value}</code>
                      <span>{entry.attrs.map(a => `@${a}`).join(' ')}</span>
                    </div>
                  ))}
                </div>
                <div className="geo-pagination">
                  <span>{page.data?.total.toLocaleString() ?? '—'} 条</span>
                  <button className="btn" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - 100))}>
                    上一页
                  </button>
                  <button
                    className="btn"
                    disabled={page.data?.next_offset == null}
                    onClick={() => setOffset(page.data!.next_offset!)}
                  >
                    下一页
                  </button>
                </div>
              </>
            )}
            {code && references.some(r => r.selector.split('@')[0] === code) && (
              <section className="geo-reference-list">
                <h4>引用位置</h4>
                {references
                  .filter(r => r.selector.split('@')[0] === code)
                  .map((r, i) => (
                    <p key={i}>
                      <code>{r.location}</code> · 规则 {r.rule + 1} · {r.selector}
                    </p>
                  ))}
              </section>
            )}
          </main>
        </div>
        <footer>
          <span className="mono">{value}</span>
          {onAdd && (
            <button
              className="btn primary"
              disabled={!category || selected.includes(value)}
              onClick={() => {
                onAdd(value);
                onClose();
              }}
            >
              {selected.includes(value) ? '已选' : '加入规则'}
            </button>
          )}
        </footer>
      </section>
    </DialogLayer>
  );
}
