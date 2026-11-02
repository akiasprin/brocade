export type MachineQuotaUnit = 'GiB' | 'TiB';
export const MACHINE_QUOTA_UNITS: readonly MachineQuotaUnit[] = ['GiB', 'TiB'];
const QUOTA_BYTES: Record<MachineQuotaUnit, bigint> = { GiB: 1024n ** 3n, TiB: 1024n ** 4n };
const U64_MAX = 18_446_744_073_709_551_615n;

/** Render a byte quota in the selected input unit, rounded to two decimal places. */
export function machineQuotaValue(value: string | null, unit: MachineQuotaUnit): string {
  if (!value) return '';
  try {
    const bytes = BigInt(value);
    const factor = QUOTA_BYTES[unit];
    const hundredths = (bytes * 100n + factor / 2n) / factor;
    return hundredths % 100n === 0n
      ? (hundredths / 100n).toString()
      : `${hundredths / 100n}.${(hundredths % 100n).toString().padStart(2, '0').replace(/0+$/, '')}`;
  } catch {
    return '';
  }
}

/** Empty means unlimited; undefined means invalid. Round IEC fractions to the nearest byte. */
export function machineQuotaBytes(value: string, unit: MachineQuotaUnit): string | null | undefined {
  const trimmed = value.trim();
  if (trimmed === '') return null;
  if (!/^\d+(?:\.\d{1,2})?$/.test(trimmed)) return undefined;
  const [whole, fraction = ''] = trimmed.split('.');
  const hundredths = BigInt(whole) * 100n + BigInt(fraction.padEnd(2, '0'));
  if (hundredths <= 0n) return undefined;
  const bytes = (hundredths * QUOTA_BYTES[unit] + 50n) / 100n;
  return bytes <= U64_MAX ? bytes.toString() : undefined;
}

/** Recover the entered unit from bytes, keeping existing TiB quotas in TiB. */
export function machineQuotaInput(value: string | null): { amount: string; unit: MachineQuotaUnit } {
  const tib = machineQuotaValue(value, 'TiB');
  if (machineQuotaBytes(tib, 'TiB') === value) return { amount: tib, unit: 'TiB' };
  const gib = machineQuotaValue(value, 'GiB');
  return machineQuotaBytes(gib, 'GiB') === value ? { amount: gib, unit: 'GiB' } : { amount: tib, unit: 'TiB' };
}
