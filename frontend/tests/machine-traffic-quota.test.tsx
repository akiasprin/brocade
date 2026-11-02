import { describe, expect, it } from 'vitest';
import { machineQuotaBytes, machineQuotaInput, machineQuotaValue } from '../src/ui/machine-traffic-quota';

describe('machine quota units', () => {
  it('uses binary GiB and TiB byte values without floating-point loss', () => {
    expect(machineQuotaBytes('100', 'GiB')).toBe('107374182400');
    expect(machineQuotaBytes(' 1.23 ', 'GiB')).toBe('1320702444');
    expect(machineQuotaBytes('0.01', 'GiB')).toBe('10737418');
    expect(machineQuotaBytes('10000000.01', 'GiB')).toBe('10737418250737418');
    expect(machineQuotaBytes('2.5', 'TiB')).toBe('2748779069440');
    expect(machineQuotaBytes('1.6', 'TiB')).toBe('1759218604442');
    expect(machineQuotaValue('1320702444', 'GiB')).toBe('1.23');
    expect(machineQuotaValue('2748779069440', 'TiB')).toBe('2.5');
  });

  it('restores the unit that can represent a saved quota and keeps TiB as the existing default', () => {
    expect(machineQuotaInput('107374182400')).toEqual({ amount: '100', unit: 'GiB' });
    expect(machineQuotaInput('10737418')).toEqual({ amount: '0.01', unit: 'GiB' });
    expect(machineQuotaInput('2748779069440')).toEqual({ amount: '2.5', unit: 'TiB' });
    expect(machineQuotaInput('1759218604442')).toEqual({ amount: '1.6', unit: 'TiB' });
    expect(machineQuotaInput(null)).toEqual({ amount: '', unit: 'TiB' });
  });

  it('supports unlimited quotas and rejects invalid precision and byte overflow in either unit', () => {
    for (const unit of ['GiB', 'TiB'] as const) {
      expect(machineQuotaBytes('  ', unit)).toBeNull();
      for (const value of ['0', '-1', '1.001', 'NaN', 'Infinity', '1e3', '18446744073709551616']) {
        expect(machineQuotaBytes(value, unit)).toBeUndefined();
      }
    }
    expect(machineQuotaBytes('17179869183.99', 'GiB')).toBe('18446744073698814198');
    expect(machineQuotaBytes('17179869184', 'GiB')).toBeUndefined();
    expect(machineQuotaBytes('16777216', 'TiB')).toBeUndefined();
  });
});
