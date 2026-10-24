// 用户详情「在线设备」里的来源封禁部件：行内禁行钮与确认、手动添加、整份名单在各机器的执行情况。
// 名单的读取与保存见 source-access.tsx 的 useSourceAccess；「已封禁」一组的行与时间线共用列和工具函数，
// 写在 users.tsx 的在线设备卡里。
import { memo, useRef, useState, type FormEvent, type ReactNode, type RefObject } from 'react';
import type { SourceAccessNode } from '../api';
import { ErrorBox } from '../ui/bits';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { Icon } from '../ui/icons';
import { useNodeNames } from '../ui/node-name';
import { SOURCE_ACCESS_PHASE_TEXT } from './source-access';
import './user-source-access.css';

/**
 * 输入地址的规范写法，与 Console 的 normalize_ip 结果一致：IPv6 按 RFC 5952 压缩为小写，
 * IPv4-mapped IPv6 写成 IPv4。只用来对照已知来源和名单；认不出的输入照原样提交，由 Console 给出校验结论。
 */
export function canonicalSourceIp(raw: string): string | null {
  const value = raw.trim();
  if (!value || /[\s/%[\]]/.test(value)) return null;
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(value);
  if (v4) {
    const parts = v4.slice(1);
    if (parts.some(part => (part.length > 1 && part.startsWith('0')) || Number(part) > 255)) return null;
    return parts.map(Number).join('.');
  }
  if (!value.includes(':')) return null;
  let host: string;
  try {
    host = new URL(`http://[${value}]/`).hostname.slice(1, -1);
  } catch {
    return null;
  }
  const mapped = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(host);
  if (!mapped) return host;
  const high = parseInt(mapped[1], 16);
  const low = parseInt(mapped[2], 16);
  return [high >> 8, high & 255, low >> 8, low & 255].join('.');
}

type RolloutTone = 'ok' | 'gold' | 'err';

/** 一份名单一个版本：各机器回执的是整份名单，不是单个地址，因此执行情况写在组标题上。 */
export function sourceAccessRollout(nodes: SourceAccessNode[]): { tone: RolloutTone; text: string } | null {
  if (nodes.length === 0) return null;
  const count = (phase: SourceAccessNode['phase']) => nodes.filter(node => node.phase === phase).length;
  const parts = [
    count('error') ? `${count('error')} 台异常` : '',
    count('unsupported') ? `${count('unsupported')} 台待升级` : '',
    count('pending') ? `同步中 ${count('applied')}/${nodes.length}` : '',
  ].filter(Boolean);
  if (parts.length === 0) return { tone: 'ok', text: '已生效' };
  return { tone: count('error') ? 'err' : 'gold', text: parts.join(' · ') };
}

const PHASE_TONE: Record<SourceAccessNode['phase'], RolloutTone> = {
  applied: 'ok',
  pending: 'gold',
  unsupported: 'gold',
  error: 'err',
};

/** 组标题右侧的执行情况；点开逐台列出机器、状态与异常原因。 */
export function SourceAccessRollout({ nodes }: { nodes: SourceAccessNode[] }) {
  const nameOf = useNodeNames();
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const rollout = sourceAccessRollout(nodes);
  if (!rollout)
    return (
      <span className="source-ban-rollout idle" title="该用户没有接入授权；授权后由相关机器执行">
        无相关机器
      </span>
    );
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={`source-ban-rollout ${rollout.tone}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        <span className="source-ban-dot" aria-hidden="true" />
        {rollout.text}
      </button>
      {open && (
        <DialogLayer mode="popover" anchor={anchor} align="end" label="各机器执行情况" onClose={() => setOpen(false)}>
          <ul className="dialog-surface source-ban-nodes">
            {nodes.map(node => (
              <li key={node.node_id}>
                <span className="source-ban-node-name" title={node.node_id}>
                  {nameOf(node.node_id)}
                </span>
                <span className={`source-ban-node-phase ${PHASE_TONE[node.phase]}`}>
                  {node.phase === 'unsupported' ? '需升级 Agent 与 Xray' : SOURCE_ACCESS_PHASE_TEXT[node.phase]}
                </span>
                {node.error && <small>{node.error}</small>}
              </li>
            ))}
          </ul>
        </DialogLayer>
      )}
    </>
  );
}

/** 点行内禁行钮后贴着它弹出的确认：写对象和后果。 */
function SourceBanConfirm({
  ip,
  online,
  anchor,
  onClose,
  onConfirm,
}: {
  ip: string;
  online: boolean;
  anchor: RefObject<HTMLElement | null>;
  onClose: () => void;
  onConfirm: () => Promise<unknown>;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  return (
    <DialogLayer mode="popover" anchor={anchor} label={`封禁 ${ip}`} onClose={onClose} canClose={() => !busy}>
      <div className="dialog-surface source-ban-confirm">
        <p className="source-ban-confirm-title">
          封禁 <code>{ip}</code>
        </p>
        <p>{online ? '断开该来源的会话，并拒绝再次接入。' : '拒绝该来源再次接入。'}</p>
        {!!error && <ErrorBox error={error} />}
        <footer>
          <DialogClose className="btn sm" disabled={busy}>
            取消
          </DialogClose>
          <button
            type="button"
            className="btn sm danger"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              setError(null);
              try {
                await onConfirm();
                // 成功后这一行移进「已封禁」组，按钮随行卸载；这里只负责仍挂着时收起。
                onClose();
              } catch (failure) {
                setError(failure);
                setBusy(false);
              }
            }}
          >
            {busy ? '封禁中…' : '封禁'}
          </button>
        </footer>
      </div>
    </DialogLayer>
  );
}

/** 时间线行里与复制钮并排的禁行钮，同样悬停或聚焦时出现、触屏常显。 */
export const SourceBanButton = memo(function SourceBanButton({
  ip,
  online,
  onBan,
}: {
  ip: string;
  online: boolean;
  onBan: (ip: string) => Promise<unknown>;
}) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className="user-fcopy source-ban-button"
        aria-label={`封禁 ${ip}`}
        title="封禁此来源"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        <Icon of="ban" size={16} />
      </button>
      {open && (
        <SourceBanConfirm
          ip={ip}
          online={online}
          anchor={anchor}
          onClose={() => setOpen(false)}
          onConfirm={() => onBan(ip)}
        />
      )}
    </>
  );
});

/**
 * 手动输入一个地址封禁。地址是已知来源时，下方写出它的地区和在线状态，不再另弹确认；
 * 已在名单中的不重复提交。
 */
export function SourceBanAdd({
  bans,
  describe,
  onSave,
  onDone,
}: {
  bans: readonly string[];
  /** 已知来源的说明；不认识的地址返回 null。 */
  describe: (ip: string) => { online: boolean; content: ReactNode } | null;
  onSave: (ip: string) => Promise<unknown>;
  onDone: () => void;
}) {
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const ip = canonicalSourceIp(value);
  const listed = !!ip && bans.includes(ip);
  const known = ip && !listed ? describe(ip) : null;
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const raw = value.trim();
    if (!raw || busy || listed) return;
    setBusy(true);
    setError(null);
    try {
      await onSave(ip ?? raw);
      setValue('');
      onDone();
    } catch (failure) {
      setError(failure);
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className="source-ban-add" aria-label="封禁其他来源" onSubmit={submit}>
      <span className="source-ban-add-row">
        <input
          className="f mono"
          value={value}
          placeholder="IPv4 / IPv6 地址"
          aria-label="来源 IP"
          autoComplete="off"
          spellCheck={false}
          autoFocus
          maxLength={45}
          disabled={busy}
          onChange={event => {
            setValue(event.target.value);
            setError(null);
          }}
          onKeyDown={event => {
            if (event.key !== 'Escape' || busy) return;
            event.preventDefault();
            onDone();
          }}
        />
        <button type="submit" className="btn danger" disabled={!value.trim() || busy || listed}>
          {busy ? '封禁中…' : '封禁'}
        </button>
        <button type="button" className="btn" disabled={busy} onClick={onDone}>
          取消
        </button>
      </span>
      {listed ? (
        <p className="source-ban-hint">已在名单中</p>
      ) : (
        known && <p className={`source-ban-hint${known.online ? ' gold' : ''}`}>{known.content}</p>
      )}
      {!!error && <ErrorBox error={error} />}
    </form>
  );
}
