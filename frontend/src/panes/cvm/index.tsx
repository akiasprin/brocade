import './styles.css';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useId, useState } from 'react';
import { fetchCvm, fetchCvmEntries, fetchCvmNicCatalog, type CvmWorkspace } from '../../api';
import { DraftSlot } from '../../forge/draft-slot';
import { navigate } from '../../forge/route';
import { EmptyState, ErrorBox, Loading } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import { TitleActions } from '../../ui/title-actions';
import { useCrumb } from '../../wm/crumb';
import type { Win } from '../../wm/store';
import { CreateForm } from './create';
import { InstanceDetail } from './detail';
import { InstanceList, type InstanceListView } from './instance-list';
import { CvmPendingChanges } from './pending-changes';
import { HostList } from './hosts';
import { AddHost } from './host-policy';
import { AdoptForm } from './adopt';
import { ENTRY_BOARD, QUERY, stackHosts, supportsCreate } from './queries';
import { NIC_CATALOG } from './network-common';
import { CvmPageHeader, CvmTabs } from './presentation';
import { NetworkPublicationEditor, NetworkPublications } from './network-publications';
import { EntryPublications } from './entry-publications';
import { editCount, useNetworkDraft } from './network-draft';
import { ManagedNetworking } from './networking';

const tabs = [
  { key: 'instances', label: '容器', icon: 'container' },
  { key: 'hosts', label: '宿主', icon: 'host' },
  { key: 'networks', label: '网络', icon: 'network' },
] as const;

export function CvmPane({ win }: { win: Win; bare?: boolean }) {
  const client = useQueryClient();
  const query = useQuery({
    queryKey: QUERY,
    queryFn: fetchCvm,
    refetchInterval: state => (state.state.data?.instances.some(item => item.status !== 'applied') ? 5_000 : 30_000),
  });
  const drill = win.data.drill as { p?: string; id?: string; tab?: string; host?: string; copy?: string } | undefined;
  const page = drill?.p;
  const publication = page === 'network' || page === 'network-new';
  const root = !['new', 'adopt', 'instance', 'network', 'network-new'].includes(page ?? '');
  const active = page === 'hosts' ? 'hosts' : page === 'networks' ? 'networks' : 'instances';
  const initialTab =
    drill?.tab === 'network' || drill?.tab === 'monitoring' || drill?.tab === 'artifacts' ? drill.tab : 'overview';
  const [listView, setListView] = useState<InstanceListView>({ filter: 'all', host: '', owner: '', search: '' });
  const id = useId();
  const data = query.data;
  const item = page === 'instance' ? data?.instances.find(instance => instance.desired.id === drill?.id) : undefined;
  const manager = !!data?.can_write && !data.redacted;
  const networkManager = !!data?.can_manage_network && !data.redacted;
  const network = useNetworkDraft(item, manager);
  // 页签计数：网络页签取入站与出站两份目录（清单与网络页也读这两份，命中同一缓存）。
  const entryBoard = useQuery({
    queryKey: ENTRY_BOARD,
    queryFn: fetchCvmEntries,
    enabled: networkManager && root,
    staleTime: 30_000,
  });
  const nicCatalog = useQuery({
    queryKey: NIC_CATALOG,
    queryFn: fetchCvmNicCatalog,
    enabled: networkManager && root,
    staleTime: 30_000,
  });
  const title = page === 'new' ? '创建容器' : page === 'adopt' ? '纳管裸机' : '容器';
  const viewHosts = () => navigate('cvm', { p: 'hosts' });
  const viewNetwork = () => navigate('cvm', { p: 'networks' });
  const createOnHost = (host: string) => navigate('cvm', { p: 'new', host });
  useCrumb(
    win,
    page === 'instance'
      ? [{ label: item?.desired.spec.name ?? '实例详情' }]
      : publication
        ? [{ label: page === 'network-new' ? '创建出站方式' : '编辑出站方式' }]
        : root
          ? []
          : [{ label: title }],
  );
  if (query.isPending) return <Loading />;
  if (query.error && !data) return <ErrorBox error={query.error} />;
  if (!data) return null;
  const counts = {
    instances: data.instances.filter(instance => instance.reserved).length,
    hosts: stackHosts(data).length,
    networks:
      entryBoard.data && nicCatalog.data
        ? entryBoard.data.publications.length + nicCatalog.data.publications.length
        : 0,
  };
  const visibleTabs = tabs
    .filter(tab => tab.key === 'instances' || (tab.key === 'hosts' ? manager : networkManager))
    .map(tab => ({
      ...tab,
      badge: { count: counts[tab.key], label: `${counts[tab.key]} 项`, tone: 'dim' as const },
    }));
  const denied =
    ((page === 'new' || page === 'hosts') && !manager) ||
    ((page === 'adopt' || active === 'networks' || publication) && !networkManager);
  return (
    <div className="cvm-page nd-page nd-sheet node-cardpage cvm-design">
      <div className="fg-sheet nd-paper">
        {query.error && <ErrorBox error={query.error} />}
        {/* 与线路、机器的草稿条同位：紧贴顶栏下方，不占纸面顶部。 */}
        <DraftSlot>
          <CvmPendingChanges workspace={data} network={network} />
        </DraftSlot>
        {denied ? (
          <div className="nd-paper-body">
            <EmptyState icon="container" title="当前账号无权打开此页面">
              <button className="btn" onClick={() => navigate('cvm', {})}>
                查看容器清单
              </button>
            </EmptyState>
          </div>
        ) : item ? (
          <InstanceDetail
            key={item.desired.id}
            instance={item}
            workspace={data}
            initialTab={initialTab}
            onTabChange={tab =>
              navigate('cvm', { p: 'instance', id: item.desired.id, ...(tab !== 'overview' ? { tab } : {}) })
            }
            networkDraftCount={network.draft ? editCount(network.draft.request) : 0}
            networkEditor={<ManagedNetworking instance={item} workspace={data} draft={network} />}
          />
        ) : publication ? (
          <NetworkPublicationEditor
            key={drill?.id ?? drill?.host ?? 'new'}
            workspace={data}
            id={drill?.id}
            initialNode={drill?.host}
            onBack={viewNetwork}
          />
        ) : (
          <>
            <CvmPageHeader
              title={title}
              meta={
                page === 'new'
                  ? '保存后进入顶部待发布条，发布生效后开始创建'
                  : page === 'adopt'
                    ? '整机独占，解除纳管时保留系统与磁盘'
                    : undefined
              }
              tabs={
                root &&
                visibleTabs.length > 1 && (
                  <CvmTabs
                    label="容器页签"
                    value={active}
                    items={visibleTabs}
                    onChange={tab => navigate('cvm', tab === 'instances' ? {} : { p: tab })}
                    panelId={id}
                  />
                )
              }
              tools={
                root && active === 'instances' && manager ? (
                  <TitleActions
                    primary={
                      <button
                        className="btn primary"
                        disabled={!data.hosts.some(supportsCreate)}
                        onClick={() => navigate('cvm', { p: 'new' })}
                      >
                        <Icon of="plus" size={13} />
                        创建容器
                      </button>
                    }
                  >
                    {networkManager && (
                      <button className="btn" onClick={() => navigate('cvm', { p: 'adopt' })}>
                        纳管裸机
                      </button>
                    )}
                  </TitleActions>
                ) : page === 'new' || page === 'adopt' ? (
                  <TitleActions>
                    <button className="btn" onClick={() => navigate('cvm', page === 'adopt' ? { p: 'hosts' } : {})}>
                      取消
                    </button>
                  </TitleActions>
                ) : root && active === 'hosts' ? (
                  <TitleActions primary={<AddHost workspace={data} />}>
                    {networkManager && (
                      <button className="btn" onClick={() => navigate('cvm', { p: 'adopt' })}>
                        纳管裸机
                      </button>
                    )}
                  </TitleActions>
                ) : undefined
              }
            />
            <div
              className="nd-paper-body"
              id={id}
              role={root && visibleTabs.length > 1 ? 'tabpanel' : undefined}
              aria-label={
                root && visibleTabs.length > 1 ? visibleTabs.find(tab => tab.key === active)?.label : undefined
              }
            >
              {page === 'new' ? (
                <CreateForm
                  key={`${drill?.host ?? ''}:${drill?.copy ?? ''}`}
                  workspace={data}
                  initialHost={drill?.host}
                  initialCopy={drill?.copy}
                  onCreated={(id, template) => {
                    const created = client
                      .getQueryData<CvmWorkspace>(QUERY)
                      ?.instances.find(item => item.desired.id === id);
                    if (template && created) network.seed(created, template);
                    navigate('cvm', { p: 'instance', id, ...(template ? { tab: 'network' } : {}) });
                  }}
                />
              ) : page === 'adopt' ? (
                <AdoptForm workspace={data} onViewHosts={viewHosts} />
              ) : page === 'instance' ? (
                <EmptyState icon="container" title="没有可查看的容器">
                  实例不存在或不在当前账号的可见范围内。
                </EmptyState>
              ) : active === 'networks' ? (
                <div className="cvm-resource-stack">
                  <EntryPublications workspace={data} />
                  <NetworkPublications
                    workspace={data}
                    onCreate={() => navigate('cvm', { p: 'network-new' })}
                    onEdit={id => navigate('cvm', { p: 'network', id })}
                  />
                </div>
              ) : active === 'hosts' ? (
                <HostList workspace={data} onCreateOnHost={createOnHost} />
              ) : (
                <InstanceList
                  workspace={data}
                  view={listView}
                  onViewChange={setListView}
                  onCreateOnHost={createOnHost}
                />
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
