import { useId, useState } from 'react';
import type { UserTrafficAccountingMode } from './api';
import { SegmentedControl } from './ui/bits';
import { Icon } from './ui/icons';

export function UserTrafficAccountingSetting({
  mode,
  disabled,
  onChange,
}: {
  mode: UserTrafficAccountingMode;
  disabled: boolean;
  onChange: (mode: UserTrafficAccountingMode) => void;
}) {
  const [showDescription, setShowDescription] = useState(false);
  const descriptionId = useId();
  return (
    <>
      <div className="user-usage-accounting">
        <button
          type="button"
          className="user-usage-accounting-help"
          aria-label="计量口径说明"
          aria-expanded={showDescription}
          aria-controls={descriptionId}
          onClick={() => setShowDescription(shown => !shown)}
        >
          计量口径
          <Icon of="info" size={13} />
        </button>
        <SegmentedControl
          value={mode}
          options={[
            { value: 'single' as const, label: '单向' },
            { value: 'double' as const, label: '双向' },
          ]}
          disabled={disabled}
          ariaLabel="用户流量计量口径"
          onChange={onChange}
        />
      </div>
      <div id={descriptionId} className="dim user-usage-accounting-note" hidden={!showDescription}>
        <p>单向计量：按代理层有效流量计量，同一业务数据仅计一次。</p>
        <p>双向计量：合计服务器入站与出站流量，同一业务数据在接收和转发阶段各计一次。</p>
        <p>切换后将按新口径重算本期用量，可能立即暂停或恢复受额度控制的接入线路或转发入口。</p>
      </div>
    </>
  );
}
