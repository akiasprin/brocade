import { ChoiceSelect } from '../../ui/choice-menu';
import { lazy, Suspense, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { fetchClashRules, saveClashRules, previewClashRules, probeClashRules, type ClashRulesView } from '../../api';
import { Confirm, ErrorBox, Loading, SegmentedControl } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import type { SubStoreNavigationGuard } from '../subscription-substore';
import { AddRuleRow, ModuleRow, RuleRow, ServiceAddRow, TargetLabel, ops } from './controls';
import { sameRules, type Rules } from '../../clash-rules';
import { RoutingProvider, normalizeRules, useRouting } from './context';
const PrivateChain = lazy(() => import('./private-chain').then(module => ({ default: module.PrivateChain })));

export const rulesKey = (tenant: string, user: string) => ['clash-rules', tenant, user];
export function ClashRulesEditor({
  tenant,
  user,
  onClose,
  onGuard,
}: {
  tenant: string;
  user: string;
  onClose: () => void;
  onGuard: (guard: SubStoreNavigationGuard | null) => void;
}) {
  const query = useQuery({
    queryKey: rulesKey(tenant, user),
    queryFn: ({ signal }) => fetchClashRules(tenant, user, signal),
    staleTime: 0,
    retry: false,
  });
  return (
    <aside className="clash-editor-pane" aria-label="规则">
      <div className="clash-editor-pane-inner">
        {query.data ? (
          <RoutingProvider catalog={query.data.catalog}>
            <Editor
              key={`${tenant}/${user}`}
              tenant={tenant}
              user={user}
              initial={query.data}
              onClose={onClose}
              onGuard={onGuard}
            />
          </RoutingProvider>
        ) : (
          <>
            <header className="clash-editor-head">
              <b>规则</b>
              <button className="sub-dialog-close" aria-label="收起规则" onClick={onClose}>
                <Icon of="close" size={14} />
              </button>
            </header>
            {query.error ? (
              <>
                <ErrorBox error={query.error} />
                <button className="btn" onClick={() => void query.refetch()}>
                  重试
                </button>
              </>
            ) : (
              <Loading variant="code" />
            )}
          </>
        )}
      </div>
    </aside>
  );
}
function Editor({
  tenant,
  user,
  initial,
  onClose,
  onGuard,
}: {
  tenant: string;
  user: string;
  initial: ClashRulesView;
  onClose: () => void;
  onGuard: (guard: SubStoreNavigationGuard | null) => void;
}) {
  const { FINAL, moduleOrder } = useRouting();
  const [saved, setSaved] = useState(() => normalizeRules(initial.rules, initial.catalog));
  const [draft, setDraft] = useState(saved);
  const [version, setVersion] = useState(initial.version);
  const [view, setView] = useState<'list' | 'yaml' | 'private'>('list');
  const [privatePending, setPrivatePending] = useState(false);
  const [leave, setLeave] = useState<{ next: () => void; rules: boolean } | null>(null);
  const [savedNotice, setSavedNotice] = useState(false);
  const [probe, setProbe] = useState('');
  const [probeKind, setProbeKind] = useState<'auto' | 'domain' | 'ip' | 'process'>('auto');
  const pane = useRef<HTMLDivElement>(null);
  const client = useQueryClient();
  const dirty = !sameRules(saved, draft);
  const remoteChanged = initial.version > version;
  const save = useMutation({
    mutationFn: () => saveClashRules(tenant, user, version, draft),
    onSuccess: async data => {
      await client.cancelQueries({ queryKey: rulesKey(tenant, user) });
      const rules = normalizeRules(data.rules, data.catalog);
      setSaved(rules);
      setDraft(rules);
      setVersion(data.version);
      setSavedNotice(true);
      client.setQueryData(rulesKey(tenant, user), data);
    },
  });
  const reload = useMutation({
    mutationFn: () => fetchClashRules(tenant, user),
    onSuccess: async data => {
      await client.cancelQueries({ queryKey: rulesKey(tenant, user) });
      const rules = normalizeRules(data.rules, data.catalog);
      setSaved(rules);
      setDraft(rules);
      setVersion(data.version);
      setSavedNotice(false);
      save.reset();
      client.setQueryData(rulesKey(tenant, user), data);
    },
  });
  // A refetch may replace cached preferences while this editor stays mounted. Adopt only
  // newer versions and only while clean, so neither local edits nor an in-flight save is lost.
  if (remoteChanged && !dirty && !save.isPending && !reload.isPending) {
    const rules = normalizeRules(initial.rules, initial.catalog);
    setSaved(rules);
    setDraft(rules);
    setVersion(initial.version);
    setSavedNotice(false);
  }
  const requestLeave = useCallback(
    (next: () => void, checkRules = true) => {
      if (save.isPending || reload.isPending) return false;
      if ((checkRules && dirty) || privatePending) {
        setLeave({ next, rules: checkRules });
        return false;
      }
      next();
      return true;
    },
    [dirty, privatePending, save.isPending, reload.isPending],
  );
  useLayoutEffect(() => {
    onGuard(requestLeave);
    return () => onGuard(null);
  }, [onGuard, requestLeave]);
  const set = (update: (rules: Rules) => Rules) => {
    check.reset();
    setSavedNotice(false);
    save.reset();
    setDraft(current => update(structuredClone(current)));
  };
  const preview = useQuery({
    queryKey: ['clash-rules-preview', tenant, user, draft],
    queryFn: ({ signal }) => previewClashRules(tenant, user, draft, signal),
    enabled: view === 'yaml',
    retry: false,
    gcTime: 0,
  });
  const check = useMutation({
    mutationFn: (input: { kind: 'domain' | 'ip' | 'process'; value: string; rules: Rules }) =>
      probeClashRules(tenant, user, input.rules, input.kind, input.value),
  });
  const activeHit = useMemo(
    () =>
      check.data &&
      !check.isPending &&
      check.variables &&
      probe.trim() === check.variables.value &&
      sameRules(check.variables.rules, draft)
        ? check.data
        : null,
    [check.data, check.isPending, check.variables, probe, draft],
  );
  useEffect(() => {
    const root = pane.current;
    if (!root) return;
    root.querySelectorAll('[data-hit]').forEach(el => el.removeAttribute('data-hit'));
    if (!activeHit || view !== 'list') return;
    const row = Array.from(root.querySelectorAll('[data-rule],[data-module]')).find(
      row => row.getAttribute('data-rule') === activeHit.id || row.getAttribute('data-module') === activeHit.id,
    );
    row?.setAttribute('data-hit', '');
    row?.scrollIntoView({ block: 'nearest' });
  }, [activeHit, view]);
  const switchView = (next: typeof view) => {
    if (next === view) return;
    const go = () => {
      setPrivatePending(false);
      setView(next);
    };
    if (view === 'private') requestLeave(go, false);
    else go();
  };
  const runProbe = () => {
    const value = probe.trim();
    const kind =
      probeKind !== 'auto'
        ? probeKind
        : value.includes(':') || /^\d+\.\d+\.\d+\.\d+$/.test(value)
          ? 'ip'
          : /\.(exe|app)$/.test(value) || !value.includes('.')
            ? 'process'
            : 'domain';
    check.mutate({ kind, value, rules: draft });
  };

  return (
    <>
      <header className="clash-editor-head">
        <b>规则</b>
        <SegmentedControl
          value={view}
          ariaLabel="规则视图"
          className="clash-editor-seg"
          options={[
            { value: 'list', label: '列表' },
            { value: 'yaml', label: 'YAML' },
            { value: 'private', label: '链式代理' },
          ]}
          onChange={switchView}
        />
        <button type="button" className="sub-dialog-close clash-editor-close" aria-label="收起规则" onClick={onClose}>
          <Icon of="close" size={14} />
        </button>
      </header>
      {view !== 'private' && (
        <>
          <form
            className="clash-editor-test"
            onSubmit={event => {
              event.preventDefault();
              runProbe();
            }}
          >
            <ChoiceSelect
              className="clash-rule-probe-kind"
              aria-label="匹配类型"
              value={probeKind}
              onChange={event => {
                setProbeKind(event.target.value as typeof probeKind);
                check.reset();
              }}
            >
              <option value="auto">自动</option>
              <option value="domain">域名</option>
              <option value="ip">IP</option>
              <option value="process">进程</option>
            </ChoiceSelect>
            <input
              aria-label="试一试：域名、IP 或进程名"
              placeholder="试一试：域名、IP 或进程名"
              spellCheck={false}
              value={probe}
              onChange={event => {
                setProbe(event.target.value);
                check.reset();
              }}
            />
            <button className="clash-rule-probe-go" type="submit" disabled={!probe.trim() || check.isPending}>
              {check.isPending ? '匹配中…' : '试一试'}
            </button>
          </form>
          <p className="clash-editor-scope-note">
            {activeHit ? (
              <span role="status">
                {activeHit.layer === 'custom'
                  ? '自定义规则'
                  : activeHit.id === 'final'
                    ? FINAL.name
                    : moduleOrder(draft).find(m => m.id === activeHit.id)?.name}{' '}
                → <TargetLabel target={activeHit.target} />
              </span>
            ) : (
              '按当前规则匹配，显示默认去向；客户端的手动选择以客户端为准。'
            )}
          </p>
          {check.error && <ErrorBox error={check.error} />}
        </>
      )}
      <div ref={pane} className={`clash-editor-body${view === 'private' ? ' clash-editor-private-body' : ''}`}>
        {view === 'private' ? (
          <Suspense fallback={<Loading variant="code" />}>
            <PrivateChain
              rules={draft}
              rulesDirty={dirty}
              onBack={() => switchView('list')}
              onPending={setPrivatePending}
            />
          </Suspense>
        ) : view === 'yaml' ? (
          <>
            {preview.error ? (
              <ErrorBox error={preview.error} />
            ) : preview.data ? (
              <pre className="clash-rule-yaml clash-editor-yaml">{preview.data.yaml}</pre>
            ) : (
              <Loading variant="code" />
            )}
            <p className="clash-editor-scope-note">私有链通过客户端扩展脚本合并，不包含在此预览中。</p>
          </>
        ) : (
          <fieldset disabled={!initial.writable || save.isPending} className="clash-rule-fields">
            <div className="clash-editor-sec">
              <b>自定义</b>
              <small>按顺序 · 先于分流</small>
            </div>
            <div className="substore-group clash-editor-table">
              {draft.custom.map((rule, index) => (
                <RuleRow key={rule.id} rule={rule} index={index} count={draft.custom.length} set={set} numbered />
              ))}
              <AddRuleRow
                numbered={draft.custom.length + 1}
                disabled={draft.custom.length >= 200}
                onAdd={(kind, value, target) => {
                  if (draft.custom.length < 200) {
                    check.reset();
                    set(ops.addRule(kind, value, target));
                  }
                }}
              />
            </div>
            <div className="clash-editor-sec">
              <b>分流</b>
              <small>MetaCubeX 规则集</small>
            </div>
            <div className="substore-group clash-editor-table">
              {moduleOrder(draft).map(def => (
                <ModuleRow
                  key={def.id}
                  def={def}
                  rules={draft}
                  set={update => {
                    check.reset();
                    set(update);
                  }}
                  detail={def.sets.slice(0, 3).join(' · ')}
                />
              ))}
              <ServiceAddRow rules={draft} set={set} />
            </div>
            <div className="clash-editor-sec">
              <b>兜底</b>
            </div>
            <div className="substore-group clash-editor-table">
              <ModuleRow def={FINAL} rules={draft} set={set} detail="MATCH" />
            </div>
          </fieldset>
        )}
      </div>
      {view !== 'private' && (
        <footer className="clash-editor-foot">
          <div className="clash-editor-foot-status" role="status">
            {!initial.writable
              ? '只读'
              : savedNotice && !dirty
                ? '规则已保存，更新订阅后生效。'
                : '只影响此用户的 Clash 订阅，无需发布。'}
          </div>
          <button
            className="btn"
            disabled={!dirty || save.isPending}
            onClick={() => {
              setDraft(saved);
              save.reset();
              check.reset();
            }}
          >
            还原
          </button>
          <button
            className="btn primary"
            disabled={!dirty || !initial.writable || save.isPending}
            onClick={() => save.mutate()}
          >
            {save.isPending ? '保存中…' : '保存规则'}
          </button>
        </footer>
      )}
      {save.error && <ErrorBox error={save.error} />}
      {(save.error || remoteChanged) && (
        <div className="clash-editor-foot">
          {remoteChanged && (
            <div className="clash-editor-foot-status" role="status">
              规则已在其他窗口更新，当前更改已保留。重新载入会放弃当前更改。
            </div>
          )}
          <button className="btn" disabled={reload.isPending} onClick={() => requestLeave(() => reload.mutate())}>
            重新载入规则
          </button>
        </div>
      )}
      {reload.error && <ErrorBox error={reload.error} />}
      {leave && (
        <Confirm
          title={dirty && leave.rules ? '放弃尚未保存的更改？' : '清空本页的私有节点？'}
          body={
            privatePending
              ? '私有节点尚未导出，离开后会清空。需要保留时，请返回导出到客户端。'
              : '普通规则尚未保存，离开后会放弃本次更改。'
          }
          confirmLabel="放弃并离开"
          onCancel={() => setLeave(null)}
          onConfirm={() => {
            const next = leave.next;
            if (leave.rules) setDraft(saved);
            setPrivatePending(false);
            setLeave(null);
            next();
          }}
        />
      )}
    </>
  );
}
