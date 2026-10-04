import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const styles = source('../src/styles.css');
const shell = source('../src/forge/shell.tsx');
const route = source('../src/forge/route.ts');
const nodes = source('../src/panes/nodes.tsx');
const telemetry = source('../src/panes/telemetry.tsx');
const nodeObservationCharts = source('../src/panes/node-observation-charts.tsx');
const echartsMotion = source('../src/ui/echarts-motion.ts');
const chains = source('../src/panes/chains.tsx');
const tunnels = source('../src/panes/tunnels.tsx');
const deploy = source('../src/panes/deploy.tsx');
const users = source('../src/panes/users.tsx');
const dialog = source('../src/ui/dialog.tsx');
const crumb = source('../src/wm/crumb.ts');

test('motion uses shared timing curves and preserves reduced-motion behavior', () => {
  const motion = source('../src/ui/motion.ts');
  assert.match(motion, /flushSync\(update\)/);
  assert.match(styles, /--motion-fast:\s*140ms/);
  assert.match(styles, /--motion-medium:\s*220ms/);
  assert.match(styles, /--motion-slow:\s*300ms/);
  assert.match(
    styles,
    /@media \(prefers-reduced-motion: reduce\)[\s\S]*animation-duration:\s*0\.01ms !important[\s\S]*transition-duration:\s*0\.01ms !important/,
  );
  assert.match(styles, /\.st-running,[\s\S]*\.st-failed-dirty\s*\{\s*animation:\s*none !important/);
});

test('initial loading stays blank and real surfaces animate only when mounted', () => {
  assert.doesNotMatch(styles, /loading-skeleton-breathe|\.loading-skeleton\s*\{[^}]*animation:/s);
  assert.doesNotMatch(styles, /loading-skeleton-exit|loading-content-enter|loading-indicator/);
  assert.doesNotMatch(shell, /<div className="fg-sheet">\s*<Pane/s);
  assert.match(styles, /\.loading-boundary-guard ~ \*:has\(~ \.loading-boundary-end\)\s*\{[^}]*visibility:\s*hidden/s);
  assert.match(styles, /\.fg-view :is\(\.fg-sheet, \.panel, \.chart-card\)\s*\{[^}]*motion-surface-mount/s);
  assert.match(styles, /\.fg-view :is\(\.ncard, \.chain-card, \.tunnel-row\)\s*\{[^}]*motion-card-mount/s);
  assert.match(styles, /@keyframes motion-surface-mount[\s\S]*translate3d\(0, 6px, 0\)/);
});

test('primary transient surfaces have paired entrance and exit motion', () => {
  assert.match(styles, /\.fg-menu\[data-motion-state='entering'\]/);
  assert.match(styles, /\.fg-menu\[data-motion-state='exiting'\]/);
  assert.match(styles, /\.fg-rail\[data-motion-state='entering'\]/);
  assert.match(styles, /\.fg-rail\[data-motion-state='exiting'\]/);
  assert.match(styles, /\.dialog-layer\.modal\[data-motion-state='entering'\]/);
  assert.match(styles, /\.dialog-layer\.modal\[data-motion-state='exiting'\]/);
  assert.match(styles, /\.dialog-layer\.drawer\[data-motion-state='exiting'\]/);
  assert.match(
    styles,
    /\.dialog-layer\[data-motion-state='exiting'\]\s*\{[^}]*pointer-events:\s*none/s,
    'a retained full-screen dialog must not intercept the page during its exit animation',
  );
  assert.match(styles, /\.observe-range-menu\[hidden\][\s\S]*visibility:\s*hidden/);
  assert.match(shell, /usePresence\(panel\.open && artifacts, 300\)/);
  assert.match(shell, /usePresence\(more, 180\)/);
  assert.match(shell, /usePresence\(st\.diag, 180\)/);
  assert.match(dialog, /usePresence\(!closing, duration\)/);
});

test('press depth stays scoped to the existing button controls', () => {
  assert.match(styles, /\.btn:active:not\(:disabled\)\s*\{\s*transform:\s*scale\(0\.96\)/);
  assert.match(styles, /\.fg-tgl:active:not\(:disabled\)[\s\S]*?transform:\s*scale\(0\.96\)/);
  assert.doesNotMatch(styles, /\.fg-nv:active\s*\{/);
  assert.doesNotMatch(styles, /:is\(\.segsw,[^)]*\) button:active/);
  assert.doesNotMatch(styles, /:is\(\.tunnel-kind-grid[^)]*\):active/);
});

test('clickable machine KPIs advertise their expandable state', () => {
  assert.match(styles, /\.kpi\.kpi-expand\.open\s*\{\s*border-color:\s*var\(--action\);\s*\}/s);
  assert.match(
    styles,
    /:root\[data-theme='light'\] \.nd-sheet \.kpi\.kpi-expand\.open:not\(\.warn, \.bad\)\s*\{\s*border-color:\s*var\(--action\);\s*\}/s,
  );
  assert.doesNotMatch(styles, /kpi-expand-mark/);
  assert.doesNotMatch(styles, /点击(?:展开|收起)/);
  assert.equal((telemetry.match(/aria-label=\{(?:cpu|memory|disk|network)DetailTitle\}/g) ?? []).length, 4);
});

test('detail ECharts sweep into view once while KPI sparklines stay independent', () => {
  assert.match(telemetry, /usePresence\(detail\.open !== null, 160\)/);
  assert.match(telemetry, /chart\.showLoading\('default'/);
  assert.match(telemetry, /chart\.hideLoading\(\)/);
  assert.match(echartsMotion, /new IntersectionObserver\(/);
  assert.match(echartsMotion, /entry\.isIntersecting && entry\.intersectionRatio >= ENTER_THRESHOLD/);
  assert.match(echartsMotion, /const ENTER_THRESHOLD = 0\.35/);
  assert.match(echartsMotion, /const ANIMATION_POINT_THRESHOLD = 10_000/);
  assert.match(echartsMotion, /animationThreshold:\s*ANIMATION_POINT_THRESHOLD/);
  assert.match(echartsMotion, /animationDuration:\s*animate \? 360 : 0/);
  assert.match(echartsMotion, /animationEasing:\s*'cubicInOut'/);
  assert.match(echartsMotion, /animationDurationUpdate:\s*0/);
  assert.match(telemetry, /!chart \|\| !ready \|\| !enteredViewport/);
  assert.equal((telemetry.match(/useEchartsViewportEntry\(elRef\)/g) ?? []).length, 2);
  assert.equal((nodeObservationCharts.match(/useEchartsViewportEntry\(elRef\)/g) ?? []).length, 1);
  assert.equal((telemetry.match(/echartsEntranceAnimation\(!hasRenderedData\.current\)/g) ?? []).length, 2);
  assert.equal((nodeObservationCharts.match(/echartsEntranceAnimation\(!hasRenderedData\.current\)/g) ?? []).length, 1);
  assert.doesNotMatch(telemetry, /function Spark[\s\S]*?echartsEntranceAnimation/);
  assert.match(nodes, /尚无 Agent 上报样本。/);
  assert.match(nodes, /Ping 落点已配置，但 Agent 尚未上报样本。/);
  assert.match(styles, /\.kpi-detail-motion-content\s*\{[^}]*motion-kpi-detail-content-in 220ms/s);
  assert.match(styles, /motion-kpi-detail-content-in[\s\S]*translate3d\(0, 6px, 0\)/);
  assert.doesNotMatch(styles, /motion-kpi-detail-(?:expand|collapse)/);
  assert.doesNotMatch(styles, /\.kpi-detail-motion\s*\{[^}]*grid-template-rows/s);
  assert.doesNotMatch(styles, /motion-kpi-detail-content-in\s*\{[^}]*clip-path/s);
});

test('content navigation updates directly without workspace snapshot transitions', () => {
  for (const [pane, nav] of [
    [nodes, 'nodes'],
    [chains, 'chains'],
    [tunnels, 'tunnels'],
    [deploy, 'deploy'],
  ]) {
    assert.match(pane, new RegExp(`const go = [^\\n]+=> navigate\\('${nav}',`));
    assert.doesNotMatch(pane, /runVisualTransition/);
  }
  assert.match(shell, /const goto = \(drill\?: unknown\) => returnTo\(nav, drill as Loc\['drill'\]\)/);
  assert.match(nodes, /setTabState\(\{ id, tab: next \}\)/);
  assert.doesNotMatch(nodes, /data-tab-transition|tabSwitched|switched:\s*(?:true|false)/);
  assert.doesNotMatch(
    styles,
    /data-motion-transition='(?:navigation|drill|selection)'|motion-(?:page|drill|node-detail|tab-content)/,
  );
  assert.match(crumb, /useLayoutEffect\(\(\) => \{/);
});

test('user selection replaces detail directly without a snapshot cross-fade', () => {
  assert.match(users, /const go = \(d: Drill\) => navigate\('users', d\)/);
  assert.doesNotMatch(users, /'selection'/);
  assert.doesNotMatch(styles, /data-motion-transition='selection'|motion-user-detail/);
  assert.match(
    styles,
    /\.fg-view \.user-split-detail,\s*\.fg-view \.user-split-detail :is\(\.panel, \.chart-card\)\s*\{\s*animation:\s*none;\s*\}/,
    'user detail and nested panels must not replay the shared mount animation on selection',
  );
  assert.match(users, /<section key=\{r\.key\} className="panel user-split-detail">/);
});

test('navigation updates directly while theme and palette retain progressive transitions', () => {
  assert.match(route, /const moveTo =[\s\S]*?cancelVisualTransition\(\)[\s\S]*?apply\(loc\)/);
  assert.match(route, /export function navigate[\s\S]*?moveTo\(nav, drill, false\)/);
  assert.match(route, /export function returnTo[\s\S]*?moveTo\(nav, drill, true\)/);
  assert.match(route, /const pushLocation =[\s\S]*?writeRouteHistory\('pushState'/);
  assert.match(route, /if \(serialize\(previous\) === canonicalHash &&/);
  assert.match(route, /const restore =[\s\S]*?apply\(next\)/);
  assert.doesNotMatch(route, /runVisualTransition|sameDrillPage/);
  assert.doesNotMatch(styles, /view-transition-name:\s*forge-view|motion-page-|data-motion-transition='navigation'/);
  assert.match(styles, /data-motion-transition='theme-dark'[\s\S]*motion-theme-diagonal-dark 650ms/);
  assert.match(styles, /data-motion-transition='theme-light'[\s\S]*motion-theme-diagonal-light 650ms/);
  assert.match(styles, /linear-gradient\(135deg, black 40%, transparent 60%\)/);
  assert.match(styles, /linear-gradient\(315deg, black 40%, transparent 60%\)/);
  assert.match(styles, /data-motion-transition='appearance'[\s\S]*motion-appearance-reveal/);
  assert.match(shell, /theme\.snapshot\(\) === 'dark' \? 'theme-light' : 'theme-dark'/);
  assert.match(shell, /\(\) => palette\.set\(option\.key\)/);
});

test('machine detail and reading tabs avoid opacity transitions', () => {
  assert.match(nodes, /nd-sheet nd-page nd-node-detail/);
  assert.doesNotMatch(nodes, /data-tab-transition|tabSwitched/);
  assert.doesNotMatch(styles, /motion-node-detail|motion-tab-content|data-tab-transition/);
});
