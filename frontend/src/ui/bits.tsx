import { useEffect, useState, type ReactNode } from 'react';
import { useNow } from './clock';

/* 发布状态标签的配色：对应状态机，failed-dirty 使用最高对比度的红色 */
const STATUS_CLASS: Record<string, string> = {
  succeeded: 'st-succeeded',
  running: 'st-running',
  converging: 'st-running',
  dispatched: 'st-dispatched',
  planned: 'st-pending',
  pending: 'st-pending',
  skipped: 'st-skipped',
  canceled: 'st-canceled',
  halted: 'st-halted',
  failed: 'st-halted',
  'failed-recovered': 'st-halted',
  'failed-dirty': 'st-failed-dirty',
};

// 状态标签的文案。协议中的取值是英文状态机名（succeeded / failed-dirty 等），界面显示中文——
// 本页其他内容均为中文，中间出现一串小写英文会被理解为内部代码。
//
// 这是全站唯一一份译名。发布列表此前另有一份，导致同一个 `running` 在列表中
// 显示为「推送中」而在详情的波次表中显示为其他文案——两个页面对应同一条发布，
// 会被理解为两种状态。修改文案时修改此处。
//
// 原始取值写入 title：查询日志、核对服务端返回、与 brocade-store 中的状态机对应时
// 需要该英文字符串。两者同时保留。
export const STATUS_TEXT: Record<string, string> = {
  succeeded: '成功',
  running: '推送中',
  converging: '收敛中',
  dispatched: '已下发',
  planned: '待推',
  pending: '待推',
  skipped: '跳过',
  canceled: '已取消',
  halted: '已中止',
  failed: '失败',
  'failed-recovered': '失败已回滚',
  'failed-dirty': '失败未回滚',
};

export function Status({ value }: { value: string }) {
  return (
    <span className={`st ${STATUS_CLASS[value] ?? ''}`} title={value}>
      {STATUS_TEXT[value] ?? value}
    </span>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return (
    <p className="note" style={{ padding: '10px 2px' }}>
      {children}
    </p>
  );
}

// 加载中状态：骨架条。不用「读取中」文字——文字每次查询都会闪现一次，反复出现的
// 一行字容易被当作页面出错；骨架条只占位、不表态，与页面内容同属灰色块。
//
// `sheeted` 一档用于自行管理内容区的页面（机器、用户走外壳的 bare 分支）：
// 它们的提前 return 发生在渲染 .fg-sheet 之前，不额外包一层时，骨架条会直接显示在
// 台面网格上，而其他页面此时显示的是内容区已渲染、其中显示骨架。
// 同一状态在两类页面上呈现不同，会被理解为其中一个出现异常。
export function Loading({ sheeted = false }: { sheeted?: boolean }) {
  const body = (
    <div className="skel" aria-hidden="true">
      <i style={{ width: '62%' }} />
      <i style={{ width: '88%' }} />
      <i style={{ width: '74%' }} />
    </div>
  );
  return sheeted ? <div className="fg-sheet">{body}</div> : body;
}

export function ErrorBox({ error }: { error: unknown }) {
  const message = error instanceof Error ? error.message : String(error);
  return <div className="callout err">{message}</div>;
}

// 危险操作的确认框。requireWord 非空时必须先输入该词才能确认——
// 回滚这类误操作后难以恢复的操作，仅二次确认不足，需要确认已阅读影响说明后才允许执行。
export function Confirm({
  title,
  body,
  confirmLabel,
  danger = true,
  requireWord,
  onConfirm,
  onCancel,
}: {
  title: string;
  body: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  requireWord?: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const [word, setWord] = useState('');
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onCancel();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onCancel]);
  const ok = !requireWord || word === requireWord;
  return (
    <div className="confirm-mask" onClick={onCancel}>
      <div className="confirm-card" onClick={e => e.stopPropagation()} role="dialog" aria-modal="true">
        <div className="confirm-title">{title}</div>
        <div className="confirm-body">{body}</div>
        {requireWord && (
          <input
            className="f confirm-word"
            autoFocus
            placeholder={`输入「${requireWord}」确认`}
            value={word}
            onChange={e => setWord(e.target.value)}
          />
        )}
        <div className="confirm-actions">
          <button className="btn" onClick={onCancel}>
            取消
          </button>
          <button className={`btn ${danger ? 'danger' : 'primary'}`} disabled={!ok} onClick={onConfirm}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

/* 时间戳统一显示为相对时间——在拉取模型下，机器距上次拉取的时长是主要信息。绝对时刻写入 title 备查。 */
export function Ago({ at }: { at: string | null }) {
  // useNow 需要在提前 return 之前调用（hook 不能有条件地跳过）。该值因此每秒自动更新，
  // 不需要依赖其他原因触发重渲染——计时器即用于该场景（ui/clock.ts）。
  const now = useNow();
  if (!at) return <span className="dim">—</span>;
  const t = Date.parse(at.endsWith('Z') || at.includes('+') ? at : `${at}Z`);
  if (Number.isNaN(t)) return <span className="mono dim">{at}</span>;
  const s = Math.max(0, Math.round((now - t) / 1000));
  const text =
    s < 60
      ? `${s} 秒前`
      : s < 3600
        ? `${Math.floor(s / 60)} 分钟前`
        : s < 86400
          ? `${Math.floor(s / 3600)} 小时前`
          : `${Math.floor(s / 86400)} 天前`;
  return (
    <span className="mono" title={at}>
      {text}
    </span>
  );
}

/* 二选一开关：两格，选中的一格反白。直角外框，与按钮同高同边框，两个状态都有文字标签。

   定义在 bits 中而非某个面板内：机器详情（NAT、是否加入 overlay、出网）和链详情（投影）
   都在使用。此前它是 panes/nodes.tsx 中的私有组件，第二处使用时只能复制一份——
   复制出的实现最终会在某次修改中与原实现产生差异。 */
export function SegSwitch({
  checked,
  disabled,
  onChange,
  off,
  on,
}: {
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
  /* 两格各自的文案。off 位于左侧（false），on 位于右侧（true） */
  off: string;
  on: string;
}) {
  return (
    <span className="segsw" role="group">
      {([false, true] as const).map(v => (
        <button
          key={String(v)}
          type="button"
          aria-pressed={checked === v}
          disabled={disabled}
          onClick={() => checked !== v && onChange(v)}
        >
          {v ? on : off}
        </button>
      ))}
    </span>
  );
}
