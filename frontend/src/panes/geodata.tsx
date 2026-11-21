import { useEffect, useId, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { fetchGeoCatalog, fetchGeoRelations, geoValueIssue, geoSuggestions, type GeodataKind } from '../api/geodata';
import { DialogLayer } from '../ui/dialog';
import { Icon } from '../ui/icons';
import { useNarrow } from '../ui/viewport';
import { GeoBrowser } from './geo-browser';
import { GeoGlyph, GeoMenu } from './geo-menu';
import { geoBlockReason, geoIndex } from './geo-menu-model';
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
  scope,
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
  /** 这一行的简称，如「D1」「规则 3」；预览里写作「加入 D1」。 */
  scope?: string;
}) {
  const catalog = useGeoCatalog(kind, !unavailable);
  const anchor = useRef<HTMLDivElement>(null);
  const menuInput = useRef<HTMLInputElement>(null);
  const fieldInput = useRef<HTMLInputElement>(null);
  const narrow = useNarrow();
  const [keyboard, setKeyboard] = useState(false);
  const [selectionRequest, setSelectionRequest] = useState(0);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [preview, setPreview] = useState<string | null>(null);
  const [shown, setShown] = useState(1);
  const id = useId();
  const compact = narrow && open;
  const typing = compact && (keyboard || query.length > 0);
  const visibleValues = compact ? values.slice(typing ? -1 : -4) : values.slice(0, shown);
  const close = () => {
    setOpen(false);
    if (narrow) fieldInput.current?.blur();
  };
  useEffect(() => {
    const viewport = window.visualViewport;
    if (!viewport) return;
    const update = () => setKeyboard(window.innerHeight - viewport.height > 100);
    update();
    viewport.addEventListener('resize', update);
    return () => viewport.removeEventListener('resize', update);
  }, []);
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
  const unknown = !catalog.data && catalog.isError && values.length > 0;
  const candidates = (catalog.data?.entries ?? []).flatMap(entry => [
    entry.code,
    ...entry.attrs.map(a => `${entry.code}@${a}`),
  ]);
  const invalidValue = values.find(v => geoValueIssue(catalog.data, v));
  const correction = invalidValue ? geoSuggestions(candidates, invalidValue)[0] : undefined;
  const remove = (value: string) => onChange(values.filter(v => v !== value));
  // 已选再点一次即移除；新值必须存在于同源目录，且不能被本条或前面规则已有的分类包含。
  const toggle = (value: string) => {
    if (disabled) return;
    const existing = values.find(v => v.toLowerCase() === value.toLowerCase());
    if (existing) return remove(existing);
    if (geoValueIssue(catalog.data, value) || !catalog.data) return;
    const index = geoIndex(kind, catalog.data, values);
    if (geoBlockReason(index, value, values, blockedValues)) return;
    onChange([...values, value]);
    if (!(narrow && value.includes('@'))) setQuery('');
    (narrow ? fieldInput : menuInput).current?.focus({ preventScroll: true });
  };
  // 预览中换属性（google → google@cn）：原位替换，同样不能被本条或前面规则包含。
  const replace = (from: string, to: string) => {
    if (disabled || !catalog.data || geoValueIssue(catalog.data, to)) return;
    const rest = values.filter(v => v !== from);
    if (geoBlockReason(geoIndex(kind, catalog.data, rest), to, rest, blockedValues)) return;
    onChange(values.map(v => (v === from ? to : v)));
  };
  return (
    <div className="geo-field">
      <div
        ref={anchor}
        className={`f geo-select${error ? ' invalid' : ''}${compact ? ' editing' : ''}${typing ? ' typing' : ''}`}
        aria-disabled={disabled}
        aria-busy={catalog.isFetching}
      >
        {visibleValues.map(value => (
          <span className={`geo-chip${geoValueIssue(catalog.data, value) ? ' invalid' : ''}`} key={value} title={value}>
            <button
              type="button"
              className="geo-chip-name"
              onClick={() => {
                setOpen(false);
                setPreview(value);
              }}
              aria-label={`预览 ${value}`}
            >
              <GeoGlyph kind={kind} code={value.split('@')[0]} />
              <span>{value}</span>
            </button>
            <button type="button" disabled={disabled} aria-label={`移除 ${value}`} onClick={() => remove(value)}>
              ×
            </button>
          </span>
        ))}
        {values.length > visibleValues.length && (
          <button
            type="button"
            disabled={disabled}
            className="geo-more-count"
            aria-label={`查看全部 ${values.length} 个已选`}
            onClick={() => {
              setQuery('');
              setSelectionRequest(v => v + 1);
              setOpen(true);
            }}
          >
            +{values.length - visibleValues.length}
          </button>
        )}
        <span className="geo-input-line">
          <input
            ref={fieldInput}
            aria-label={label}
            maxLength={253}
            role="combobox"
            aria-expanded={open}
            aria-controls={open ? id : undefined}
            aria-autocomplete="list"
            value={query}
            disabled={disabled}
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            enterKeyHint="search"
            placeholder={compact ? '继续添加…' : values.length ? '' : kind === 'geosite' ? '如 netflix' : '如 cn'}
            onClick={() => setOpen(true)}
            onChange={e => {
              setQuery(e.target.value);
              setOpen(true);
            }}
            onKeyDown={e => {
              if (e.nativeEvent.isComposing) return;
              if (!open && (e.key === 'Enter' || e.key === 'ArrowDown')) {
                e.preventDefault();
                setOpen(true);
              }
              if (e.key === 'Backspace' && !query && values.length) onChange(values.slice(0, -1));
              if (e.key === 'Escape') close();
            }}
          />
          <button
            type="button"
            disabled={disabled}
            className={compact ? 'geo-done' : 'geo-chevron'}
            aria-label={compact ? `完成 ${kind} 选择` : `展开 ${kind}`}
            onClick={() => {
              if (open) close();
              else setOpen(true);
            }}
          >
            {compact ? '完成' : <Icon of="chevronDown" size={14} />}
          </button>
        </span>
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
          align="below"
          gap={2}
          modal={!narrow}
          className={`geo-menu-layer${narrow ? ' geo-menu-compact' : ''}`}
          label={`${kind} 选项`}
          onClose={() => setOpen(false)}
        >
          <GeoMenu
            kind={kind}
            catalog={catalog}
            values={values}
            blockedValues={blockedValues}
            disabled={disabled}
            query={query}
            onQuery={setQuery}
            onToggle={toggle}
            onRemove={remove}
            onPreview={value => {
              setPreview(value);
              setOpen(false);
            }}
            compact={narrow}
            selectionRequest={selectionRequest}
            inputRef={narrow ? fieldInput : menuInput}
            listId={id}
          />
        </DialogLayer>
      )}
      {preview !== null && (
        <GeoBrowser
          kind={kind}
          catalog={catalog}
          initial={preview}
          selected={values}
          blockedValues={blockedValues}
          scope={scope}
          onToggle={disabled ? undefined : toggle}
          onReplace={disabled ? undefined : replace}
          onClose={() => setPreview(null)}
        />
      )}
    </div>
  );
}

/** 规则集预览：自己加载目录（设置页可传入未保存的来源），界面见 `GeoBrowser`。 */
export function GeodataBrowser({ kind, source, ...props }: Omit<Parameters<typeof GeoBrowser>[0], 'catalog'>) {
  const catalog = useGeoCatalog(kind, true, source);
  return <GeoBrowser kind={kind} source={source} catalog={catalog} {...props} />;
}
