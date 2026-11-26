import { useQuery } from '@tanstack/react-query';
import { useId, useState } from 'react';
import { fetchCvmPendingReview, planDeployment, type CvmInstance, type CvmWorkspace } from '../../api';
import { navigate } from '../../forge/route';
import { ErrorBox } from '../../ui/bits';
import { diffLines } from '../../forge/diff';
import { actionBlocked, ListCommandDialog, type ListCommand } from './instance-list-actions';
import { editCount, type NetworkDraftController } from './network-draft';
import { stagedEntryLabel } from './network-entries';
import { hasPendingCvmChange, operationText } from './queries';

const networkKinds = new Set([
  'entries',
  'routes',
  'mount',
  'detach',
  'network_edit',
  'network-edit',
  'nic_version',
  'nic_transport',
]);

function ChangeLocation({
  instance,
  network = false,
  label,
}: {
  instance: CvmInstance;
  network?: boolean;
  label?: string;
}) {
  return (
    <button
      className="fg-draft-location fg-draft-name"
      type="button"
      onClick={() =>
        navigate('cvm', {
          p: 'instance',
          id: instance.desired.id,
          ...(network ? { tab: 'network' } : {}),
        })
      }
    >
      {instance.desired.spec.name}
      {label ? ` · ${label}` : ''}
    </button>
  );
}

export function CvmPendingChanges({
  workspace,
  network,
}: {
  workspace: CvmWorkspace;
  network: NetworkDraftController;
}) {
  const [showPending, setShowPending] = useState(false);
  const [showDraft, setShowDraft] = useState(false);
  const pendingId = useId();
  const draftId = useId();
  const [command, setCommand] = useState<ListCommand | null>(null);
  const [notice, setNotice] = useState('');
  const pending = workspace.instances.filter(hasPendingCvmChange);
  const revision = workspace.pending_revision_id;
  const canPublish = !!workspace.can_publish && !workspace.redacted;
  const canReview = workspace.can_write && !workspace.redacted;
  const hasChanges = pending.length > 0 || ((canReview || canPublish) && revision != null);
  const plan = useQuery({
    queryKey: ['plan', revision],
    queryFn: () => planDeployment(revision!),
    enabled: revision != null && canPublish && showPending && hasChanges,
    retry: false,
  });
  const changes = network.draft?.request;
  const count = changes ? editCount(changes) : 0;
  const totalDrafts = count + network.others.reduce((sum, item) => sum + editCount(item.request), 0);
  if (!hasChanges && !command && !notice && !totalDrafts) return null;
  const title = pending.length ? `${pending.length} 个容器待发布` : '容器与网络有待发布变更';
  const otherMachines = plan.data?.targets.filter(target => target.actions.some(action => action !== 'sync-cvm')) ?? [];
  const machineName = (node: string) => workspace.hosts.find(host => host.node_id === node)?.name ?? node;
  return (
    <div className="fg-draft cvm-pending-changes" aria-label="容器草稿与待发布变更">
      {totalDrafts > 0 && (
        <div className="fg-draft-head">
          <button
            type="button"
            className="fg-draft-count"
            aria-expanded={showDraft}
            aria-controls={showDraft ? draftId : undefined}
            onClick={() => setShowDraft(!showDraft)}
          >
            <span aria-hidden="true">{showDraft ? '▾' : '▸'} </span>
            {totalDrafts} 处未保存
          </button>
          <span className="sp" />
          {network.draft && (
            <>
              <button
                className="btn"
                type="button"
                disabled={network.busy || network.draft.uncertain}
                onClick={network.discard}
              >
                丢弃当前草稿
              </button>
              {!network.draft.uncertain && (
                <button
                  className="btn"
                  type="button"
                  disabled={network.busy || network.stale}
                  onClick={() => {
                    setShowDraft(true);
                    network.submit(false);
                  }}
                >
                  查看配置差异
                </button>
              )}
              <button
                className="btn primary"
                type="button"
                disabled={network.busy || network.stale}
                onClick={() => network.submit(true)}
              >
                {network.busy ? '正在检查或保存…' : network.draft.uncertain ? '重试原保存' : '保存网络变更'}
              </button>
            </>
          )}
        </div>
      )}
      {network.error && (
        <div className="fg-draft-err">
          <ErrorBox error={network.error} />
        </div>
      )}
      {network.stale && (
        <p className="note fg-draft-err" role="alert">
          容器设置已变化，草稿仍保留。请核对变更后丢弃此草稿，再基于当前配置编辑。
        </p>
      )}
      {network.draft?.uncertain && (
        <p className="note fg-draft-err" role="alert">
          上次保存结果尚未确认。重试会使用原请求，避免重复分配。
        </p>
      )}
      {showDraft && totalDrafts > 0 && (
        <ul className="fg-draft-list" id={draftId}>
          {network.others.map(item => (
            <li key={item.instance.desired.id}>
              <ChangeLocation instance={item.instance} network label={`${editCount(item.request)} 处未保存`} />
            </li>
          ))}
          {changes?.entry_additions?.map((entry, index) => (
            <li key={`add-entry-${index}`}>添加入站方式 · {stagedEntryLabel(entry)}</li>
          ))}
          {changes?.entry_releases?.map(entry => (
            <li key={entry.id}>
              撤销入站方式 · <code>{entry.id}</code>
            </li>
          ))}
          {changes?.mounts?.map(nic => (
            <li key={nic.draft_id}>
              添加出站方式 · <code>{nic.publication_id}</code> · {nic.transport}
              {nic.use_dns_suggestions ? ' · 使用网络提供的 DNS' : ''}
            </li>
          ))}
          {changes?.detach_interface_ids?.map(id => (
            <li key={id}>
              移除出站方式 · <code>{id}</code>
            </li>
          ))}
          {changes?.routes && network.draft && (
            <li>
              <details>
                <summary>路由与 DNS 差异</summary>
                <RouteDiff before={network.draft.instance} after={changes.routes.routes} />
              </details>
            </li>
          )}
          {network.draft?.review && <li role="status">服务端配置检查通过。保存后进入待发布修订。</li>}
        </ul>
      )}
      {hasChanges && (
        <div className="fg-draft-head">
          {canReview || canPublish ? (
            <button
              type="button"
              className="fg-draft-count"
              aria-expanded={showPending}
              aria-controls={showPending ? pendingId : undefined}
              onClick={() => setShowPending(!showPending)}
            >
              <span aria-hidden="true">{showPending ? '▾' : '▸'} </span>
              {title}
            </button>
          ) : (
            <span role="status">变更已保存，等待管理员发布</span>
          )}
          {(canReview || canPublish) && revision != null && (
            <span className="note">
              变更已保存，将随<b>发布计划</b>统一生效（当前修订 R{revision}）。
            </span>
          )}
          <span className="sp" />
          {canPublish && (
            <button className="btn" onClick={() => navigate('deploy', { p: 'plan' })}>
              查看发布计划
            </button>
          )}
        </div>
      )}
      {hasChanges && showPending && (canReview || canPublish) && (
        <ul className="fg-draft-list" id={pendingId}>
          {pending.map(instance => {
            const kind = instance.operation_kind ?? '';
            const inNetwork = networkKinds.has(kind);
            const label = inNetwork ? '网络' : kind === 'create' ? '创建' : kind === 'assign' ? '使用授权' : undefined;
            const blocked = actionBlocked('undo_pending', instance, undefined, workspace);
            return (
              <li key={instance.desired.id}>
                <div className="fg-draft-entry">
                  <ChangeLocation instance={instance} network={inNetwork} label={label} />
                  <span>{operationText[kind] ?? '容器配置'}</span>
                  {instance.blocking_deployment_id != null && (
                    <span>发布单 #{instance.blocking_deployment_id} 已锁定</span>
                  )}
                  {!instance.user_id && kind === 'assign' && <span>控制台访问已收回，不可撤回</span>}
                  {canReview && <PendingReview instance={instance} />}
                </div>
                {canReview &&
                  (blocked ? (
                    instance.blocking_deployment_id == null &&
                    kind !== 'assign' && <span className="note">{blocked}</span>
                  ) : (
                    <button
                      className="btn sm"
                      onClick={() => {
                        setNotice('');
                        setCommand({ action: 'undo_pending', instances: [instance] });
                      }}
                    >
                      撤回
                    </button>
                  ))}
              </li>
            );
          })}
          {plan.isFetching && <li>正在读取同修订的发布检查…</li>}
          {plan.error && (
            <li>
              <ErrorBox error={plan.error} />
            </li>
          )}
          {plan.data && (
            <li className="cvm-pending-plan">
              <span className="note">
                同修订发布检查：{plan.data.summary.changed_targets} 台机器待更新
                {otherMachines.length
                  ? ` · 同时包含 ${otherMachines.map(target => machineName(target.node_id)).join('、')} 的机器配置`
                  : ''}
                {plan.data.warnings.map((warning, index) => (
                  <span className="cvm-pending-warning" key={index}>
                    {warning.message}
                  </span>
                ))}
              </span>
            </li>
          )}
        </ul>
      )}
      {notice && (
        <p className="note fg-draft-err" role="status">
          {notice}
        </p>
      )}
      {command && (
        <ListCommandDialog
          command={command}
          workspace={workspace}
          onClose={() => setCommand(null)}
          onSaved={(_, message) => setNotice(message)}
        />
      )}
    </div>
  );
}

const fieldNames: Record<string, string> = {
  power: '电源状态',
  restart: '重启',
  image: '系统镜像',
  name: '名称',
  resources: '资源规格',
  interfaces: '出站方式',
  routes: '路由与优先级',
  dns: 'DNS',
  login: '登录凭证',
  create: '实例创建',
  access: '使用授权',
};

function PendingReview({ instance }: { instance: CvmInstance }) {
  const [open, setOpen] = useState(false);
  const id = instance.desired.id;
  const query = useQuery({
    queryKey: ['cvm-pending-review', id, instance.revision_id],
    queryFn: () => fetchCvmPendingReview(id),
    enabled: open,
    retry: false,
  });
  const data = query.data;
  return (
    <details onToggle={event => setOpen(event.currentTarget.open)}>
      <summary>变更详情</summary>
      {query.isFetching && <p className="note">正在读取已保存的变更…</p>}
      {query.error && <ErrorBox error={query.error} />}
      {data && (
        <div className="cvm-pending-review">
          <p className="note">
            {data.author ? `${data.author} · ` : ''}
            {new Date(data.saved_at).toLocaleString('zh-CN')} · R{data.revision}
          </p>
          {!!data.fields.length && <p>{data.fields.map(field => fieldNames[field] ?? field).join('、')}</p>}
          {!!data.entries_added && <p>新增 {data.entries_added} 个入站方式</p>}
          {!!data.entries_removed && <p>撤销 {data.entries_removed} 个入站方式</p>}
          {!!data.nics_added && <p>添加 {data.nics_added} 个出站方式</p>}
          {!!data.nics_removed && <p>移除 {data.nics_removed} 个出站方式</p>}
        </div>
      )}
    </details>
  );
}

function RouteDiff({ before, after }: { before: CvmInstance; after: CvmInstance['desired']['spec']['routing'] }) {
  const format = (routing: typeof after) =>
    JSON.stringify({ routes: routing.routes, priorities: routing.priorities, dns: routing.dns }, null, 2);
  return (
    <pre className="cvm-artifact-content" aria-label="路由与 DNS 差异">
      {diffLines(format(before.desired.spec.routing), format(after)).map((line, index) => (
        <span key={index} className={line.t === '+' ? 'added' : line.t === '-' ? 'removed' : ''}>
          {line.t} {line.s}
        </span>
      ))}
    </pre>
  );
}
