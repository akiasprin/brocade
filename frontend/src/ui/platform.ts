// 控制台以 http 部署在 IP 上时不属于安全上下文，crypto.randomUUID、navigator.clipboard
// 这类 API 不存在。localhost 属于安全上下文，因此本地开发正常而部署到公网后失效——
// randomUUID 会抛出异常导致按钮点击无响应，clipboard 因使用了 ?. 而静默失败。
// 此处的两个替代实现在两种上下文下都可用。

/** 用于幂等键的随机 id。getRandomValues 不受安全上下文限制。 */
export function randomKey(): string {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, b => b.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** 复制到剪贴板，返回是否成功。没有 navigator.clipboard 时回退到 execCommand。 */
export async function copyText(text: string): Promise<boolean> {
  if (typeof navigator.clipboard?.writeText === 'function') {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      /* 权限被拒时同样走下面的回退实现 */
    }
  }
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.style.position = 'fixed';
  area.style.top = '0';
  area.style.opacity = '0';
  document.body.appendChild(area);
  area.select();
  let copied = false;
  try {
    copied = document.execCommand('copy');
  } catch {
    // 初始值即为 false，catch 中无需重新赋值
  }
  area.remove();
  return copied;
}
