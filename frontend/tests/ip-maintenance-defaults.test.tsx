import { describe, expect, it } from 'vitest';
import { maintenanceDefaults } from '../src/panes/ip-maintenance/defaults';
import type { MaintenanceRegion, NodeAgentStateItem } from '../src/api';
import { maintenancePolicy, maintenanceRegions } from './fixtures/ip-maintenance-data';

const region = (country: string, city: string): MaintenanceRegion => ({
  ...maintenanceRegions[0],
  country,
  city,
  id: `${country}/Default/${city}`,
});
const regions = [
  region('JP', 'Tokyo'),
  region('JP', 'Osaka'),
  region('US', 'Los_Angeles'),
  region('US', 'San_Jose'),
  region('HK', 'HongKong'),
  region('SG', 'Singapore'),
  region('UK', 'London'),
];
const machine = (values: Partial<NodeAgentStateItem>) =>
  ({
    name: '机器',
    public_ipv4: '1.1.1.1',
    ...values,
  }) as NodeAgentStateItem;
const defaults = (values: Partial<NodeAgentStateItem>) =>
  maintenanceDefaults(machine(values), regions, maintenancePolicy());

describe('first-use maintenance location', () => {
  it.each([
    ['HK', '香港 AKILE', 'HK/Default/HongKong'],
    ['SG', '新加坡 AWS', 'SG/Default/Singapore'],
    ['GB', '伦敦 AKKO', 'UK/Default/London'],
    ['US', '洛杉矶 DMIT', 'US/Default/Los_Angeles'],
    ['US', 'San Jose Oracle', 'US/Default/San_Jose'],
    ['JP', '东京 ORACLE', 'JP/Default/Tokyo'],
    ['JP', '大阪 IIJ', 'JP/Default/Osaka'],
  ])('follows the %s flag and city name %s', (country, name, id) => {
    expect(defaults({ public_ipv4_country: country, name }).policy.region).toBe(id);
  });
  it('keeps a manual flag correction ahead of an observed IP country', () => {
    expect(defaults({ public_ipv4_country_override: 'HK', public_ipv4_country: 'US' }).policy.region).toBe(
      'HK/Default/HongKong',
    );
  });
  it('uses the available IPv6 address and its matching country observation', () => {
    const result = defaults({
      public_ipv4: null,
      public_ipv6: '2606:4700::1111',
      observed_public_ipv6: {
        current_ip: '2606:4700::1111',
        country_code: 'SG',
      } as NodeAgentStateItem['observed_public_ipv6'],
    });
    expect(result.policy.family).toBe('v6');
    expect(result.policy.region).toBe('SG/Default/Singapore');
    expect(result.address).toBe('2606:4700::1111');
  });
  it('does not classify a new configured address using an old observed exit', () => {
    const result = defaults({
      observed_public_ipv4: {
        current_ip: '8.8.8.8',
        country_code: 'HK',
      } as NodeAgentStateItem['observed_public_ipv4'],
    });
    expect(result.country).toBeNull();
    expect(result.policy.region).toBe('');
  });
  it('asks for a city when only a country is known and has multiple templates', () => {
    const result = defaults({ public_ipv4_country: 'US' });
    expect(result.policy.region).toBe('');
    expect(result.notice).toContain('请选择养护城市');
  });
  it.each([null, 'CN'])('does not substitute Japan for an unknown or unsupported country: %s', country => {
    const result = defaults({ public_ipv4_country: country });
    expect(result.policy.region).toBe('');
    expect(result.policy.enabled).toBe(false);
    expect(result.notice).toBeTruthy();
  });
});
