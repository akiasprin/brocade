/** Remove the retired machine-card preference without touching other account or UI state. */
export function clearRetiredMachineOrder(): void {
  try {
    const storage = window.localStorage;
    for (let index = storage.length - 1; index >= 0; index -= 1) {
      const key = storage.key(index);
      if (key?.startsWith('brocade-console:node-card-order:') || key === 'brocade-mockup:machine-card-order-v1') {
        storage.removeItem(key);
      }
    }
  } catch {
    // A browser policy denying storage must not prevent access to the Console.
    console.warn('无法清理已退役的机器排序偏好：浏览器存储不可用');
  }
}
