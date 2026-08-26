import { useState } from 'react';
import { useMutation } from '@tanstack/react-query';
import { changeMyPassword } from '../api';
import { useSession } from '../session';
import { ErrorBox } from '../ui/bits';

const MIN_LEN = 8;

// 自助修改密码。管理员代设的密码需要能够更换，因此该页面对所有角色开放——
// 服务端只要求 Read 权限，作用对象为当前登录的操作者。
export function PasswordPane() {
  const { who } = useSession();
  const [current, setCurrent] = useState('');
  const [next, setNext] = useState('');
  const [again, setAgain] = useState('');
  const [done, setDone] = useState<number | null>(null);

  const change = useMutation({
    mutationFn: () => changeMyPassword({ current_password: current, new_password: next }),
    onSuccess: r => {
      setDone(r.sessions_revoked);
      setCurrent('');
      setNext('');
      setAgain('');
    },
  });

  const mismatch = again.length > 0 && next !== again;
  const tooShort = next.length > 0 && next.length < MIN_LEN;
  // public readonly 账号可使用空的当前密码关闭自身的免密入口。
  const ready = next.length >= MIN_LEN && next === again;

  return (
    <>
      <p className="note">
        修改 <b className="mono">{who.operator_id}</b> 自己的登录密码。为避免旧凭据继续有效，现有 API token
        会一并撤销，需要时在操作者页重新签发。
      </p>
      <form
        onSubmit={e => {
          e.preventDefault();
          if (ready) {
            setDone(null);
            change.mutate();
          }
        }}
      >
        <div className="toolbar">
          <input
            className="f"
            style={{ width: 220 }}
            type="password"
            autoComplete="current-password"
            placeholder="当前密码（public 免密时留空）"
            value={current}
            onChange={e => setCurrent(e.target.value)}
          />
        </div>
        <div className="toolbar">
          <input
            className="f"
            style={{ width: 180 }}
            type="password"
            autoComplete="new-password"
            placeholder={`新密码（至少 ${MIN_LEN} 位）`}
            value={next}
            onChange={e => setNext(e.target.value)}
          />
          <input
            className="f"
            style={{ width: 180 }}
            type="password"
            autoComplete="new-password"
            placeholder="再输一遍"
            value={again}
            onChange={e => setAgain(e.target.value)}
          />
          <button className="btn primary" type="submit" disabled={!ready || change.isPending}>
            改密码
          </button>
        </div>
      </form>
      {tooShort && <p className="note err">新密码至少 {MIN_LEN} 位。</p>}
      {mismatch && <p className="note err">两次输入不一致。</p>}
      {change.error && <ErrorBox error={change.error} />}
      {done !== null && (
        <div className="callout">
          密码已改。当前这条登录留着，
          {done > 0 ? `别处的 ${done} 条登录已作废。` : '别处没有其它登录。'}
        </div>
      )}
    </>
  );
}
