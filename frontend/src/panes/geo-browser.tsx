import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useInfiniteQuery, useQuery, type UseQueryResult } from '@tanstack/react-query';
import {
  fetchGeoPage,
  lookupGeosite,
  type GeoCatalog,
  type GeoCategory,
  type GeoEntry,
  type GeodataKind,
} from '../api/geodata';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { Icon } from '../ui/icons';
import { GeoGlyph } from './geo-menu';
import { geoBlockReason, geoIndex, looksLikeDomain, regionName, searchGeo, type GeoIndex } from './geo-menu-model';

const fmt = (n: number) => n.toLocaleString('en-US');

/** 条目类型的展示顺序与名称；与 page 接口的排序一致，未知类型排在最后。 */
const KIND_LABEL: Record<string, string> = {
  domain: '后缀',
  full: '完整',
  regexp: '正则',
  keyword: '关键词',
  ipv4: 'IPv4',
  ipv6: 'IPv6',
};
const KIND_ORDER = ['domain', 'full', 'regexp', 'keyword', 'ipv4', 'ipv6'];
const kindRank = (kind: string) => (KIND_ORDER.includes(kind) ? KIND_ORDER.indexOf(kind) : KIND_ORDER.length);
const kindLabel = (kind: string) => KIND_LABEL[kind] ?? kind;

/** 每次取 200 条（接口上限），最多展开到 2,000 条；再多就该用筛选缩小范围，而不是继续撑大列表。 */
const PAGE_SIZE = 200;
const LOADED_LIMIT = 2_000;

const IP_LITERAL = /^(\d{1,3}(\.\d{1,3}){3}|[0-9a-f]*:[0-9a-f:.]*)$/i;
const NONE: string[] = [];

/** 中文与 ASCII 相邻处补一个空格：「加入 D1」「D1 已选」，「加入本条」「已在规则 3」不变。 */
const spaced = (...parts: string[]) =>
  parts.reduce((text, part) =>
    text && part && /[!-~]$/.test(text) !== /^[!-~]/.test(part) ? `${text} ${part}` : text + part,
  );

type Tab = 'entries' | 'narrower' | 'refs';

/**
 * 规则集预览：左栏按「本条已选 / 其他规则已用 / 全部」列出分类，右栏是一个分类的条目。
 * 头部一行放名称、条目数和加入／移除；属性带条目数；一个输入框既筛选条目，像域名（或 IP）时也检查命中，
 * 并指出是哪条规则命中。条目按类型分节，每节按值的长度决定列宽，滚动到底继续加载。
 */
export function GeoBrowser({
  kind,
  catalog,
  source,
  initial = '',
  selected = NONE,
  blockedValues = NONE,
  scope = '本条',
  onToggle,
  onReplace,
  onClose,
}: {
  kind: GeodataKind;
  catalog: UseQueryResult<GeoCatalog>;
  /** 设置页预览尚未保存的来源；此时没有反查替代分类。 */
  source?: string;
  initial?: string;
  /** 打开预览的那一行已选的值。 */
  selected?: string[];
  blockedValues?: string[];
  /** 那一行的简称，如「D1」「规则 3」，用于「加入 D1」。 */
  scope?: string;
  onToggle?: (value: string) => void;
  onReplace?: (from: string, to: string) => void;
  onClose: () => void;
}) {
  const data = catalog.data;
  const index = useMemo(() => (data ? geoIndex(kind, data, selected) : null), [kind, data, selected]);
  const [inverse, setInverse] = useState(kind === 'geoip' && initial.startsWith('!'));
  const [code, setCode] = useState(initial.split('@')[0].replace(/^!+/, '').toLowerCase());
  const [attr, setAttr] = useState(initial.split('@').slice(1).join('@').toLowerCase());
  const [entryKind, setEntryKind] = useState('');
  const [text, setText] = useState('');
  const [tab, setTab] = useState<Tab>('entries');
  const [navQuery, setNavQuery] = useState('');
  const [detail, setDetail] = useState(!!initial);
  const listRef = useRef<HTMLDivElement>(null);

  const category = index?.byCode.get(code);
  const narrower = useMemo(() => {
    const map = new Map<string, string[]>();
    for (const entry of data?.entries ?? [])
      for (const wider of entry.within ?? []) map.set(wider, [...(map.get(wider) ?? []), entry.code]);
    return map;
  }, [data]);
  const references = (data?.references ?? []).filter(r => r.selector.split('@')[0].replace(/^!+/, '') === code);

  const go = (next: string, keepText = false) => {
    setCode(next);
    setAttr('');
    setInverse(false);
    setEntryKind('');
    setTab('entries');
    setDetail(true);
    if (!keepText) setText('');
    listRef.current?.scrollTo?.({ top: 0 });
  };

  const name = `${inverse ? '!' : ''}${code}`;
  const value = `${name}${attr ? `@${attr}` : ''}`;
  const count = category ? (attr ? (category.attr_counts?.[attr] ?? 0) : category.count) : 0;
  const file = (source ?? data?.source ?? '').split(/[/?#]/).filter(Boolean).at(-1) ?? `${kind}.dat`;
  const loadedAt = data
    ? new Date(data.loaded_at * 1000).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
    : '';

  return (
    <DialogLayer label="规则集预览" onClose={onClose} className="geo-browser-layer">
      <section className={`dialog-surface geo-browser${detail ? ' detail' : ''}`}>
        <header className="geo-browser-bar">
          <Icon of="dns" size={15} />
          <b>规则集</b>
          <code>{file || `${kind}.dat`}</code>
          <span className="geo-browser-meta header-stat" title={source ?? data?.source}>
            {data ? (
              <>
                <i aria-hidden="true" />
                {fmt(data.entries.length)} 类 · {fmt(data.entries.reduce((n, e) => n + e.count, 0))} 条 · {loadedAt}{' '}
                加载
              </>
            ) : catalog.isError ? (
              '规则集未加载'
            ) : (
              '加载中…'
            )}
          </span>
          <DialogClose className="geo-close" aria-label="关闭">
            <Icon of="close" size={16} />
          </DialogClose>
        </header>
        {!data || !index ? (
          <div className="geo-browser-body">
            <div className="geo-menu-state">
              {catalog.isError ? '规则集无法加载' : '规则集加载中…'}
              {catalog.isError && (
                <button className="btn" onClick={() => void catalog.refetch()}>
                  重试
                </button>
              )}
            </div>
          </div>
        ) : (
          <div className="geo-browser-body">
            <BrowserNav
              index={index}
              selected={selected}
              references={data.references ?? []}
              scope={scope}
              query={navQuery}
              current={code}
              onQuery={setNavQuery}
              onPick={go}
            />
            <section className="geo-browser-main" aria-label={name || '分类'}>
              {!category ? (
                <div className="geo-browser-head">
                  <BackButton onClick={() => setDetail(false)} />
                  <p className="geo-browser-empty">{code ? `${code} 不在此规则集中` : '从左侧选择一个分类'}</p>
                </div>
              ) : (
                <CategoryDetail
                  kind={kind}
                  source={source}
                  catalogSource={data.source}
                  index={index}
                  category={category}
                  name={name}
                  value={value}
                  attr={attr}
                  count={count}
                  entryKind={entryKind}
                  text={text}
                  tab={tab}
                  narrower={narrower.get(code) ?? []}
                  references={references}
                  selected={selected}
                  blockedValues={blockedValues}
                  scope={scope}
                  listRef={listRef}
                  onAttr={next => {
                    setAttr(next);
                    setEntryKind('');
                  }}
                  onEntryKind={setEntryKind}
                  onText={setText}
                  onTab={setTab}
                  onGo={go}
                  onBack={() => setDetail(false)}
                  onToggle={onToggle}
                  onReplace={onReplace}
                />
              )}
            </section>
          </div>
        )}
      </section>
    </DialogLayer>
  );
}

function BackButton({ onClick }: { onClick: () => void }) {
  return (
    <button type="button" className="geo-browser-back" aria-label="全部分类" onClick={onClick}>
      <Icon of="chevronLeft" size={16} />
    </button>
  );
}

/** 左栏：来源行已选、其他规则已用、全部 A–Z；输入时按与选择菜单相同的相关度排序。 */
const BrowserNav = memo(function BrowserNav({
  index,
  selected,
  references,
  scope,
  query,
  current,
  onQuery,
  onPick,
}: {
  index: GeoIndex;
  selected: string[];
  references: NonNullable<GeoCatalog['references']>;
  scope: string;
  query: string;
  current: string;
  onQuery: (query: string) => void;
  onPick: (code: string) => void;
}) {
  const base = (value: string) => value.split('@')[0].replace(/^!+/, '').toLowerCase();
  const picked = [...new Set(selected.map(base))];
  const used = [...new Set(references.map(r => base(r.selector)))].filter(c => !picked.includes(c));
  const text = query.trim().toLowerCase();
  const row = (code: string) => {
    const entry = index.byCode.get(code);
    return (
      <button
        key={code}
        type="button"
        className={`geo-browser-nav-row${entry ? '' : ' missing'}`}
        aria-label={`${code} ${entry ? `${fmt(entry.count)} 条` : '不存在'}`}
        aria-current={code === current}
        onClick={() => onPick(code)}
      >
        <GeoGlyph kind={index.kind} code={code} />
        <code>
          <Highlight text={code} part={text} />
        </code>
        {picked.includes(code) && <i className="geo-browser-dot" title={spaced('已在', scope)} />}
        <small>{entry ? fmt(entry.count) : '不存在'}</small>
      </button>
    );
  };
  const head = (title: string, n: number) => (
    <div className="geo-browser-nav-head" aria-hidden="true">
      <span>{title}</span>
      <span>{fmt(n)}</span>
    </div>
  );
  let body: ReactNode;
  if (text) {
    const hits = searchGeo(index, { mode: 'list', text, base: text.replace(/^!+/, ''), attr: null });
    body = hits.length ? (
      <>
        {head('搜索结果', hits.length)}
        {hits.map(hit => row(hit.entry.code))}
      </>
    ) : (
      <p className="geo-browser-empty">没有名称包含 “{text}” 的分类</p>
    );
  } else {
    const all =
      index.groups.find(g => g.id === 'az')?.codes ?? index.entries.map(e => e.code).sort((a, b) => a.localeCompare(b));
    body = (
      <>
        {picked.length > 0 && (
          <>
            {head(spaced(scope, '已选'), picked.length)}
            {picked.map(row)}
          </>
        )}
        {used.length > 0 && (
          <>
            {head(picked.length ? '其他规则已用' : '已引用', used.length)}
            {used.map(row)}
          </>
        )}
        {head('全部 A–Z', all.length)}
        {all.map(row)}
      </>
    );
  }
  return (
    <nav className="geo-browser-nav" aria-label="分类">
      <div className="geo-browser-nav-top">
        <FindInput value={query} onChange={onQuery} label="搜索分类" placeholder="搜索分类" />
      </div>
      <div className="geo-browser-nav-list">{body}</div>
    </nav>
  );
});

function FindInput({
  value,
  onChange,
  label,
  placeholder,
}: {
  value: string;
  onChange: (value: string) => void;
  label: string;
  placeholder: string;
}) {
  return (
    <label className="geo-browser-find">
      <Icon of="search" size={14} />
      <input
        className="f"
        aria-label={label}
        placeholder={placeholder}
        value={value}
        maxLength={253}
        spellCheck={false}
        autoComplete="off"
        onChange={event => onChange(event.target.value)}
      />
      {value && (
        <button type="button" aria-label={`清除${label}`} onClick={() => onChange('')}>
          ×
        </button>
      )}
    </label>
  );
}

function useSettled(value: string, delay: number) {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), value ? delay : 0);
    return () => clearTimeout(timer);
  }, [value, delay]);
  return settled;
}

function CategoryDetail({
  kind,
  source,
  catalogSource,
  index,
  category,
  name,
  value,
  attr,
  count,
  entryKind,
  text,
  tab,
  narrower,
  references,
  selected,
  blockedValues,
  scope,
  listRef,
  onAttr,
  onEntryKind,
  onText,
  onTab,
  onGo,
  onBack,
  onToggle,
  onReplace,
}: {
  kind: GeodataKind;
  source?: string;
  catalogSource: string;
  index: GeoIndex;
  category: GeoCategory;
  name: string;
  value: string;
  attr: string;
  count: number;
  entryKind: string;
  text: string;
  tab: Tab;
  narrower: string[];
  references: NonNullable<GeoCatalog['references']>;
  selected: string[];
  blockedValues: string[];
  scope: string;
  listRef: React.RefObject<HTMLDivElement | null>;
  onAttr: (attr: string) => void;
  onEntryKind: (kind: string) => void;
  onText: (text: string) => void;
  onTab: (tab: Tab) => void;
  onGo: (code: string, keepText?: boolean) => void;
  onBack: () => void;
  onToggle?: (value: string) => void;
  onReplace?: (from: string, to: string) => void;
}) {
  const settled = useSettled(text.trim().toLowerCase(), 250);
  const target =
    kind === 'geosite'
      ? looksLikeDomain(settled)
        ? settled.replace(/\.$/, '')
        : ''
      : IP_LITERAL.test(settled)
        ? settled
        : '';
  const pages = useInfiniteQuery({
    queryKey: ['geodata-page', kind, source ?? catalogSource, name, attr, entryKind, settled, target],
    queryFn: ({ pageParam, signal }) =>
      fetchGeoPage(
        kind,
        name,
        {
          attr,
          type: entryKind,
          q: settled,
          offset: String(pageParam),
          limit: String(PAGE_SIZE),
          target,
          ...(source ? { source } : {}),
        },
        signal,
      ),
    initialPageParam: 0,
    getNextPageParam: last => last.next_offset ?? undefined,
    enabled: tab === 'entries',
    retry: false,
  });
  const first = pages.data?.pages[0];
  const loaded = pages.data?.pages.flatMap(page => page.entries) ?? [];
  const lookup = useQuery({
    queryKey: ['geosite-lookup', catalogSource, target],
    queryFn: ({ signal }) => lookupGeosite(target, signal),
    enabled: kind === 'geosite' && !source && !!target && first?.hit === false,
    staleTime: 60000,
    retry: false,
  });
  const canLoad = !!pages.hasNextPage && !pages.isFetchingNextPage && loaded.length < LOADED_LIMIT;
  const fetchNext = pages.fetchNextPage;
  const sentinel = useRef<HTMLDivElement>(null);
  // 滚动到底自动取下一页；没有 IntersectionObserver 的环境仍可点「继续加载」。
  useEffect(() => {
    const node = sentinel.current;
    if (!node || !canLoad || typeof IntersectionObserver === 'undefined') return;
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) void fetchNext();
      },
      { root: listRef.current, rootMargin: '240px' },
    );
    observer.observe(node);
    return () => observer.disconnect();
  }, [canLoad, fetchNext, listRef]);

  const kinds = Object.entries(first?.counts ?? {}).sort(([a], [b]) => kindRank(a) - kindRank(b));
  const hits = first?.hits ?? [];
  const hitKey = (e: GeoEntry) => `${e.kind}\0${e.value}`;
  const hitKeys = new Set(hits.map(hitKey));
  // 文本筛选不到任何条目时（输入的是完整域名，通常如此），列出命中的条目本身。
  const shown = target && !loaded.length && hits.length ? hits : loaded;
  // 各类型的条目数只与属性有关；文本筛选后不再是该节的数量。
  const counted = !settled;
  const attrs = [...category.attrs].sort(
    (a, b) => (category.attr_counts?.[b] ?? 0) - (category.attr_counts?.[a] ?? 0) || a.localeCompare(b),
  );
  const region = kind === 'geoip' ? regionName(category.code) : '';

  return (
    <>
      <div className="geo-browser-head">
        <div className="geo-browser-title">
          <BackButton onClick={onBack} />
          <GeoGlyph kind={kind} code={category.code} />
          <h3 className="geo-browser-name">
            {name}
            {attr && <span>@{attr}</span>}
          </h3>
          {region && <span className="geo-browser-total">{region}</span>}
          <span className="geo-browser-total">{fmt(count)} 条</span>
          <span className="geo-browser-grow" />
          {onToggle && (
            <BrowserAction
              index={index}
              value={value}
              selected={selected}
              blockedValues={blockedValues}
              scope={scope}
              onAttr={onAttr}
              onToggle={onToggle}
              onReplace={onReplace}
            />
          )}
        </div>
        <Relations
          kind={kind}
          category={category}
          narrower={narrower.length}
          references={references.length}
          onGo={onGo}
          onTab={onTab}
        />
        {attrs.length > 0 && (
          <div className="geo-browser-seg" role="group" aria-label="属性">
            <button
              type="button"
              className="sans"
              aria-label={`全部 ${fmt(category.count)} 条`}
              aria-pressed={!attr}
              onClick={() => onAttr('')}
            >
              全部<small>{fmt(category.count)}</small>
            </button>
            {attrs.map(a => (
              <button
                key={a}
                type="button"
                aria-label={`@${a} ${fmt(category.attr_counts?.[a] ?? 0)} 条`}
                aria-pressed={attr === a}
                onClick={() => onAttr(a)}
              >
                @{a}
                <small>{fmt(category.attr_counts?.[a] ?? 0)}</small>
              </button>
            ))}
          </div>
        )}
        {tab === 'entries' && (
          <div className="geo-browser-tools">
            <FindInput
              value={text}
              onChange={onText}
              label="筛选条目或检查命中"
              placeholder={kind === 'geosite' ? '筛选条目，或输入域名检查命中' : '筛选条目，或输入 IP 检查命中'}
            />
            {kinds.length > 0 && (
              <div className="geo-browser-kinds" role="group" aria-label="条目类型">
                {kinds.map(([k, n]) => (
                  <button
                    key={k}
                    type="button"
                    aria-label={`${kindLabel(k)} ${fmt(n)} 条`}
                    aria-pressed={entryKind === k}
                    title={kinds.length > 1 ? `只看${kindLabel(k)}` : kindLabel(k)}
                    onClick={() => onEntryKind(entryKind === k ? '' : k)}
                  >
                    <i className={`geo-browser-mark ${k}`} />
                    {kindLabel(k)}
                    <b>{fmt(n)}</b>
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
      {tab === 'narrower' ? (
        <div className="geo-browser-list" ref={listRef}>
          <SectionHead title={`被 ${name} 完整包含的分类`} count={narrower.length} onBack={() => onTab('entries')} />
          <div className="geo-browser-cards">
            {narrower
              .map(code => index.byCode.get(code))
              .filter((entry): entry is GeoCategory => !!entry)
              .sort((a, b) => b.count - a.count || a.code.localeCompare(b.code))
              .map(entry => (
                <button
                  key={entry.code}
                  type="button"
                  className="geo-browser-card"
                  aria-label={`${entry.code} ${fmt(entry.count)} 条`}
                  onClick={() => onGo(entry.code)}
                >
                  <GeoGlyph kind={kind} code={entry.code} />
                  <code>{entry.code}</code>
                  <small>{fmt(entry.count)}</small>
                </button>
              ))}
          </div>
          <p className="geo-browser-hint">
            这些分类的全部条目都在 <code>{name}</code> 中；同一行已有 <code>{name}</code> 时不能再加它们。
          </p>
        </div>
      ) : tab === 'refs' ? (
        <div className="geo-browser-list" ref={listRef}>
          <SectionHead title="引用位置" count={references.length} onBack={() => onTab('entries')} />
          {references.map((r, i) => (
            <p key={i} className="geo-browser-ref">
              <code>{r.location}</code>
              <span>规则 {r.rule + 1}</span>
              <code>{r.selector}</code>
            </p>
          ))}
        </div>
      ) : (
        <>
          {target && (
            <Verdict
              target={target}
              value={value}
              pending={pages.isPending}
              error={pages.error}
              hit={first?.hit}
              hits={hits}
              alternatives={(lookup.data?.hits ?? []).filter(h => h.code !== category.code)}
              onGo={code => onGo(code, true)}
            />
          )}
          <div className="geo-browser-list" ref={listRef}>
            {!target && pages.error && <p className="geo-browser-empty error">{pages.error.message}</p>}
            {pages.isPending && !target && <p className="geo-browser-empty">加载中…</p>}
            {!pages.isPending && !shown.length && !target && (
              <p className="geo-browser-empty">{settled ? `没有条目包含 “${settled}”` : '没有条目'}</p>
            )}
            <EntrySections
              entries={shown}
              counts={counted ? first?.counts : undefined}
              part={settled}
              isHit={entry => hitKeys.has(hitKey(entry))}
            />
            {shown === loaded && loaded.length > 0 && (
              <div className="geo-browser-end" ref={sentinel}>
                {pages.hasNextPage ? (
                  loaded.length < LOADED_LIMIT ? (
                    <>
                      已显示 {fmt(loaded.length)} / {fmt(first?.total ?? 0)} 条
                      <button
                        type="button"
                        disabled={pages.isFetchingNextPage}
                        onClick={() => void pages.fetchNextPage()}
                      >
                        {pages.isFetchingNextPage ? '加载中…' : '继续加载'}
                      </button>
                    </>
                  ) : (
                    <>
                      已显示 {fmt(loaded.length)} / {fmt(first?.total ?? 0)} 条，用上方筛选或类型缩小范围
                    </>
                  )
                ) : (
                  <>共 {fmt(first?.total ?? loaded.length)} 条</>
                )}
              </div>
            )}
          </div>
        </>
      )}
    </>
  );
}

/** 相对来源行的状态：已在行内、同名换属性、被行内或前面规则包含，或可以加入。 */
function BrowserAction({
  index,
  value,
  selected,
  blockedValues,
  scope,
  onAttr,
  onToggle,
  onReplace,
}: {
  index: GeoIndex;
  value: string;
  selected: string[];
  blockedValues: string[];
  scope: string;
  onAttr: (attr: string) => void;
  onToggle: (value: string) => void;
  onReplace?: (from: string, to: string) => void;
}) {
  const lower = value.toLowerCase();
  const base = lower.split('@')[0];
  const existing = selected.find(v => v.toLowerCase() === lower);
  const sibling = selected.find(v => v.toLowerCase().split('@')[0] === base && v.toLowerCase() !== lower);
  if (existing)
    return (
      <div className="geo-browser-act">
        <span className="geo-browser-state">
          <Icon of="check" size={13} />
          {spaced('已在', scope)}
        </span>
        <button type="button" className="btn" onClick={() => onToggle(existing)}>
          移除
        </button>
      </div>
    );
  if (sibling && onReplace)
    return (
      <div className="geo-browser-act">
        <button type="button" className="btn" onClick={() => onAttr(sibling.split('@').slice(1).join('@'))}>
          取消
        </button>
        <button type="button" className="btn primary" onClick={() => onReplace(sibling, value)}>
          把 {sibling} 改为 {value}
        </button>
      </div>
    );
  const reason = geoBlockReason(index, value, selected, blockedValues);
  return (
    <div className="geo-browser-act">
      {reason && (
        <span className="geo-browser-reason">
          {reason.startsWith('本条') ? spaced(scope, reason.slice(2)) : reason}
        </span>
      )}
      <button type="button" className="btn primary" disabled={!!reason} onClick={() => onToggle(value)}>
        <Icon of="plus" size={13} />
        {spaced('加入', scope)}
      </button>
    </div>
  );
}

function Relations({
  kind,
  category,
  narrower,
  references,
  onGo,
  onTab,
}: {
  kind: GeodataKind;
  category: GeoCategory;
  narrower: number;
  references: number;
  onGo: (code: string) => void;
  onTab: (tab: Tab) => void;
}) {
  const within = category.within ?? [];
  if (kind !== 'geosite' && !references) return null;
  return (
    <div className="geo-browser-rel">
      {kind === 'geosite' && (
        <span>
          {within.length ? (
            <>
              <em>包含于</em>
              {within.map((code, i) => (
                <span key={code}>
                  {i > 0 && <em>·</em>}
                  <button type="button" className="geo-browser-link" onClick={() => onGo(code)}>
                    {code}
                  </button>
                </span>
              ))}
            </>
          ) : (
            <em>不被其他分类完整包含</em>
          )}
        </span>
      )}
      {narrower > 0 && (
        <span>
          <em>包含</em>
          <button type="button" className="geo-browser-link sans" onClick={() => onTab('narrower')}>
            {fmt(narrower)} 个分类
          </button>
        </span>
      )}
      {references > 0 && (
        <span>
          <em>引用</em>
          <button type="button" className="geo-browser-link sans" onClick={() => onTab('refs')}>
            {fmt(references)} 处
          </button>
        </span>
      )}
    </div>
  );
}

function Verdict({
  target,
  value,
  pending,
  error,
  hit,
  hits,
  alternatives,
  onGo,
}: {
  target: string;
  value: string;
  pending: boolean;
  error: Error | null;
  hit: boolean | null | undefined;
  hits: GeoEntry[];
  alternatives: { code: string; count: number }[];
  onGo: (code: string) => void;
}) {
  if (error) return <p className="geo-browser-verdict error">{error.message}</p>;
  if (pending || hit === undefined) return <p className="geo-browser-verdict miss">正在检查 {target}…</p>;
  if (hit === null)
    return (
      <p className="geo-browser-verdict miss">
        <b>待确认</b>
        <code>{value}</code> 含需 Xray 引擎确认的条目
      </p>
    );
  if (hit)
    return (
      <p className="geo-browser-verdict" role="status">
        <b>
          <Icon of="check" size={13} />
          命中
        </b>
        <code>{target}</code>
        {hits.length > 0 && (
          <span>
            由{' '}
            {hits.slice(0, 3).map((h, i) => (
              <span key={`${h.kind}:${h.value}`}>
                {i > 0 && '、'}
                <span className={`geo-browser-tag ${h.kind}`}>{kindLabel(h.kind)}</span>
                <code>{h.value}</code>
              </span>
            ))}
            {hits.length > 3 && ` 等 ${hits.length} 条`} 匹配
          </span>
        )}
      </p>
    );
  return (
    <p className="geo-browser-verdict miss" role="status">
      <b>未命中</b>
      <span>
        <code>{value}</code> 不含 <code>{target}</code>
      </span>
      {alternatives.length > 0 && (
        <>
          <span>· 包含它的分类</span>
          <span className="geo-browser-alts">
            {alternatives.slice(0, 4).map(h => (
              <button
                key={h.code}
                type="button"
                className="geo-browser-link"
                aria-label={`${h.code} ${fmt(h.count)} 条`}
                onClick={() => onGo(h.code)}
              >
                {h.code} <b>{fmt(h.count)}</b>
              </button>
            ))}
          </span>
        </>
      )}
    </p>
  );
}

function SectionHead({ title, count, onBack }: { title: string; count: number; onBack: () => void }) {
  return (
    <div className="geo-browser-sec">
      <b>{title}</b>
      <span>{fmt(count)}</span>
      <button type="button" className="geo-browser-link sans" onClick={onBack}>
        返回条目
      </button>
    </div>
  );
}

/**
 * 条目按类型分节（接口已按类型排序，节内保持文件顺序）。每节按值长度的第 80 百分位定最小列宽：
 * 短域名排三四列，长的两列，不跨列也不留空洞；个别超长的省略显示，悬停看全文。
 */
function EntrySections({
  entries,
  counts,
  part,
  isHit,
}: {
  entries: GeoEntry[];
  counts?: Record<string, number>;
  part: string;
  isHit: (entry: GeoEntry) => boolean;
}) {
  const sections: { kind: string; entries: GeoEntry[] }[] = [];
  for (const entry of entries) {
    const last = sections.at(-1);
    if (last?.kind === entry.kind) last.entries.push(entry);
    else sections.push({ kind: entry.kind, entries: [entry] });
  }
  return (
    <>
      {sections.map((section, i) => {
        const lengths = section.entries
          .map(e => e.value.length + (e.attrs.length ? e.attrs.join(' ').length + 3 : 0))
          .sort((a, b) => a - b);
        const p80 = lengths[Math.floor(lengths.length * 0.8)] ?? 20;
        const min =
          section.kind === 'regexp' ? '100%' : `${Math.min(420, Math.max(150, Math.round(p80 * 7.25 + 20)))}px`;
        return (
          <section key={`${section.kind}:${i}`} className="geo-browser-section" aria-label={kindLabel(section.kind)}>
            <div className="geo-browser-sec" aria-hidden="true">
              <i className={`geo-browser-mark ${section.kind}`} />
              <b>{kindLabel(section.kind)}</b>
              {counts?.[section.kind] !== undefined && <span>{fmt(counts[section.kind])}</span>}
            </div>
            <div className="geo-browser-grid" style={{ '--min': min } as React.CSSProperties}>
              {section.entries.map((entry, j) => {
                const attrs = entry.attrs.map(a => `@${a}`).join(' ');
                return (
                  <div
                    key={`${entry.value}:${j}`}
                    className={`geo-browser-entry${isHit(entry) ? ' hit' : ''}`}
                    title={`${kindLabel(entry.kind)}  ${entry.value}${attrs ? `  ${attrs}` : ''}`}
                  >
                    <code>
                      <Highlight text={entry.value} part={part} />
                    </code>
                    {attrs && <small>{attrs}</small>}
                  </div>
                );
              })}
            </div>
          </section>
        );
      })}
    </>
  );
}

function Highlight({ text, part }: { text: string; part: string }) {
  const at = part ? text.toLowerCase().indexOf(part) : -1;
  if (at < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, at)}
      <mark>{text.slice(at, at + part.length)}</mark>
      {text.slice(at + part.length)}
    </>
  );
}
