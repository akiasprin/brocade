import { useState } from 'react';
import { type CvmChange, type CvmHost, type CvmInstance } from '../../api';
import { ErrorBox } from '../../ui/bits';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Icon } from '../../ui/icons';
import { useUnsavedChanges } from '../../ui/navigation-guard';

import { capacity, MIB } from './queries';
import { uncertainCvmResult, useCvmErrorNotice, useNicRequestId } from './network-common';
import { CompilePreview, useCompilePreview } from './compile-preview';

export function CommandDialog({
  instance,
  host,
  dialog,
  pending,
  error,
  onClose,
  onApply,
  onRetry,
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
  onApply: (change: CvmChange, inputSha256: string | undefined, generation: number) => Promise<unknown>;
  onRetry: () => Promise<unknown>;
}) {
  const [base, setBase] = useState(() => structuredClone(instance.desired));
  const [checking, setChecking] = useState(false);
  const busy = pending || checking;
  const uncertain = uncertainCvmResult(error);
  const inputLocked = busy || uncertain;
  const [configName, setConfigName] = useState(instance.desired.spec.name);
  const [vcpus, setVcpus] = useState(String(instance.desired.spec.resources.vcpus));
  const [memoryMiB, setMemoryMiB] = useState(String(instance.desired.spec.resources.memory_bytes / MIB));
  const [imageId, setImageId] = useState(instance.desired.spec.image?.id ?? '');
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
  const operationId = useNicRequestId();
  const previewChange: CvmChange | null =
    dialog.action === 'configure'
      ? { action: 'configure', name: configName.trim(), vcpus: Number(vcpus), memory_bytes: Number(memoryMiB) * MIB }
      : dialog.action === 'detach' && dialog.interfaceId
        ? { action: 'detach', interface_id: dialog.interfaceId }
        : null;
  const previewBody = previewChange ? { expected_generation: base.generation, ...previewChange } : null;
  const preview = useCompilePreview(
    instance.desired.id,
    previewBody ? { kind: 'command', request: { ...previewBody, request_id: operationId(previewBody) } } : null,
  );
  const requiresPreview = dialog.action === 'configure' || dialog.action === 'detach';
  const stale = base.generation !== instance.desired.generation;
  const dirty =
    dialog.action === 'configure'
      ? configName !== base.spec.name ||
        vcpus !== String(base.spec.resources.vcpus) ||
        memoryMiB !== String(base.spec.resources.memory_bytes / MIB)
      : dialog.action === 'reinstall' && imageId !== (base.spec.image?.id ?? '');
  useUnsavedChanges(dirty, title);
  const submit = async () => {
    setLocalError(null);
    if (uncertain) {
      setChecking(true);
      try {
        await onRetry();
      } catch {
        // Preserve the original request until its actual result is known.
      } finally {
        setChecking(false);
      }
      return;
    }
    let change: CvmChange;
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
      change = {
        action: 'configure',
        name: configName.trim(),
        vcpus: Number(vcpus),
        memory_bytes: Number(memoryMiB) * MIB,
      };
    } else if (dialog.action === 'reinstall') {
      change = { action: 'reinstall', image_id: imageId };
    } else if (dialog.action === 'detach' && dialog.interfaceId)
      change = { action: 'detach', interface_id: dialog.interfaceId };
    else if (dialog.action === 'start' || dialog.action === 'stop' || dialog.action === 'restart')
      change = { action: dialog.action };
    else return;
    setChecking(true);
    try {
      if (requiresPreview) await preview.save(inputSha256 => onApply(change, inputSha256, base.generation));
      else await onApply(change, undefined, base.generation);
    } catch {
      // Configuration checks and the parent mutation present errors and retain input.
    } finally {
      setChecking(false);
    }
  };
  const body = useCvmErrorNotice(error ?? localError ?? preview.query.error);
  return (
    <DialogLayer label={title} onClose={onClose} canClose={() => !busy}>
      <div className="dialog-surface tunnel-dialog cvm-dialog">
        <header>
          <div>
            <b>
              {title}
              {dirty && <span className="nd-tab-badge dot" aria-label="未保存的修改" title="未保存的修改" />}
            </b>
            <p className="note">{instance.desired.spec.name}</p>
          </div>
          <DialogClose className="btn icon" aria-label="关闭" title="关闭" disabled={busy}>
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="tunnel-dialog-body cvm-dialog-body" ref={body}>
          {uncertain && (
            <p className="cvm-notice" role="status">
              上次保存结果尚未确认。请重试原操作，或关闭后核查结果再修改配置。
            </p>
          )}
          {stale && !uncertain && (
            <div className="cvm-notice" role="alert">
              <p>容器配置已被其他操作修改。当前输入已保留，请载入最新配置后再编辑。</p>
              <button
                className="btn small"
                disabled={inputLocked}
                onClick={() => {
                  setBase(structuredClone(instance.desired));
                  setConfigName(instance.desired.spec.name);
                  setVcpus(String(instance.desired.spec.resources.vcpus));
                  setMemoryMiB(String(instance.desired.spec.resources.memory_bytes / MIB));
                  setImageId(instance.desired.spec.image?.id ?? '');
                  setLocalError(null);
                }}
              >
                载入最新配置并替换当前输入
              </button>
            </div>
          )}
          {dialog.action === 'configure' ? (
            <>
              <label className="cvm-field">
                实例名称
                <input
                  className="f"
                  value={configName}
                  onChange={e => setConfigName(e.target.value)}
                  disabled={inputLocked}
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
                    disabled={inputLocked}
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
                    disabled={inputLocked}
                  />
                </label>
              </div>
              <p className="note">
                修改配置会保留系统盘和网卡。发布生效后可以开机。系统盘容量为{' '}
                {capacity(instance.desired.spec.resources.root_disk_bytes)}。
              </p>
            </>
          ) : dialog.action === 'reinstall' ? (
            <>
              <p>重装会清除系统盘中的全部数据，并重新生成系统访问身份。CPU、内存和已挂载网卡保留。</p>
              <label className="cvm-field">
                系统镜像
                <select className="f" value={imageId} onChange={e => setImageId(e.target.value)} disabled={inputLocked}>
                  {host.capabilities.images.map(image => (
                    <option key={image.id} value={image.id}>
                      {image.name} {image.version}
                    </option>
                  ))}
                </select>
              </label>
              <p className="note">重装后自动生成新的高强度登录密码。</p>
            </>
          ) : (
            <p>
              {dialog.action === 'restart'
                ? '重启会短暂中断连接，系统盘数据和网卡配置保留。'
                : dialog.action === 'stop'
                  ? '停止会中断实例连接，系统盘和挂载配置保留。'
                  : dialog.action === 'detach'
                    ? '发布后移除这张网卡，经由它的网络连接将中断。移除完成后释放该网卡的地址。'
                    : '使用现有系统盘和网卡配置开机，保留系统盘数据。'}
            </p>
          )}
          <p className="note">保存后在「发布」中统一应用，发布前可在实例详情撤回。</p>
          {requiresPreview && <CompilePreview preview={preview} />}
          {localError && (
            <p className="error" role="alert">
              {localError}
            </p>
          )}
          {error && <ErrorBox error={error} />}
        </div>
        <footer>
          <button className="btn" disabled={busy} onClick={onClose}>
            取消
          </button>
          <button className="btn primary" disabled={busy || (stale && !uncertain)} onClick={() => void submit()}>
            {busy ? '正在保存…' : '保存变更'}
          </button>
        </footer>
      </div>
    </DialogLayer>
  );
}
