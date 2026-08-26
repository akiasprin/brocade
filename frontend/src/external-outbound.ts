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
