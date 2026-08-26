// 行级 diff。产物在几十到几百行，LCS 的 O(n·m) 复杂度可以满足；
// 遇到超大文件时退化为整体替换，接受 diff 效果下降以避免界面阻塞。

export interface DiffOp {
  t: ' ' | '-' | '+';
  /* 新文本中的行号；删除的行没有该值 */
  n: number | null;
  s: string;
}

const CELL_CAP = 4_000_000;

export function diffLines(oldText: string, newText: string): DiffOp[] {
  const A = (oldText ?? '').split('\n');
  const B = (newText ?? '').split('\n');
  const n = A.length;
  const m = B.length;

  if (n * m > CELL_CAP) {
    return [...A.map<DiffOp>(s => ({ t: '-', n: null, s })), ...B.map<DiffOp>((s, i) => ({ t: '+', n: i + 1, s }))];
  }

  const lcs: Uint32Array[] = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lcs[i][j] = A[i] === B[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }

  const ops: DiffOp[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (A[i] === B[j]) {
      ops.push({ t: ' ', n: j + 1, s: A[i] });
      i++;
      j++;
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
      ops.push({ t: '-', n: null, s: A[i] });
      i++;
    } else {
      ops.push({ t: '+', n: j + 1, s: B[j] });
      j++;
    }
  }
  while (i < n) ops.push({ t: '-', n: null, s: A[i++] });
  while (j < m) ops.push({ t: '+', n: j + 1, s: B[j++] });
  return ops;
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
  if (n == null) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 ** 2).toFixed(1)} MB`;
}
