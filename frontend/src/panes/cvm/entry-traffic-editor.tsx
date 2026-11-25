import type { CvmEntryAddressOffer } from '../../api';
import { type EntryTrafficDraft, type EntryTransportDraft, reservedPortsText } from './entry-policy';
export function EntryTrafficEditor({
  value,
  onChange,
  offer,
}: {
  value: EntryTrafficDraft;
  onChange: (next: EntryTrafficDraft) => void;
  offer: CvmEntryAddressOffer;
}) {
  const shared = offer.kind === 'shared';
  const transport = (protocol: 'tcp' | 'udp') => {
    const item = value[protocol],
      name = protocol.toUpperCase();
    const patch = (next: Partial<EntryTransportDraft>) => onChange({ ...value, [protocol]: { ...item, ...next } });
    return (
      <fieldset className="cvm-form-fieldset" key={protocol}>
        <legend>
          <label>
            <input type="checkbox" checked={item.enabled} onChange={e => patch({ enabled: e.target.checked })} /> {name}
          </label>
        </legend>
        {item.enabled && (
          <div className="cvm-form-grid">
            <label>
              {name} 转发范围
              <select
                className="f"
                value={item.all ? 'all' : 'ranges'}
                onChange={e => patch({ all: e.target.value === 'all' })}
              >
                <option value="all">{shared ? '全部可分配端口' : '全部端口'}</option>
                <option value="ranges">指定端口与范围</option>
              </select>
            </label>
            {!item.all && (
              <label>
                {name} 入口端口
                <input
                  className="f mono"
                  value={item.ports}
                  maxLength={4096}
                  placeholder="80,443,8000-8100"
                  onChange={e => patch({ ports: e.target.value })}
                />
              </label>
            )}
            <label>
              {name} 端口映射（可选）
              <textarea
                className="f mono"
                rows={2}
                maxLength={8192}
                value={item.mappings}
                placeholder={'30022 → 22\n30000-30099 → 40000-40099'}
                onChange={e => patch({ mappings: e.target.value })}
              />
            </label>
          </div>
        )}
      </fieldset>
    );
  };
  return (
    <div className="cvm-entry-traffic-editor">
      {(['tcp', 'udp'] as const).map(transport)}
      {!shared && (
        <div className="cvm-form-grid">
          <label className="cvm-check">
            <input
              type="checkbox"
              checked={value.icmp}
              onChange={e => onChange({ ...value, icmp: e.target.checked })}
            />{' '}
            ICMP
          </label>
          <label className="cvm-check">
            <input
              type="checkbox"
              checked={value.other_ip}
              onChange={e => onChange({ ...value, other_ip: e.target.checked })}
            />{' '}
            其他 IP 协议
          </label>
        </div>
      )}
      <p className="note">
        未填写映射的端口保持原端口。保留在入口机器的端口：{reservedPortsText(offer.reserved_ports) || '无自定义保留'}。
      </p>
    </div>
  );
}
