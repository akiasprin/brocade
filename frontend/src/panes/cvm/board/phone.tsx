// 手机：拖线不适合触屏，按宿主分组列出每个容器的入口与出口芯片；点容器打开实例详情。
import type { CvmEntryBoard, CvmInstance, CvmWorkspace } from '../../../api';
import { navigate } from '../../../forge/route';
import { stackHosts } from '../queries';
import { entryText, isDefault, type ExitTile, type RuleLink } from './model';

export function PhoneBoard({
  workspace,
  instances,
  entries,
  exits,
  rules,
}: {
  workspace: CvmWorkspace;
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
                          const text = `${exits.find(item => item.id === link.exit)?.provider ?? '网卡'} · ${
                            link.route.service
                              ? `${link.route.service.protocol}/${link.route.service.port_start}`
                              : isDefault(link.route.destination)
                                ? '默认'
                                : '网段'
                          }`;
                          return [text, link.layer] as const;
                        }),
                      ),
                    ].map(([text, layer]) => (
                      <span key={text} className={`cz-chip ${layer}`}>
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
