export const REALITY_FINGERPRINT_OPTIONS = [
  ['chrome', 'Chrome'],
  ['firefox', 'Firefox'],
  ['safari', 'Safari'],
  ['ios', 'iOS'],
  ['android', 'Android'],
  ['edge', 'Edge'],
  ['360', '360'],
  ['qq', 'QQ'],
  ['random', 'Random'],
  ['randomized', 'Randomized'],
] as const;

export const TLS_FINGERPRINT_OPTIONS = [['none', '关闭（原生 TLS）'], ...REALITY_FINGERPRINT_OPTIONS] as const;

const REALITY_FINGERPRINTS = new Set<string>(REALITY_FINGERPRINT_OPTIONS.map(([value]) => value));

export function realityPublicKeyIsValid(value: string): boolean {
  const trimmed = value.trim();
  if (!/^[A-Za-z0-9_-]{43}$/.test(trimmed)) return false;
  try {
    const padded = `${trimmed.replace(/-/g, '+').replace(/_/g, '/')}${'='.repeat((4 - (trimmed.length % 4)) % 4)}`;
    return atob(padded).length === 32;
  } catch {
    return false;
  }
}

export function realityShortIdIsValid(value: string): boolean {
  return /^(?:[0-9a-fA-F]{2}){1,8}$/.test(value.trim());
}

export function realityFingerprintIsValid(value: string): boolean {
  return REALITY_FINGERPRINTS.has(value.trim().toLowerCase());
}

export function tlsFingerprintIsValid(value: string): boolean {
  return value.trim().toLowerCase() === 'none' || realityFingerprintIsValid(value);
}

export function realityServerNameIsValid(value: string): boolean {
  const trimmed = value.trim();
  return trimmed !== '' && !/[\s:*]/.test(trimmed);
}
