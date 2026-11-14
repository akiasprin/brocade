import type { ClashRulesView } from '../../src/api';
// Synthetic HTTP responses for the production subscription editor.
export function clashRulesTransport(writable = true) {
  const modules = [
    ['ad', '广告拦截', '🛑 广告拦截', 'reject', 'category-ads-all'],
    ['private', '私有网络', '🏠 私有网络', 'direct', 'private', 'private-ip'],
    ['ai', 'AI 服务', '🤖 AI 服务', 'select', 'openai', 'anthropic', 'category-ai-chat-!cn'],
    ['cn', '国内服务', '🔒 国内服务', 'direct', 'geolocation-cn', 'cn-ip'],
    ['youtube', '油管视频', '📹 油管视频', 'select', 'youtube'],
    ['google', '谷歌服务', '🔍 谷歌服务', 'select', 'google', 'google-ip'],
    ['telegram', '电报消息', '📲 电报消息', 'select', 'telegram', 'telegram-ip'],
    ['github', '代码托管', '🐱 代码托管', 'select', 'github', 'gitlab', 'atlassian'],
    ['microsoft', '微软服务', 'Ⓜ️ 微软服务', 'select', 'microsoft', 'onedrive'],
    ['apple', '苹果服务', '🍏 苹果服务', 'select', 'apple', 'icloud'],
    ['global', '非中国', '🌍 非中国', 'select', 'geolocation-!cn'],
    ['netflix', 'Netflix', '🎯 Netflix', 'select', 'netflix'],
  ].map(([id, name, yaml, standard, ...sets]) => ({
    id,
    name,
    yaml,
    standard,
    sets,
    locked: ['private', 'cn'].includes(id),
    block_only: id === 'ad',
    catalog: id === 'netflix',
  })) as ClashRulesView['catalog']['modules'];
  let view: ClashRulesView = {
    version: 0,
    writable,
    rules: { custom: [], modules: {}, extra: [], final: 'select' },
    catalog: {
      modules,
      lines: [
        { id: 'line:香港示例 01', name: '香港示例 01', yaml: '📍 香港示例 01' },
        { id: 'line:美国示例 01', name: '美国示例 01', yaml: '📍 美国示例 01' },
      ],
      select: '🚀 节点选择',
      auto: '⚡ 自动选择',
      final_group: '🐟 漏网之鱼',
    },
  };
  return (url: URL, init?: RequestInit): Response | null => {
    if (url.pathname.endsWith('/clash-rules')) {
      if (init?.method === 'PUT') {
        const write = JSON.parse(String(init.body));
        if (!writable) return Response.json({ error: '规则只读' }, { status: 403 });
        if (write.version !== view.version)
          return Response.json({ error: '规则已在其他窗口更新，请重新载入后再保存' }, { status: 409 });
        view = { ...view, version: view.version + 1, rules: write.rules };
      }
      return Response.json(view);
    }
    if (url.pathname.endsWith('/clash-rules/preview'))
      return Response.json({
        yaml: 'proxy-groups:\n  - name: 🚀 节点选择\n    type: select\nrules:\n  - MATCH,🐟 漏网之鱼\n',
      });
    if (url.pathname.endsWith('/clash-rules/probe'))
      return Response.json({ layer: 'module', id: 'ai', target: 'select', via: 'RULE-SET,openai' });
    return null;
  };
}
