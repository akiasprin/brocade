import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { ChoiceSelect } from '../../ui/choice-menu';
import { Icon } from '../../ui/icons';
import { SubscriptionLogo } from '../subscription-logo';
import type { Rules, Target } from '../../clash-rules';
import { useRouting } from './context';
import {
  downloadPrivateScript,
  parsePrivateNodes,
  LINK_PROTOCOLS,
  chainProxyName,
  privateChainScript,
  type PrivateProxy,
  type PrivateExitMode,
} from './private-export';

export function PrivateChain({
  rules,
  rulesDirty,
  onBack,
  onPending,
}: {
  rules: Rules;
  rulesDirty: boolean;
  onBack: () => void;
  onPending: (pending: boolean) => void;
}) {
  const { FINAL, LINES, targetName, moduleOrder, yamlTarget, catalog } = useRouting();
  const id = useId();
  const root = useRef<HTMLElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const [link, setLink] = useState('');
  const [nodes, setNodes] = useState<PrivateProxy[]>([]);
  const [adding, setAdding] = useState(true);
  const [visible, setVisible] = useState(false);
  const [front, setFront] = useState<Target>('auto');
  const [serviceId, setServiceId] = useState('select');
  const [exitMode, setExitMode] = useState<PrivateExitMode>('select');
  const [error, setError] = useState('');
  const [importNote, setImportNote] = useState('');
  const [exported, setExported] = useState(false);
  const services = useMemo(
    () => [
      { id: 'select', name: '节点选择', yaml: catalog.select },
      ...moduleOrder(rules).filter(def => !def.locked && !def.blockOnly && rules.modules[def.id].on),
      FINAL,
    ],
    [rules, moduleOrder, FINAL, catalog.select],
  );
  const service = services.find(def => def.id === serviceId) ?? services[0];
  const identity = (node: PrivateProxy) => {
    const sorted = (value: unknown): unknown =>
      Array.isArray(value)
        ? value.map(sorted)
        : value && typeof value === 'object'
          ? Object.fromEntries(
              Object.entries(value)
                .sort(([a], [b]) => a.localeCompare(b))
                .map(([key, item]) => [key, sorted(item)]),
            )
          : value;
    return JSON.stringify(sorted({ ...node, name: '' }));
  };
  useEffect(() => {
    root.current?.closest('.clash-editor-body')?.scrollTo({ top: 0 });
    root.current?.querySelector<HTMLButtonElement>('.clash-chain-back')?.focus({ preventScroll: true });
  }, []);
  useEffect(() => {
    onPending(Boolean(link || nodes.length) && !exported);
  }, [link, nodes, exported, onPending]);
  const add = (value = link) => {
    setExported(false);
    setError('');
    setImportNote('');
    try {
      const parsed = parsePrivateNodes(value);
      const seen = new Set(nodes.map(identity));
      const fresh = parsed.filter(node => {
        const key = identity(node);
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      });
      if (!fresh.length) throw new Error('这些出口已在列表中，不需要重复添加。');
      if (nodes.length + fresh.length > 200) throw new Error('最多添加 200 个出口，请分开配置。');
      setNodes(current => [...current, ...fresh]);
      setImportNote(
        parsed.length > 1
          ? `已导入 ${fresh.length} 个出口${parsed.length > fresh.length ? `，跳过 ${parsed.length - fresh.length} 个重复节点` : ''}。`
          : '',
      );
      setLink('');
      setVisible(false);
      setAdding(false);
    } catch (cause) {
      setError((cause as Error).message);
    }
  };
  const clear = () => {
    setLink('');
    setNodes([]);
    setError('');
    setImportNote('');
    setExported(false);
    setVisible(false);
    setAdding(true);
    input.current?.focus();
  };
  return (
    <section ref={root} className="clash-chain-panel" aria-label="私有链式代理">
      <div className="clash-chain-top">
        <header className="sub-pass-head">
          <SubscriptionLogo card="clash" />
          <span className="sub-pass-name">
            <b>私有链式代理</b>
            <small>平台线路 → 你的自建出口</small>
          </span>
          <span className="clash-chain-local-badge">仅本机</span>
        </header>
        <button type="button" className="clash-chain-back" onClick={onBack}>
          <Icon of="chevronLeft" size={12} />
          返回规则
        </button>
        <p className="clash-chain-note">可添加多个自建出口，每次选一个使用。节点不上传，离开本页会清空。</p>
        {rulesDirty && (
          <p className="clash-chain-warning" role="status">
            普通规则还有未保存的更改，导出不会替你保存。
          </p>
        )}
        <div className="clash-chain-route" aria-label="代理顺序">
          <span>
            <small>前置线路</small>
            <b>{targetName(front)}</b>
          </span>
          <span className="clash-chain-route-arrow" aria-hidden="true">
            →
          </span>
          <span>
            <small>自建出口 · 选一</small>
            <b>{nodes.length > 1 ? `${nodes.length} 个自建出口` : (nodes[0]?.name ?? '等待添加')}</b>
          </span>
        </div>
        <div className="clash-chain-picks">
          <div className="substore-row">
            <span>应用到</span>
            <ChoiceSelect
              className="substore-row-pick"
              aria-label="私有链应用到"
              value={service.id}
              onChange={event => {
                const value = event.target.value;
                setServiceId(value);
                if (value === 'select' && front === 'select') setFront('auto');
                setExported(false);
              }}
            >
              <optgroup label="客户端入口">
                <option value="select" data-detail="在客户端选中链式代理后使用">
                  节点选择
                </option>
              </optgroup>
              <optgroup label="分流">
                {services
                  .filter(def => def.id !== 'select')
                  .map(def => (
                    <option key={def.id} value={def.id}>
                      {def.name}
                    </option>
                  ))}
              </optgroup>
            </ChoiceSelect>
          </div>
          <div className="substore-row">
            <span>前置线路</span>
            <ChoiceSelect
              className="substore-row-pick"
              aria-label="前置线路"
              value={front}
              onChange={event => {
                setFront(event.target.value as Target);
                setExported(false);
              }}
            >
              <optgroup label="客户端策略">
                <option
                  value="select"
                  disabled={service.id === 'select'}
                  data-detail={service.id === 'select' ? '会与链式代理形成循环' : '跟随客户端当前选项'}
                >
                  {targetName('select')}
                </option>
                <option value="auto" data-detail="按延迟自动选平台线路">
                  {targetName('auto')}
                </option>
              </optgroup>
              <optgroup label="平台线路">
                {LINES.map(line => (
                  <option value={line.id} key={line.id} data-badge={line.region}>
                    {line.name}
                  </option>
                ))}
              </optgroup>
            </ChoiceSelect>
          </div>
          {nodes.length > 1 && (
            <div className="substore-row">
              <span>出口选择</span>
              <ChoiceSelect
                className="substore-row-pick"
                aria-label="自建出口选择方式"
                value={exitMode}
                onChange={event => {
                  setExitMode(event.target.value as PrivateExitMode);
                  setExported(false);
                }}
              >
                <option value="select" data-detail="在客户端选择自建出口">
                  手动选择
                </option>
                <option value="url-test" data-detail="按整条代理链的延迟选择">
                  自动选出口
                </option>
              </ChoiceSelect>
            </div>
          )}
        </div>
        {service.id === 'select' && (
          <p className="clash-chain-scope-note">
            客户端分组：链式代理 → 节点选择。在「节点选择」里选中「链式代理」即可使用。前置须独立选择，避免循环代理。
          </p>
        )}
      </div>
      <div className="clash-chain-body">
        <div className="clash-chain-input-head">
          <label htmlFor={adding ? `${id}-link` : undefined}>自建出口</label>
          <button
            type="button"
            className="clash-chain-add"
            aria-expanded={adding}
            aria-label={adding ? '收起添加出口' : '添加自建出口'}
            onClick={() => setAdding(value => !value)}
          >
            <Icon of={adding ? 'close' : 'plus'} size={12} />
            {adding ? '收起' : '添加'}
          </button>
        </div>
        {adding && (
          <>
            <div className="clash-chain-input-wrap">
              <textarea
                ref={input}
                id={`${id}-link`}
                aria-label="自建节点链接"
                data-masked={!visible || undefined}
                rows={3}
                value={link}
                placeholder="粘贴节点链接（可多行），或 Clash YAML / JSON"
                autoComplete="off"
                spellCheck={false}
                aria-invalid={!!error}
                aria-describedby={`${id}-hint${error ? ` ${id}-error` : ''}`}
                onChange={event => {
                  setLink(event.target.value);
                  setError('');
                  setImportNote('');
                  setExported(false);
                }}
              />
              <button
                type="button"
                className="clash-chain-reveal"
                aria-label={visible ? '隐藏节点链接' : '显示节点链接'}
                onClick={() => setVisible(value => !value)}
              >
                {visible ? '隐藏' : '显示'}
              </button>
            </div>
            <p className="clash-chain-hint" id={`${id}-hint`}>
              多行链接或 Clash 配置均在本页解析，不代拉订阅网址。
            </p>
            <details className="clash-chain-import-formats">
              <summary>支持哪些格式</summary>
              <p>{LINK_PROTOCOLS} 分享链接。</p>
              <p>
                VLESS 支持 TCP、WS、gRPC、H2、XHTTP 和 TLS / Reality；VMess、Trojan
                支持各自常用传输。链接中未支持的参数会明确报错。
              </p>
              <p>
                Clash YAML / JSON 可提取 proxies，也支持单个节点或节点数组。WireGuard、SSH、Snell、SSR
                等原生节点保留连接参数，由客户端确认兼容性。
              </p>
            </details>
            <div className="clash-chain-import-actions">
              <button type="button" className="btn clash-chain-parse" disabled={!link.trim()} onClick={() => add()}>
                添加到列表
              </button>
            </div>
          </>
        )}
        {error && (
          <p className="clash-chain-error" role="alert" id={`${id}-error`}>
            {error}
          </p>
        )}
        {importNote && (
          <p className="clash-chain-success" role="status">
            {importNote}
          </p>
        )}
        {nodes.length > 0 && (
          <div className="clash-chain-node-list" role="region" aria-label="已添加的自建出口">
            <div className="clash-chain-list-title" role="status">
              已添加 {nodes.length} 个出口
            </div>
            <ul>
              {nodes.map((node, index) => (
                <li className="clash-chain-node" key={`${index}:${node.server}:${node.port}`}>
                  <span className="clash-chain-local-badge">
                    {node.type === 'http' && node.tls ? 'HTTPS' : node.type.toUpperCase()}
                  </span>
                  <b title={chainProxyName(index, node.name)}>{chainProxyName(index, node.name)}</b>
                  <button
                    type="button"
                    className="clash-chain-remove"
                    aria-label={`移除出口 ${index + 1}：${node.name}`}
                    onClick={() => {
                      setNodes(current => current.filter((_, position) => position !== index));
                      if (nodes.length === 1) setAdding(true);
                      setExported(false);
                      setError('');
                      setImportNote('');
                    }}
                  >
                    <Icon of="close" size={12} />
                  </button>
                  <code>
                    {node.server}:{node.port}
                  </code>
                </li>
              ))}
            </ul>
            {nodes.some(node => ['hysteria', 'hysteria2', 'tuic', 'wireguard', 'shadowquic'].includes(node.type)) && (
              <p className="clash-chain-hint">含 UDP 出口：前置线路也需支持 UDP。</p>
            )}
          </div>
        )}
        {nodes.length > 0 && link.trim() && (
          <p className="clash-chain-warning" role="status">
            还有未添加的链接，请先添加出口或清除输入再导出。
          </p>
        )}
        <div className="clash-chain-export">
          <button
            type="button"
            className="btn primary"
            disabled={!nodes.length || !!link.trim()}
            onClick={() => {
              try {
                downloadPrivateScript(
                  privateChainScript(
                    nodes,
                    {
                      name: front.startsWith('line:')
                        ? (LINES.find(line => line.id === front)?.name ?? '')
                        : yamlTarget(front),
                      kind: front.startsWith('line:') ? 'line' : 'group',
                      selection: front === 'select',
                    },
                    service,
                    catalog.select,
                    nodes.length > 1 ? exitMode : 'select',
                  ),
                );
                setExported(true);
                setError('');
              } catch (cause) {
                setError((cause as Error).message);
              }
            }}
          >
            <Icon of="download" size={14} />
            导出到客户端
          </button>
          <button type="button" className="btn" disabled={!link && !nodes.length} onClick={clear}>
            清空全部
          </button>
        </div>
        {exported && (
          <p className="clash-chain-success" role="status">
            已生成下载文件。安装订阅扩展脚本后，
            {service.id === 'select' ? '在「节点选择」中选中「链式代理」。' : '重新激活订阅即可使用。'}
          </p>
        )}
        <details className="clash-chain-install" open={exported || undefined}>
          <summary>
            用于 Clash Verge Rev <span>如何导入</span>
          </summary>
          <ol>
            <li>
              下载 <code>brocade-private-chain.js</code>
            </li>
            <li>在 Brocade 订阅上右键 → 编辑扩展脚本</li>
            <li>粘贴文件全文，保存后重新激活订阅</li>
            {service.id === 'select' && (
              <li>
                在「节点选择」选中「链式代理」，
                {nodes.length > 1 && exitMode === 'url-test' ? '出口将按延迟自动选择' : '再选自建出口'}
              </li>
            )}
          </ol>
          <p>平台订阅照常更新。换设备时需自己迁移这份文件。</p>
        </details>
      </div>
    </section>
  );
}
