/* 私有节点只在页面内存与用户下载的文件中存在，不参与普通规则的保存。 */
import type { PrivateProxy } from './private-import';
export { parsePrivateNode, parsePrivateNodes, LINK_PROTOCOLS, type PrivateProxy } from './private-import';

export type PrivateExitMode = 'select' | 'url-test';

export const CHAIN_GROUP_NAME = '链式代理';
export function chainProxyName(index: number, originalName: string): string {
  return `链式代理 ${String(index + 1).padStart(2, '0')} | ${originalName}`;
}

/** 输出给 Clash Verge Rev 的订阅扩展脚本；数据由 JSON 编码，节点内容不会作为代码执行。 */
export function privateChainScript(
  node: PrivateProxy | PrivateProxy[],
  front: { name: string; kind: 'line' | 'group'; selection: boolean },
  service: { id: string; yaml: string },
  selection: string,
  exitMode: PrivateExitMode = 'select',
): string {
  const nodes = Array.isArray(node) ? node : [node];
  if (!nodes.length) throw new Error('请先添加至少一个自建出口。');
  if (service.id === 'select' && front.selection)
    throw new Error('加入节点选择时，前置请使用自动或指定平台线路，避免循环代理。');
  const settings = JSON.stringify(
    {
      nodes: nodes.map((node, index) => ({ ...node, name: chainProxyName(index, node.name) })),
      front: front.name,
      frontKind: front.kind,
      service: service.yaml,
      mode: service.id === 'select' ? 'selection' : 'service',
      exitMode,
      selection,
      chain: CHAIN_GROUP_NAME,
    },
    null,
    2,
  );
  return `// Brocade · 私有链式代理 · Clash Verge Rev 订阅扩展脚本
// 本文件包含你的节点凭据，请保存在自己的设备上。
// 在目标 Brocade 订阅的「编辑扩展脚本」中粘贴全文，保存后重新激活订阅。
const settings = ${settings};

function main(config) {
  const proxies = config.proxies || [];
  const groups = config['proxy-groups'] || [];
  const rules = config.rules || [];
  const all = [...proxies, ...groups];
  const frontPool = settings.frontKind === 'group' ? groups : all;
  const front = frontPool.find(item => item.name === settings.front || item.name.startsWith(settings.front + ' · Brocade '));
  const service = groups.find(item => item.name === settings.service || item.name.startsWith(settings.service + ' · Brocade '));
  if (!front) throw new Error('找不到所选前置线路，请回到链式代理编辑器重新导出。');
  if (!service) throw new Error('找不到所选分流，请先启用并保存该分流。');
  const privateProxies = settings.nodes.map(node => ({ ...node, 'dialer-proxy': front.name }));
  const privateNames = privateProxies.map(node => node.name);
  if (all.some(item => privateNames.includes(item.name) || item.name === settings.chain))
    throw new Error('链式代理名称冲突，请仅保留一份此订阅的扩展脚本。');
  const chainGroup = {
    name: settings.chain, type: settings.exitMode, proxies: privateNames,
    ...(settings.exitMode === 'url-test' ? { url: 'https://www.gstatic.com/generate_204', interval: 300, lazy: false } : {}),
  };
  const insertChainGroup = current => {
    const index = current.findIndex(group => group.name === settings.selection || group.name.startsWith(settings.selection + ' · Brocade '));
    const ordered = [...current];
    ordered.splice(index < 0 ? 0 : index, 0, chainGroup);
    return ordered;
  };
  if (settings.mode === 'selection') {
    // 不能让私有链的前置再次经过其所在的「节点选择」，包括间接引用。
    const dependsOn = (name, target, visited = new Set()) => {
      if (name === target) return true;
      if (visited.has(name)) return false;
      visited.add(name);
      const item = all.find(entry => entry.name === name);
      const children = [...(item?.proxies || []), ...(item?.['dialer-proxy'] ? [item['dialer-proxy']] : [])];
      return children.some(child => dependsOn(child, target, visited));
    };
    if (dependsOn(front.name, service.name)) throw new Error('前置线路会回到节点选择，请改用独立的平台线路，避免循环代理。');
    return {
      ...config,
      proxies: [...proxies, ...privateProxies],
      'proxy-groups': insertChainGroup(groups.map(group => group === service ? { ...group, proxies: [settings.chain, ...(group.proxies || [])] } : group)),
    };
  }
  let matched = 0;
  const nextRules = rules.map(rule => {
    if (typeof rule !== 'string') return rule;
    const fields = rule.split(',');
    const target = fields.length - (fields[fields.length - 1] === 'no-resolve' ? 2 : 1);
    if (fields[target] === service.name) {
      fields[target] = settings.chain;
      matched++;
    }
    return fields.join(',');
  });
  if (!matched) throw new Error('所选分流没有启用的规则，请先保存规则并更新订阅。');
  return {
    ...config,
    proxies: [...proxies, ...privateProxies],
    'proxy-groups': insertChainGroup(groups),
    rules: nextRules,
  };
}
`;
}

export function downloadPrivateScript(script: string) {
  const url = URL.createObjectURL(new Blob([script], { type: 'text/javascript;charset=utf-8' }));
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = 'brocade-private-chain.js';
  anchor.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}
