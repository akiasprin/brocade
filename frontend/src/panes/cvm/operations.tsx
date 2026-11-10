import { useState } from 'react';
import { type CvmChange, type CvmHost, type CvmInstance } from '../../api';
import { ErrorBox } from '../../ui/bits';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Icon } from '../../ui/icons';

import { capacity, MIB } from './queries';

export function CommandDialog({
  instance,
  host,
  dialog,
  pending,
  error,
  onClose,
  onApply,
}: {
  instance: CvmInstance;
  host: CvmHost;
  dialog: {
    action: 'start' | 'stop' | 'restart' | 'reinstall' | 'configure' | 'detach';
    interfaceId?: string;
  };
  pending: boolean;
  error: Error | null;
  onClose: () => void;
  onApply: (change: CvmChange) => void;
}) {
  const [name, setName] = useState('');
  const [deleteData, setDeleteData] = useState(false);
  const [configName, setConfigName] = useState(instance.desired.spec.name);
  const [vcpus, setVcpus] = useState(String(instance.desired.spec.resources.vcpus));
  const [memoryMiB, setMemoryMiB] = useState(String(instance.desired.spec.resources.memory_bytes / MIB));
  const [imageId, setImageId] = useState(instance.desired.spec.image.id);
  const [publicKey, setPublicKey] = useState(
    instance.desired.spec.ssh_public_key.trim().split(/\s+/).slice(0, 2).join(' '),
  );
  const limits = host.capabilities.resource_limits;
  const [localError, setLocalError] = useState<string | null>(null);
  const title = {
    start: '开机',
    stop: '关机',
    restart: '重启实例',
    reinstall: '重装系统',
    configure: '修改配置',
    detach: '卸载网卡',
  }[dialog.action];
  const submit = () => {
    setLocalError(null);
    if (dialog.action === 'configure') {
      if (
        !configName.trim() ||
        !limits ||
        !Number.isInteger(Number(vcpus)) ||
        Number(vcpus) < limits.min_vcpus ||
        Number(vcpus) > limits.max_vcpus ||
        !Number.isInteger(Number(memoryMiB)) ||
        Number(memoryMiB) * MIB < limits.min_memory_bytes ||
        Number(memoryMiB) * MIB > limits.max_memory_bytes ||
        (Number(memoryMiB) * MIB) % limits.memory_step_bytes !== 0
      ) {
        setLocalError('请按机器支持的范围填写名称、CPU 和内存。');
        return;
      }
      onApply({
        action: 'configure',
        name: configName.trim(),
        vcpus: Number(vcpus),
        memory_bytes: Number(memoryMiB) * MIB,
      });
    } else if (dialog.action === 'reinstall') {
      if (name !== instance.desired.spec.name || !deleteData) {
        setLocalError('请填写完整实例名称，并确认清除系统盘数据。');
        return;
      }
      if (!publicKey.trim()) {
        setLocalError('请填写 SSH 公钥。');
        return;
      }
      onApply({ action: 'reinstall', image_id: imageId, ssh_public_key: publicKey.trim() });
    } else if (dialog.action === 'detach' && dialog.interfaceId)
      onApply({ action: 'detach', interface_id: dialog.interfaceId });
    else if (dialog.action === 'start' || dialog.action === 'stop' || dialog.action === 'restart')
      onApply({ action: dialog.action });
  };
  return (
    <DialogLayer label={title} onClose={onClose} canClose={() => !pending}>
      <div className="dialog-surface tunnel-dialog cvm-dialog">
        <header>
          <div>
            <b>{title}</b>
            <p className="note">{instance.desired.spec.name}</p>
          </div>
          <DialogClose className="btn icon" aria-label="关闭" title="关闭" disabled={pending}>
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="tunnel-dialog-body cvm-dialog-body">
          {dialog.action === 'configure' ? (
            <>
              <label className="cvm-field">
                实例名称
                <input
                  className="f"
                  value={configName}
                  onChange={e => setConfigName(e.target.value)}
                  disabled={pending}
                />
              </label>
              <div className="cvm-form-grid">
                <label className="cvm-field">
                  CPU（核）
                  <input
                    className="f"
                    type="number"
                    min={limits?.min_vcpus}
                    max={limits?.max_vcpus}
                    step={1}
                    value={vcpus}
                    onChange={e => setVcpus(e.target.value)}
                    disabled={pending}
                  />
                </label>
                <label className="cvm-field">
                  内存（MiB）
                  <input
                    className="f"
                    type="number"
                    min={(limits?.min_memory_bytes ?? 0) / MIB}
                    max={(limits?.max_memory_bytes ?? 0) / MIB}
                    step={(limits?.memory_step_bytes ?? MIB) / MIB}
                    value={memoryMiB}
                    onChange={e => setMemoryMiB(e.target.value)}
                    disabled={pending}
                  />
                </label>
              </div>
              <p className="note">
                保留原系统盘和网卡，保存后可开机。系统盘容量为{' '}
                {capacity(instance.desired.spec.resources.root_disk_bytes)}。
              </p>
            </>
          ) : dialog.action === 'reinstall' ? (
            <>
              <p>重装会清除系统盘中的全部数据，并重新生成系统访问身份。CPU、内存和已挂载网卡保留。</p>
              <label className="cvm-field">
                系统镜像
                <select className="f" value={imageId} onChange={e => setImageId(e.target.value)} disabled={pending}>
                  {host.capabilities.images.map(image => (
                    <option key={image.id} value={image.id}>
                      {image.name} {image.version}
                    </option>
                  ))}
                </select>
              </label>
              <label className="cvm-field">
                SSH 公钥
                <textarea
                  className="f mono"
                  value={publicKey}
                  onChange={e => setPublicKey(e.target.value)}
                  disabled={pending}
                />
              </label>
              <label className="cvm-field">
                输入实例名称以确认
                <input className="f" value={name} onChange={e => setName(e.target.value)} disabled={pending} />
              </label>
              <label className="cvm-check">
                <input
                  type="checkbox"
                  checked={deleteData}
                  onChange={e => setDeleteData(e.target.checked)}
                  disabled={pending}
                />
                确认清除系统盘数据
              </label>
            </>
          ) : (
            <p>
              {dialog.action === 'restart'
                ? '重启会短暂中断连接，系统盘数据和网卡配置保留。'
                : dialog.action === 'stop'
                  ? '停止会中断实例连接，系统盘和挂载配置保留。'
                  : dialog.action === 'detach'
                    ? '卸载会停止该接口的业务投递。设备移除确认前仍保留地址分配。'
                    : '使用原系统盘和当前挂载配置启动，不创建新的实例身份。'}
            </p>
          )}
          {localError && (
            <p className="error" role="alert">
              {localError}
            </p>
          )}
          {error && <ErrorBox error={error} />}
        </div>
        <footer>
          <button className="btn" disabled={pending} onClick={onClose}>
            取消
          </button>
          <button className="btn primary" disabled={pending} onClick={submit}>
            {pending ? '提交中…' : `确认${title}`}
          </button>
        </footer>
      </div>
    </DialogLayer>
  );
}
