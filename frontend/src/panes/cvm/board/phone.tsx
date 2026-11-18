// 手机：拖线不适合触屏，按宿主分组列出每个容器的入口与出口芯片；点容器打开实例详情。
import type { CvmChainCatalog, CvmEntryBoard, CvmInstance, CvmWorkspace } from '../../../api';
import { RegionFlag } from '../../../ui/region-flag';
import { navigate } from '../../../forge/route';
import { NetworkLogsButton } from '../network-logs';
import { stackHosts } from '../queries';
import { entryText, isDefault, type ExitTile, type RuleLink } from './model';

export function PhoneBoard({
  workspace,
  chains,
  instances,
  entries,
  exits,
  rules,
}: {
  workspace: CvmWorkspace;
  chains: CvmChainCatalog | undefined;
  instances: CvmInstance[];
  entries: CvmEntryBoard;
  exits: ExitTile[];
  rules: RuleLink[];
}) {
  return (
    <div className="cz-phone">
      {stackHosts(workspace).map(host => {
        const here = instances.filter(item => item.node_id === host.node_id);
        if (!here.length) return null;
        return (
          <section key={host.node_id} className="cz-phone-host" aria-label={host.name}>
            <header>
              <b>{host.name}</b>
              <NetworkLogsButton
                iconOnly
                title="网络配置日志"
                allowed={workspace.can_manage_network}
                machines={[
                  { node_id: host.node_id, name: host.name, role: '计算' },
                  ...entries.entries
                    .filter(e => here.some(i => i.desired.id === e.instance_id))
                    .map(e => ({
                      node_id: e.provider_node,
                      name: workspace.hosts.find(h => h.node_id === e.provider_node)?.name,
                      role: '入口',
                    })),
                  ...here.flatMap(i =>
                    i.desired.spec.interfaces.flatMap(n =>
                      n.allocated
                        ? [
                            {
                              node_id: n.allocated.provider_node,
                              name: workspace.hosts.find(h => h.node_id === n.allocated?.provider_node)?.name,
                              role: '出口',
                            },
                          ]
                        : [],
                    ),
                  ),
                ]}
              />
              {!host.fresh && <small>离线</small>}
            </header>
            {here.map(instance => {
              const mine = entries.entries.filter(item => item.instance_id === instance.desired.id);
              const links = rules.filter(item => item.instance === instance.desired.id);
              return (
                <button
                  key={instance.desired.id}
                  className="cz-phone-row"
                  onClick={() => navigate('cvm', { p: 'instance', id: instance.desired.id })}
                >
                  <span className="cz-phone-name">
                    <span className={`cz-dot ${instance.status} ${instance.desired.power}`} />
                    <b>{instance.desired.spec.name}</b>
                  </span>
                  <span className="cz-phone-flow">
                    <small>入</small>
                    {mine.length ? (
                      mine.map(entry => {
                        const text = entryText(entry);
                        return (
                          <span key={entry.id} className={`cz-chip ${text.layer} ${entry.state}`}>
                            {entry.mode.kind === 'l3' ? '整 IP' : `${text.badge} ${entry.mode.listen_port}`}
                          </span>
                        );
                      })
                    ) : (
                      <span className="cz-chip none">无</span>
                    )}
                  </span>
                  <span className="cz-phone-flow">
                    <small>出</small>
                    {[
                      ...new Map(
                        links.map(link => {
                          const text = `${(link.route.chain_id && chains?.chains.find(c => c.id === link.route.chain_id)?.name) || exits.find(item => item.id === link.exit)?.provider || '网卡'} · ${
                            link.route.service
                              ? `${link.route.service.protocol}/${link.route.service.port_start}`
                              : isDefault(link.route.destination)
                                ? '默认'
                                : '网段'
                          }`;
                          return [
                            text,
                            {
                              layer: link.layer,
                              country: chains?.chains.find(c => c.id === link.route.chain_id)?.country,
                            },
                          ] as const;
                        }),
                      ),
                    ].map(([text, { layer, country }]) => (
                      <span key={text} className={`cz-chip ${layer}`}>
                        <RegionFlag code={country} />
                        {text}
                      </span>
                    ))}
                    {!links.some(link => isDefault(link.route.destination) && !link.route.service) && (
                      <span className="cz-chip none">本机</span>
                    )}
                  </span>
                </button>
              );
            })}
          </section>
        );
      })}
    </div>
  );
}
