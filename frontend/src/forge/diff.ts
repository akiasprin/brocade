import { fileBytes } from '../ui/format';

// 行级 diff。产物在几十到几百行，LCS 的 O(n·m) 复杂度可以满足；
// 遇到超大文件时退化为整体替换，接受 diff 效果下降以避免界面阻塞。

export interface DiffOp {
  t: ' ' | '-' | '+';
  /* 新文本中的行号；删除的行没有该值 */
  n: number | null;
  s: string;
}

const CELL_CAP = 4_000_000;

/** 对齐结果：相同行的 `s` 取新文本，`was` 是旧文本（比较键归一化后两者可能不同）。 */
interface AlignedLine {
  t: ' ' | '-' | '+';
  s: string;
  was?: string;
}

/** LCS 对齐两组行。`key` 决定哪些行算作相同；同分时先删后增。 */
function alignLines(oldLines: string[], newLines: string[], key: (line: string) => string = line => line) {
  const A = oldLines.map(key);
  const B = newLines.map(key);
  const n = A.length;
  const m = B.length;

  if (n * m > CELL_CAP) {
    return [...oldLines.map<AlignedLine>(s => ({ t: '-', s })), ...newLines.map<AlignedLine>(s => ({ t: '+', s }))];
  }

  const lcs: Uint32Array[] = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lcs[i][j] = A[i] === B[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }

  const ops: AlignedLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (A[i] === B[j]) {
      ops.push({ t: ' ', s: newLines[j], was: oldLines[i] });
      i++;
      j++;
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
      ops.push({ t: '-', s: oldLines[i] });
      i++;
    } else {
      ops.push({ t: '+', s: newLines[j] });
      j++;
    }
  }
  while (i < n) ops.push({ t: '-', s: oldLines[i++] });
  while (j < m) ops.push({ t: '+', s: newLines[j++] });
  return ops;
}

export function diffLines(oldText: string, newText: string): DiffOp[] {
  let line = 0;
  return alignLines((oldText ?? '').split('\n'), (newText ?? '').split('\n')).map(op => ({
    t: op.t,
    n: op.t === '-' ? null : ++line,
    s: op.s,
  }));
}

/* ── 审阅视图：变更单详情「产物记录」 ──────────────────────────────────────────
   比 diffLines 多做四件事：新旧双行号；纯插入/删除块平移到对象边界；成对修改行标出行内改动段；
   可选地把 ruleTag 重新编号当作未改动。 */

/** 审阅视图的一行。`o` / `n` 是旧 / 新行号；`mark` 是行内改动段的纯文本偏移 [起, 止)。 */
export interface ReviewRow {
  t: ' ' | '-' | '+';
  s: string;
  o: number | null;
  n: number | null;
  /** 只有比较时忽略的部分（ruleTag 编号）不同，按未改动显示 */
  retagged: boolean;
  mark?: [number, number];
}

export interface ReviewDiff {
  rows: ReviewRow[];
  counts: { add: number; del: number };
  /** 按未改动处理的 ruleTag 重新编号行数 */
  retagged: number;
  /** 旧 / 新规则表的生成号，没有 ruleTag 时为 undefined */
  generations: [string | undefined, string | undefined];
  /** 每一行所在的位置（JSON 键路径或 ini 段），下标是行号 − 1 */
  paths: { old: string[]; new: string[] };
}

// brocade-core 的 stamp_rule_tags 用整张规则表的哈希作生成号：`r:{8 位十六进制}:{三位序号}`。
// 规则表任何一处改动都会换掉生成号，每条规则的 ruleTag 都跟着变，这些行不是本次的内容改动。
const RULE_TAG = /("ruleTag": ")r:([0-9a-f]{8}):(\d{3})(")/;

/** 按行切分；以换行结尾的文件不留最后那一行空行。 */
export const artifactLines = (text: string) => {
  const lines = text.split('\n');
  if (lines.length > 1 && lines[lines.length - 1] === '') lines.pop();
  return lines;
};

const indentOf = (line: string) => (line.trim() === '' ? 99 : line.length - line.trimStart().length);
const settled = (op: AlignedLine) => op.t === ' ' && op.was === op.s;

/** 纯插入或纯删除的连续块可以在相同的上下文行之间平移而不改变含义；LCS 常把新增的 JSON 对象
 *  错开一行（从第二行开始、吞掉下一个对象的 `{`）。取首尾两行缩进之和最小的位置。 */
function slideBlocks(ops: AlignedLine[]) {
  let i = 0;
  while (i < ops.length) {
    if (ops[i].t === ' ') {
      i++;
      continue;
    }
    const type = ops[i].t;
    let start = i;
    let end = i;
    while (end < ops.length && ops[end].t === type) end++;
    const pure = (start === 0 || ops[start - 1].t === ' ') && (end === ops.length || ops[end].t === ' ');
    if (!pure) {
      i = end;
      continue;
    }
    const up = () => {
      ops[start - 1].t = type;
      ops[end - 1] = { t: ' ', s: ops[end - 1].s, was: ops[end - 1].s };
      start--;
      end--;
    };
    const down = () => {
      ops[start] = { t: ' ', s: ops[start].s, was: ops[start].s };
      ops[end].t = type;
      start++;
      end++;
    };
    while (start > 0 && settled(ops[start - 1]) && ops[start - 1].s === ops[end - 1].s) up();
    let best = start;
    let bestScore = indentOf(ops[start].s) + indentOf(ops[end - 1].s);
    while (end < ops.length && settled(ops[end]) && ops[end].s === ops[start].s) {
      down();
      const score = indentOf(ops[start].s) + indentOf(ops[end - 1].s);
      if (score < bestScore) {
        best = start;
        bestScore = score;
      }
    }
    while (start > best) up();
    i = end;
  }
  return ops;
}

/** k 行删除紧跟 k 行新增时逐对比较，标出行内改动段，范围扩展到完整的字母数字词。
 *  相似度不到一半的两行不标：那是整行替换，标出来只是一整行高亮。 */
function markPairs(rows: ReviewRow[]) {
  const word = (ch: string | undefined) => /[A-Za-z0-9]/.test(ch ?? '');
  let i = 0;
  while (i < rows.length) {
    if (rows[i].t !== '-') {
      i++;
      continue;
    }
    let j = i;
    while (j < rows.length && rows[j].t === '-') j++;
    let k = j;
    while (k < rows.length && rows[k].t === '+') k++;
    if (k - j === j - i) {
      for (let p = 0; p < j - i; p++) {
        const a = rows[i + p].s;
        const b = rows[j + p].s;
        let head = 0;
        while (head < a.length && head < b.length && a[head] === b[head]) head++;
        let tail = 0;
        while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail])
          tail++;
        // 相似度不计缩进：缩进相同的两行天然有很长的公共前缀
        const indent = Math.min(head, indentOf(a), indentOf(b));
        if (head - indent + tail < (Math.min(a.length, b.length) - indent) * 0.5) continue;
        while (head > 0 && word(a[head - 1]) && word(a[head])) head--;
        while (tail > 0 && word(a[a.length - tail]) && word(a[a.length - tail - 1])) tail--;
        rows[i + p].mark = [head, a.length - tail];
        rows[j + p].mark = [head, b.length - tail];
      }
    }
    i = k;
  }
  return rows;
}

/** 每一行所在的位置：JSON 取外层键路径（routing › rules[6]），ini 取所在段与段首注释。 */
function lineLocations(lines: string[], fmt: string): string[] {
  if (fmt === 'ini') {
    let section = '';
    return lines.map((line, i) => {
      if (/^\[.*\]$/.test(line.trim())) {
        const note = lines[i + 1]?.match(/^#\s*(.+)$/);
        section = note ? `${line.trim()} ${note[1]}` : line.trim();
      }
      return section;
    });
  }
  if (fmt !== 'json') return lines.map(() => '');
  const stack: { name: string | null; array: boolean; index: number }[] = [];
  const show = (own: string | null) => {
    let path = '';
    for (const name of [...stack.map(frame => frame.name), own]) {
      if (!name) continue;
      path += name.startsWith('[') || !path ? name : ` › ${name}`;
    }
    return path;
  };
  return lines.map(line => {
    const text = line.trim();
    if (/^[\]}]/.test(text)) {
      const path = show(null);
      stack.pop();
      return path;
    }
    const parent = stack[stack.length - 1];
    const keyed = text.match(/^"((?:[^"\\]|\\.)*)"\s*:\s*(.*)$/);
    let name = keyed ? keyed[1] : null;
    const value = keyed ? keyed[2] : text;
    if (!keyed && parent?.array) {
      parent.index += 1;
      name = `[${parent.index}]`;
    }
    const opens = /[[{]$/.test(value);
    const path = show(opens ? name : null);
    if (opens) stack.push({ name, array: value.endsWith('['), index: -1 });
    return path;
  });
}

/**
 * 审阅用的差异。`before` 为 null 表示新建（全部是新增行），`after` 为 null 表示停用（全部是删除行）。
 * `foldRuleTags` 只对 xray 产物生效：比较时忽略 ruleTag 的生成号与序号。
 */
export function reviewDiff(
  before: string | null,
  after: string | null,
  kind: string,
  foldRuleTags: boolean,
): ReviewDiff {
  const fmt = artifactFmt(kind);
  const oldLines = before === null ? [] : artifactLines(before);
  const newLines = after === null ? [] : artifactLines(after);
  const fold = foldRuleTags && kind === 'xray';
  const aligned =
    before === null
      ? newLines.map<AlignedLine>(s => ({ t: '+', s }))
      : after === null
        ? oldLines.map<AlignedLine>(s => ({ t: '-', s }))
        : alignLines(oldLines, newLines, fold ? line => line.replace(RULE_TAG, '$1…$4') : undefined);
  let oldLine = 0;
  let newLine = 0;
  const rows = markPairs(
    slideBlocks(aligned).map<ReviewRow>(op => ({
      t: op.t,
      s: op.s,
      o: op.t === '+' ? null : ++oldLine,
      n: op.t === '-' ? null : ++newLine,
      retagged: op.t === ' ' && op.was !== undefined && op.was !== op.s,
    })),
  );
  return {
    rows,
    counts: countChanges(rows),
    retagged: rows.filter(row => row.retagged).length,
    generations: [before?.match(RULE_TAG)?.[2], after?.match(RULE_TAG)?.[2]],
    paths: { old: lineLocations(oldLines, fmt), new: lineLocations(newLines, fmt) },
  };
}

/** 在 highlight 产出的 HTML 里按纯文本偏移包上 <mark>；跨过标签时先闭合再重开，保持嵌套合法。 */
export function markRange(html: string, from: number, to: number): string {
  if (from >= to) return html;
  let out = '';
  let offset = 0;
  let open = false;
  for (let i = 0; i < html.length;) {
    if (offset === from && !open) {
      out += '<mark>';
      open = true;
    }
    if (offset === to && open) {
      out += '</mark>';
      open = false;
    }
    const tagEnd = html[i] === '<' ? html.indexOf('>', i) + 1 : 0;
    if (tagEnd > i) {
      out += open ? `</mark>${html.slice(i, tagEnd)}<mark>` : html.slice(i, tagEnd);
      i = tagEnd;
      continue;
    }
    // highlight 会把 & < > 转成实体，一个实体对应原文的一个字符
    const entityEnd = html[i] === '&' ? html.indexOf(';', i) + 1 : 0;
    const next = entityEnd > i ? entityEnd : i + 1;
    out += html.slice(i, next);
    i = next;
    offset++;
  }
  if (open) out += '</mark>';
  return out.replace(/<mark><\/mark>/g, '');
}

export function countChanges(ops: DiffOp[]): { add: number; del: number } {
  let add = 0;
  let del = 0;
  for (const op of ops) {
    if (op.t === '+') add++;
    else if (op.t === '-') del++;
  }
  return { add, del };
}

/* 简化的着色实现。产物只有 json / ini / yaml / uri 四种格式，不需要完整的语法分析。 */
export function highlight(line: string, fmt: string): string {
  const esc = line.replace(/[&<>]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c] as string);
  if (fmt === 'json') {
    return highlightJsonLine(esc);
  }
  if (fmt === 'ini') {
    return esc
      .replace(/^(#.*)$/, '<i class="c">$1</i>')
      .replace(/^(\[.*\])$/, '<i class="k">$1</i>')
      .replace(/^([A-Za-z]+)(\s*=\s*)(.*)$/, '<i class="k">$1</i>$2<i class="s">$3</i>');
  }
  if (fmt === 'yaml') {
    return esc
      .replace(/^(\s*-?\s*)([\w.-]+)(:)/, '$1<i class="k">$2</i>$3')
      .replace(/^(\s*#.*)$/, '<i class="c">$1</i>');
  }
  if (fmt === 'uri') {
    return esc.replace(/^(vless):/, '<i class="k">$1</i>:').replace(/([?&])([\w-]+)=/g, '$1<i class="k">$2</i>=');
  }
  return esc;
}

function highlightJsonLine(line: string): string {
  const keyed = line.match(/^(\s*)("(?:[^"\\]|\\.)*")(\s*:\s*)(.*)$/);
  if (keyed) {
    return `${keyed[1]}<i class="k">${keyed[2]}</i>${keyed[3]}${highlightJsonValue(keyed[4])}`;
  }
  return highlightJsonValue(line);
}

function highlightJsonValue(value: string): string {
  return value
    .replace(/^(\s*)("(?:[^"\\]|\\.)*")(\s*,?)$/, '$1<i class="s">$2</i>$3')
    .replace(/^(\s*)(-?\d+(?:\.\d+)?|true|false|null)(\s*,?)$/, '$1<i class="n">$2</i>$3');
}

/** 产物类型到文件名和格式的映射。与服务端的 artifact_kind 对应。 */
export const ARTIFACT_META: Record<string, { file: string; fmt: string }> = {
  phantun: { file: 'phantun.json', fmt: 'json' },
  xray: { file: 'xray.json', fmt: 'json' },
  wireguard: { file: 'wg0.conf', fmt: 'ini' },
  hy2_port_hop: { file: 'hy2_port_hop.json', fmt: 'json' },
  grants: { file: 'grants.json-rpc', fmt: 'json' },
  uri: { file: 'sub.uri.txt', fmt: 'uri' },
  clash: { file: 'clash.yaml', fmt: 'yaml' },
};

export const artifactFile = (kind: string) => ARTIFACT_META[kind]?.file ?? kind;
export const artifactFmt = (kind: string) => ARTIFACT_META[kind]?.fmt ?? '';

export function fmtBytes(n: number | null): string {
  return fileBytes(n);
}
