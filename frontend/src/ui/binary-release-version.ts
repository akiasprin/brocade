import type { BinaryComponent } from '../api';

/** Compact display only; APIs and the release ledger retain the complete source revision. */
export function binaryReleaseVersion(component: BinaryComponent, value: string) {
  const dirty = value.endsWith('-dirty');
  const revision = dirty ? value.slice(0, -'-dirty'.length) : value;
  if ((component === 'agent' || component === 'xray') && /^[0-9a-f]{40,64}$/.test(revision))
    return `${revision.slice(0, 12)}${dirty ? ' · dirty' : ''}`;
  const version = value.match(/(?:^|\b)(?:Xray\s+)?v?(\d+(?:\.\d+){1,3})(?:\b|$)/i)?.[1];
  return version ? `v${version}` : value;
}
