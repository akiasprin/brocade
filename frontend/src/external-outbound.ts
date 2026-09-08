export function serverNameAfterAddressChange(
  previousAddress: string,
  currentServerName: string,
  nextAddress: string,
): string {
  return !currentServerName.trim() || currentServerName === previousAddress ? nextAddress : currentServerName;
}

export function externalImportCanSave(entryMode: 'import' | 'manual', hasCurrentParse: boolean): boolean {
  return entryMode !== 'import' || hasCurrentParse;
}

// Keep the envelope aligned with the bundled Xray infra/conf/vless.go. Padding precedes keys.
export function vlessEncryptionIsValid(value: string): boolean {
  if (value === 'none') return true;
  const [suite, appearance, rtt, ...blocks] = value.split('.');
  if (
    suite !== 'mlkem768x25519plus' ||
    !['native', 'xorpub', 'random'].includes(appearance) ||
    !['0rtt', '1rtt'].includes(rtt)
  )
    return false;
  let keys = 0;
  for (const block of blocks) {
    if (block.length < 20) {
      if (keys > 0 || !/^\d+-\d+-\d+$/.test(block)) return false;
    } else {
      if (!/^[A-Za-z0-9_-]+$/.test(block)) return false;
      try {
        const bytes = atob(block.replace(/-/g, '+').replace(/_/g, '/'));
        if (bytes.length !== 32 && bytes.length !== 1184) return false;
        if (btoa(bytes).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '') !== block) return false;
      } catch {
        return false;
      }
      keys += 1;
    }
  }
  return keys > 0;
}
