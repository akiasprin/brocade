import { useEffect, useState, useSyncExternalStore, type KeyboardEvent, type ReactNode } from 'react';
import type { NodeAgentStateItem } from '../api';
import { Ago, ErrorBox } from '../ui/bits';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { Icon } from '../ui/icons';

/* 机器退役与移出：详情页 ⋯ 菜单最后一项打开的弹层、退役后的状态横幅，以及移出后回到机器列表时的
   一次性提示。三处共用同一套状态判断，API 调用与缓存失效留在 NodeDetail。 */

type LifecyclePhase = NodeAgentStateItem['lifecycle_phase'];
export type NodeRetireMode = 'retire' | 'remove';

/**
 * 弹层中每种选择对应的服务端操作：
 * - `retire`：`setNodeStatus(id, 'retired')`，下发停用，等机器回报；
 * - `force-retire`：`abandonNode(id, true)`，不等回报立即完成；
 * - `schedule-removal`：`scheduleNodeRemoval(id)`，退役完成后由 Console 移出；
 * - `force-remove`：先 `abandonNode`，再 `removeRetiredNodes([id])`；
 * - `remove`：已退役的机器直接 `removeRetiredNodes([id])`。
 */
export type NodeRetireAction = 'retire' | 'force-retire' | 'schedule-removal' | 'force-remove' | 'remove';

const isEnded = (phase: LifecyclePhase) => phase === 'retired' || phase === 'abandoned';

/** null 表示当前状态下没有这个选择（例如退役中的机器不能再「仅退役」）。 */
export function nodeRetireAction(phase: LifecyclePhase, mode: NodeRetireMode, force: boolean): NodeRetireAction | null {
  if (isEnded(phase)) return mode === 'remove' ? 'remove' : null;
  if (mode === 'retire') return phase === 'active' ? (force ? 'force-retire' : 'retire') : null;
  return force ? 'force-remove' : 'schedule-removal';
}

/** 已安排的移出只对应当前这次退役；运行中的机器即使带着旧字段也不算已安排。 */
export const nodeRemovalQueued = (node: NodeAgentStateItem) =>
  node.lifecycle_phase !== 'active' && node.removal_status === 'queued';

/** ⋯ 菜单里「退役/移出」的说明随状态变化；已安排移出时置灰，避免重复提交。 */
export function nodeLifecycleMenuItem(node: NodeAgentStateItem): { note: string; scheduled: boolean } {
  if (nodeRemovalQueued(node)) {
    return node.lifecycle_phase === 'retiring'
      ? { note: '已安排：停用完成后自动移出', scheduled: true }
      : { note: '已安排：正在从机器列表移出', scheduled: true };
  }
  switch (node.lifecycle_phase) {
    case 'active':
      return { note: '仅退役，或退役后从机器列表移出', scheduled: false };
    case 'retiring':
      return { note: '退役进行中；可以安排移出', scheduled: false };
    default:
      return { note: '已退役；可以从机器列表移出', scheduled: false };
  }
}

const PHASE_LABEL: Record<Exclude<LifecyclePhase, 'active'>, string> = {
  retiring: '退役中',
  retired: '已退役',
  abandoned: '强制退役',
};

const nameOf = (node: NodeAgentStateItem) => node.name || node.node_id;

export function NodeRetireDialog({
  node,
  chains,
  offline,
  allowed,
  pending,
  error,
  onConfirm,
  onClose,
}: {
  node: NodeAgentStateItem;
  /** 经过这台机器的线路名称，按页面上的顺序。 */
  chains: readonly string[];
  /** 失联时长；在线时为 null。 */
  offline: ReactNode | null;
  allowed: boolean;
  pending: boolean;
  error: unknown;
  onConfirm: (action: NodeRetireAction) => void;
  onClose: () => void;
}) {
  const phase = node.lifecycle_phase;
  const live = phase === 'active';
  // 轮询更新状态不能升级用户意图；仅退役不再可用时禁用确认，移出须显式选择。
  const [mode, setMode] = useState<NodeRetireMode>(live ? 'retire' : 'remove');
  const [force, setForce] = useState(true);
  const removing = mode === 'remove';
  const showForce = live || phase === 'retiring';
  const action = nodeRetireAction(phase, mode, showForce ? force : true);
  const name = nameOf(node);
  const title = `${live ? '退役' : '移出'} ${name}`;
  const ip = node.public_ipv4 ?? node.public_ipv6;
  const since = phase === 'retiring' ? node.retired_at : (node.lifecycle_completed_at ?? node.retired_at);
  const confirmLabel = removing ? (live ? '退役且移出' : '移出') : '退役';

  return (
    <DialogLayer label={title} onClose={onClose}>
      <div className="dialog-surface node-retire-dialog" data-mode={mode}>
        <header className="node-retire-head">
          <div className="node-retire-ident">
            <h2>{title}</h2>
            <span className="node-retire-sub">
              {live ? (
                <>
                  {ip && <span className="mono">{ip}</span>}
                  {offline && (
                    <>
                      {ip && ' · '}
                      <span className="node-retire-offline">失联 {offline}</span>
                    </>
                  )}
                </>
              ) : (
                <>
                  {PHASE_LABEL[phase]}
                  {since && (
                    <>
                      {' · '}
                      <Ago at={since} />
                    </>
                  )}
                </>
              )}
            </span>
          </div>
          <DialogClose className="node-retire-close" aria-label="关闭">
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="node-retire-body">
          <div className="node-retire-options" role="radiogroup" aria-label="退役方式">
            <label className="node-retire-option" data-checked={mode === 'retire'} data-disabled={!live || undefined}>
              <input
                type="radio"
                name={`node-retire-mode-${node.node_id}`}
                value="retire"
                checked={mode === 'retire'}
                disabled={!live}
                onChange={() => setMode('retire')}
              />
              <span className="node-retire-dot" aria-hidden="true" />
              <span className="node-retire-option-title">
                <b>仅退役</b>
                {!live && <span className="node-retire-state">{phase === 'retiring' ? '进行中' : '已完成'}</span>}
              </span>
              <small>停止服务，保留机器记录和历史；之后可以恢复。</small>
            </label>
            <label className="node-retire-option danger" data-checked={removing}>
              <input
                type="radio"
                name={`node-retire-mode-${node.node_id}`}
                value="remove"
                checked={removing}
                onChange={() => setMode('remove')}
              />
              <span className="node-retire-dot" aria-hidden="true" />
              <span className="node-retire-option-title">
                <b>{live ? '退役且移出' : '移出'}</b>
              </span>
              <small>{live ? '停止服务后' : ''}从机器列表移出，并删除经过它的线路。不能撤销。</small>
            </label>
          </div>
          <div className="node-retire-routes" data-removing={(removing && chains.length > 0) || undefined}>
            <span>
              {chains.length > 0
                ? `将${removing ? '删除' : '暂停'}经过它的 ${chains.length} 条线路：`
                : '没有线路经过它。'}
            </span>
            {chains.map((chain, index) => (
              <span className="node-retire-route" key={`${chain}-${index}`}>
                {chain}
              </span>
            ))}
          </div>
          {showForce && (
            <div className="node-retire-force">
              <div className="node-retire-force-row">
                <b id={`node-retire-force-${node.node_id}`}>强制</b>
                <TwoWaySwitch
                  labelledBy={`node-retire-force-${node.node_id}`}
                  value={force}
                  onChange={setForce}
                  on="开启"
                  off="关闭"
                />
              </div>
              <p>
                {force
                  ? `不等这台机器回报，立即完成${removing ? '并移出' : ''}。其他机器马上改为不经过它；这台机器上的 Xray 和 WireGuard 不保证关闭，机器仍在运行时请到服务商处关机或重装。`
                  : `先下发停用，等这台机器回报已关闭 Xray 和 WireGuard 后才完成${removing ? '，随后自动移出' : ''}。`}
              </p>
              {offline && !force && (
                <p className="node-retire-warn">
                  <Icon of="warn" size={13} />
                  <span>这台已失联 {offline}，关闭强制会一直等到它重新上线。</span>
                </p>
              )}
            </div>
          )}
          {error ? <ErrorBox error={error} /> : null}
        </div>
        <footer className="node-retire-foot">
          {/* 移出不能撤销，打开时焦点落在「取消」，回车不会直接移出；仅退役可以恢复，焦点落在确认上。 */}
          <DialogClose className="btn" autoFocus={removing}>
            取消
          </DialogClose>
          <button
            type="button"
            className={removing ? 'btn danger solid' : 'btn danger'}
            autoFocus={!removing}
            disabled={!allowed || pending || action === null}
            onClick={() => {
              if (action) onConfirm(action);
            }}
          >
            <Icon of={removing ? 'trash' : 'power'} size={14} />
            {pending ? '提交中…' : confirmLabel}
          </button>
        </footer>
      </div>
    </DialogLayer>
  );
}

/** 两格都有文字的开关，沿用观测页地址族开关的外观与键盘行为：方向键切换，选中的一格可聚焦。 */
function TwoWaySwitch({
  labelledBy,
  value,
  onChange,
  on,
  off,
}: {
  labelledBy: string;
  value: boolean;
  onChange: (next: boolean) => void;
  on: string;
  off: string;
}) {
  const move = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
    event.preventDefault();
    const next = !value;
    onChange(next);
    const group = event.currentTarget.parentElement;
    group?.querySelector<HTMLButtonElement>(`[data-value="${next}"]`)?.focus();
  };
  return (
    <div
      className="ping-family-switch two-way-switch"
      role="radiogroup"
      aria-labelledby={labelledBy}
      data-thumb={value ? 'start' : 'end'}
    >
      <span className="ping-family-switch-thumb" aria-hidden="true" />
      {[true, false].map(option => (
        <button
          key={String(option)}
          type="button"
          role="radio"
          aria-checked={value === option}
          tabIndex={value === option ? 0 : -1}
          data-value={String(option)}
          onClick={() => onChange(option)}
          onKeyDown={move}
        >
          {option ? on : off}
        </button>
      ))}
    </div>
  );
}

/** 横幅里的操作；访客看不到写操作，传 null。 */
export interface NodeLifecycleBannerActions {
  allowed: boolean;
  pending: boolean;
  onViewDeployment: () => void;
  onRepair: () => void;
  onRestore: () => void;
  onForceComplete: () => void;
  onRemove: () => void;
}

export function NodeLifecycleBanner({
  node,
  chains,
  offline,
  teardownNeedsRepair,
  removalError,
  actions,
}: {
  node: NodeAgentStateItem;
  chains: readonly string[];
  offline: ReactNode | null;
  /** 退役中却没有可继续执行的停用发布（已取消、失败或缺失）。 */
  teardownNeedsRepair: boolean;
  /** 本页刚才的移出请求失败（例如强制退役成功、随后的移出失败）。 */
  removalError: unknown;
  actions: NodeLifecycleBannerActions | null;
}) {
  const phase = node.lifecycle_phase;
  if (phase === 'active') return null;
  const queued = nodeRemovalQueued(node);
  const disabled = !actions?.allowed || actions.pending;

  if (phase === 'retiring') {
    const stuck = teardownNeedsRepair || offline !== null;
    const steps = ['创建停用发布', '等待机器回报', '停用完成', ...(queued ? ['移出'] : [])];
    const current = teardownNeedsRepair ? 0 : 1;
    return (
      <section className={`node-life${stuck ? ' bad' : ''}`} aria-label="退役状态">
        <div className="node-life-top">
          <b>退役中</b>
          <span className="node-life-meta">
            {node.lifecycle_deployment_id ? `停用发布 #${node.lifecycle_deployment_id}` : '没有停用发布'}
            {node.retired_at && (
              <>
                {' · 已等待 '}
                <Ago at={node.retired_at} withPastSuffix={false} />
              </>
            )}
          </span>
          {queued && <span className="node-life-tag">完成后移出</span>}
        </div>
        <ol className="node-life-steps">
          {steps.map((label, index) => (
            <li
              key={label}
              data-state={index < current ? 'done' : index === current ? (stuck ? 'stuck' : 'now') : undefined}
              aria-current={index === current ? 'step' : undefined}
            >
              <i aria-hidden="true" />
              {label}
            </li>
          ))}
        </ol>
        <div className="node-life-foot">
          <span className={`node-life-note${stuck ? ' bad' : ''}`}>
            {teardownNeedsRepair ? (
              '当前没有可继续执行的停用发布；重建后这台机器才会收到停用。'
            ) : offline ? (
              <>这台已失联 {offline}，回报不会到来，可以强制完成。</>
            ) : (
              '机器关闭 Xray 和 WireGuard 并回报后自动完成，不必停留在此页。'
            )}
          </span>
          {actions && (
            <div className="node-life-actions">
              {teardownNeedsRepair ? (
                <button type="button" className="btn sm" disabled={disabled} onClick={actions.onRepair}>
                  <Icon of="deploy" size={13} />
                  重建停用发布
                </button>
              ) : (
                node.lifecycle_deployment_id && (
                  <button type="button" className="btn sm node-life-link" onClick={actions.onViewDeployment}>
                    <Icon of="deploy" size={13} />
                    查看发布 #{node.lifecycle_deployment_id}
                  </button>
                )
              )}
              <button
                type="button"
                className="btn sm"
                disabled={disabled}
                title="创建恢复发布；恢复后需要重新签发 token"
                onClick={actions.onRestore}
              >
                <Icon of="undo" size={13} />
                恢复
              </button>
              <button
                type="button"
                className={`btn sm${stuck ? ' node-life-urgent' : ''}`}
                disabled={disabled}
                title={
                  queued
                    ? '不等这台机器回报，立即完成退役，随后自动移出'
                    : '不等这台机器回报，立即完成退役；机器上的服务不保证关闭'
                }
                onClick={actions.onForceComplete}
              >
                <Icon of="bolt" size={13} />
                强制完成
              </button>
            </div>
          )}
        </div>
      </section>
    );
  }

  const forced = phase === 'abandoned';
  const since = node.lifecycle_completed_at ?? node.retired_at;
  const serverFailure = node.removal_status === 'failed';
  const retrying = queued && !!node.removal_last_error;
  const canRemove = serverFailure || !!removalError;
  return (
    <section className={`node-life ${forced || canRemove ? 'bad' : 'idle'}`} aria-label="退役状态">
      <div className="node-life-top">
        <b>{forced ? '强制退役' : node.lifecycle_last_error ? '待清理' : '已退役'}</b>
        <span className="node-life-meta">
          {since && (
            <>
              <Ago at={since} />
              {' · '}
            </>
          )}
          {forced ? '未确认这台机器已关闭服务' : '机器已回报停用'}
        </span>
        {queued && <span className="node-life-tag">正在移出</span>}
      </div>
      {node.lifecycle_last_error && <p className="node-life-error">外部资源待清理：{node.lifecycle_last_error}</p>}
      {serverFailure && (
        <p className="node-life-error" role="alert">
          自动移出失败{node.removal_last_error ? `：${node.removal_last_error}` : ''}。可以手动移出。
        </p>
      )}
      {retrying && <p className="node-life-error">移出重试中：{node.removal_last_error}</p>}
      {!serverFailure && removalError ? <ErrorBox error={removalError} /> : null}
      <div className="node-life-foot">
        <span className="node-life-note">
          {forced && '机器如果仍在运行，请到服务商处关机或重装。'}
          {chains.length > 0 ? `经过它的 ${chains.length} 条线路已暂停：${chains.join('、')}。` : '没有线路经过它。'}
        </span>
        {actions && (
          <div className="node-life-actions">
            {canRemove && (
              <button type="button" className="btn sm danger" disabled={disabled} onClick={actions.onRemove}>
                <Icon of="trash" size={13} />
                移出
              </button>
            )}
            <button
              type="button"
              className="btn sm"
              disabled={disabled}
              title={
                queued
                  ? '撤销已安排的移出并创建恢复发布；恢复后需要重新签发 token'
                  : '创建恢复发布；恢复后需要重新签发 token'
              }
              onClick={actions.onRestore}
            >
              <Icon of="undo" size={13} />
              恢复
            </button>
          </div>
        )}
      </div>
    </section>
  );
}

/* 移出成功后详情页已不存在，确认改在机器列表里以浮层提示呈现。提示放在模块级：详情页卸载、
   列表挂载之间没有共同的组件状态；StrictMode 的重复挂载也不会把它清掉。 */
let removalNotice: { id: number; text: string } | null = null;
let removalNoticeSeq = 0;
const removalNoticeListeners = new Set<() => void>();
const emitRemovalNotice = () => removalNoticeListeners.forEach(listener => listener());
const subscribeRemovalNotice = (listener: () => void) => {
  removalNoticeListeners.add(listener);
  return () => removalNoticeListeners.delete(listener);
};

export function announceNodeRemoval(text: string) {
  removalNotice = { id: ++removalNoticeSeq, text };
  emitRemovalNotice();
}

function dismissNodeRemovalNotice(id: number) {
  if (removalNotice?.id !== id) return;
  removalNotice = null;
  emitRemovalNotice();
}

const NOTICE_MS = 6_500;

/** 机器列表左下角的一次性确认；几秒后自动消失，不挤动下面的卡片。 */
export function NodeRemovalToast() {
  const notice = useSyncExternalStore(
    subscribeRemovalNotice,
    () => removalNotice,
    () => null,
  );
  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => dismissNodeRemovalNotice(notice.id), NOTICE_MS);
    return () => window.clearTimeout(timer);
  }, [notice]);
  if (!notice) return null;
  return (
    <div className="node-removal-toast" role="status">
      <Icon of="check" size={15} />
      <span>{notice.text}</span>
      <button type="button" aria-label="关闭提示" onClick={() => dismissNodeRemovalNotice(notice.id)}>
        <Icon of="close" size={12} />
      </button>
    </div>
  );
}
