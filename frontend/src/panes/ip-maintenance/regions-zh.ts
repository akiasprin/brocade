import type { MaintenanceRegion } from '../../api';

/* 上游地区目录的 name 中英混杂（「日本 (东京)」「United States - Dallas」），
   页面只用国家代码与城市字段拼中文名。落地时这张表随前端一起维护，或由 Console 返回 name_zh。 */
const CITY: Record<string, string> = {
  Dubai: '迪拜',
  Sydney: '悉尼',
  Brisbane: '布里斯班',
  Adelaide: '阿德莱德',
  Melbourne: '墨尔本',
  Perth: '珀斯',
  Dhaka: '达卡',
  Sofia: '索非亚',
  Montreal: '蒙特利尔',
  Toronto: '多伦多',
  Berlin: '柏林',
  Nuremberg: '纽伦堡',
  Frankfurt: '法兰克福',
  Aachen: '亚琛',
  Falkenstein: '法尔肯施泰因',
  Madrid: '马德里',
  Paris: '巴黎',
  HongKong: '香港',
  Jakarta: '雅加达',
  Delhi: '德里',
  Osaka: '大阪',
  Tokyo: '东京',
  PhnomPenh: '金边',
  Seoul: '首尔',
  Vientiane: '万象',
  Yangon: '仰光',
  Ulaanbaatar: '乌兰巴托',
  Macau: '澳门',
  Kuala_Lumpur: '吉隆坡',
  Lagos: '拉各斯',
  Amsterdam: '阿姆斯特丹',
  Kathmandu: '加德满都',
  Manila: '马尼拉',
  Riyadh: '利雅得',
  Singapore: '新加坡',
  Bangkok: '曼谷',
  Istanbul: '伊斯坦布尔',
  Taipei: '台北',
  Coventry: '考文垂',
  London: '伦敦',
  Phoenix: '凤凰城',
  Fremont: '弗里蒙特',
  Irvine: '尔湾',
  Los_Angeles: '洛杉矶',
  Ontario: '安大略',
  San_Jose: '圣何塞',
  Boulder: '博尔德',
  Miami: '迈阿密',
  Atlanta: '亚特兰大',
  Honolulu: '檀香山',
  Council_Bluffs: '康瑟尔布拉夫斯',
  Chicago: '芝加哥',
  Warrenville: '沃伦维尔',
  Kansas_City: '堪萨斯城',
  Charlotte: '夏洛特',
  Newark: '纽瓦克',
  Las_Vegas: '拉斯维加斯',
  Buffalo: '布法罗',
  New_York: '纽约',
  Columbus: '哥伦布',
  Bend: '本德',
  Dallas: '达拉斯',
  Houston: '休斯敦',
  Salt_Lake_City: '盐湖城',
  Ashburn: '阿什本',
  Seattle: '西雅图',
  Hanoi: '河内',
};
const STATE: Record<string, string> = {
  'US/AZ': '亚利桑那',
  'US/CA': '加利福尼亚',
  'US/CO': '科罗拉多',
  'US/FL': '佛罗里达',
  'US/GA': '佐治亚',
  'US/HI': '夏威夷',
  'US/IA': '艾奥瓦',
  'US/IL': '伊利诺伊',
  'US/KS': '堪萨斯',
  'US/NC': '北卡罗来纳',
  'US/NJ': '新泽西',
  'US/NV': '内华达',
  'US/NY': '纽约州',
  'US/OH': '俄亥俄',
  'US/OR': '俄勒冈',
  'US/TX': '得克萨斯',
  'US/UT': '犹他',
  'US/VA': '弗吉尼亚',
  'US/WA': '华盛顿州',
  'DE/BE': '柏林',
  'DE/BY': '巴伐利亚',
  'DE/NW': '北莱茵-威斯特法伦',
  'DE/SN': '萨克森',
  'AU/NSW': '新南威尔士',
  'AU/QLD': '昆士兰',
  'AU/SA': '南澳',
  'AU/VIC': '维多利亚',
  'AU/WA': '西澳',
};
const COUNTRY = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
/** 上游用 UK，旗帜与 Intl 都只认 GB。 */
export const isoOf = (country: string) => (country === 'UK' ? 'GB' : country);
export function countryName(country: string | null | undefined): string {
  if (!country) return '';
  try {
    return COUNTRY.of(isoOf(country)) ?? country;
  } catch {
    return country;
  }
}
export function cityName(region: Pick<MaintenanceRegion, 'city'>): string {
  return CITY[region.city] ?? region.city.replaceAll('_', ' ');
}
export function stateName(region: Pick<MaintenanceRegion, 'country' | 'state'>): string | null {
  return region.state === 'Default' ? null : (STATE[`${region.country}/${region.state}`] ?? null);
}
/** 「东京」够用时不重复国名；香港、澳门、新加坡这类城市与地区同名时只写一次。 */
export function placeName(region: MaintenanceRegion): { city: string; country: string; full: string } {
  const city = cityName(region);
  const country = countryName(region.country);
  return { city, country, full: city === country ? city : `${country} · ${city}` };
}
