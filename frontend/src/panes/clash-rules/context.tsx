import { createContext, useContext, useMemo, type ReactNode } from 'react';
import {
  moduleOrder,
  type RoutingCatalog,
  type GlyphId,
  type Line,
  type ModuleDef,
  type Rules,
  type Target,
} from '../../clash-rules';

const glyphs: Record<string, GlyphId> = { ad: 'ban', private: 'house', ai: 'sparkle', cn: 'cn', global: 'globe' };
function routing(catalog: RoutingCatalog) {
  const modules: ModuleDef[] = catalog.modules.map(module => ({
    ...module,
    blockOnly: module.block_only,
    glyph: glyphs[module.id] ?? (module.id as GlyphId),
  }));
  const LINES: Line[] = catalog.lines.map(line => ({ ...line, region: '' }));
  const FINAL: ModuleDef = {
    id: 'final',
    name: '漏网之鱼',
    yaml: catalog.final_group,
    glyph: 'fish',
    standard: 'select',
    sets: [],
    locked: true,
  };
  const standardRules = (): Rules => ({
    custom: [],
    modules: Object.fromEntries(modules.filter(m => !m.catalog).map(m => [m.id, { on: true, target: m.standard }])),
    extra: [],
    final: 'select',
  });
  const targetName = (target: Target) =>
    (({ select: '节点选择', auto: '自动选择', direct: '直连', reject: '拦截' }) as Record<string, string>)[target] ??
    LINES.find(line => line.id === target)?.name ??
    '线路已不可用';
  const yamlTarget = (target: Target) =>
    (({ select: catalog.select, auto: catalog.auto, direct: 'DIRECT', reject: 'REJECT' }) as Record<string, string>)[
      target
    ] ??
    LINES.find(line => line.id === target)?.yaml ??
    '';
  return {
    catalog,
    STANDARD: modules.filter(m => !m.catalog),
    CATALOG: modules.filter(m => m.catalog),
    FINAL,
    LINES,
    targetName,
    yamlTarget,
    standardRules,
    moduleOrder: (rules: Rules) => moduleOrder(rules, modules),
  };
}
const RoutingContext = createContext<ReturnType<typeof routing> | null>(null);
export function RoutingProvider({ catalog, children }: { catalog: RoutingCatalog; children: ReactNode }) {
  const value = useMemo(() => routing(catalog), [catalog]);
  return <RoutingContext.Provider value={value}>{children}</RoutingContext.Provider>;
}
export function useRouting() {
  const value = useContext(RoutingContext);
  if (!value) throw new Error('缺少订阅规则上下文');
  return value;
}
export function normalizeRules(rules: Rules, catalog: RoutingCatalog): Rules {
  return { ...rules, modules: { ...routing(catalog).standardRules().modules, ...rules.modules } };
}
export const isStandardRules = (rules: Rules, catalog: RoutingCatalog) =>
  !rules.custom.length &&
  !rules.extra.length &&
  rules.final === 'select' &&
  catalog.modules
    .filter(m => !m.catalog)
    .every(m => !rules.modules[m.id] || (rules.modules[m.id].on && rules.modules[m.id].target === m.standard));
