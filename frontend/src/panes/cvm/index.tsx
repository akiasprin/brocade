import './styles.css';
import { useQuery } from '@tanstack/react-query';
import { useId, useState } from 'react';
import { fetchCvm } from '../../api';
import { navigate } from '../../forge/route';
import { EmptyState, ErrorBox, Loading } from '../../ui/bits';
import { TitleActions } from '../../ui/title-actions';
import { useCrumb } from '../../wm/crumb';
import type { Win } from '../../wm/store';
import { CreateForm } from './create';
import { InstanceDetail } from './detail';
import { BoardPane } from './board';
import { HostList } from './hosts';
import { AddHost } from './host-policy';
import { AdoptHostAction } from './adopt';
import { QUERY, supportsCreate } from './queries';
import { QuotaList } from './quotas';
import { CvmPageHeader, CvmTabs } from './presentation';
import { NetworkPublications } from './network-publications';
import { EntryPublications } from './entry-publications';

type Tab = 'instances' | 'hosts' | 'networks' | 'quotas';
export function CvmPane({ win }: { win: Win; bare?: boolean }) {
  const query = useQuery({
    queryKey: QUERY,
    queryFn: fetchCvm,
    refetchInterval: state => (state.state.data?.instances.some(item => item.status !== 'applied') ? 5_000 : 30_000),
  });
  const storedDrill = win.data.drill as { p?: string; id?: string } | undefined;
  const drill = storedDrill?.p === 'list' ? undefined : storedDrill;
  const [tab, setTab] = useState<Tab>('instances');
  const id = useId();
  const item = query.data?.instances.find(instance => instance.desired.id === drill?.id);
  useCrumb(
    win,
    drill?.p === 'new'
      ? [{ label: '创建容器' }]
      : drill?.p === 'instance'
        ? [{ label: item?.desired.spec.name ?? '实例详情' }]
        : [],
  );
  if (query.isPending) return <Loading />;
  if (query.error && !query.data) return <ErrorBox error={query.error} />;
  const data = query.data;
  if (!data) return null;
  const tabs: { key: Tab; label: string }[] = [
    { key: 'instances', label: '组合台' },
    { key: 'hosts', label: '宿主' },
    { key: 'networks', label: '网络' },
    ...(data.can_manage_quotas ? [{ key: 'quotas' as const, label: '额度' }] : []),
  ];
  const active = tabs.some(item => item.key === tab) ? tab : 'instances';
  return (
    <div className="cvm-page nd-page nd-sheet node-cardpage cvm-design">
      <div className="fg-sheet nd-paper">
        {query.error && <ErrorBox error={query.error} />}
        {data.pending_revision_id != null && !drill?.p && (
          <div className="cgr-notice cvm-release-notice" role="status">
            <span>容器与网络变更已保存至全局 R{data.pending_revision_id}，等待发布。</span>
            <button className="btn" disabled={!data.can_publish} onClick={() => navigate('deploy', { p: 'plan' })}>
              查看混合发布计划
            </button>
          </div>
        )}
        {drill?.p === 'instance' && item ? (
          <InstanceDetail key={item.desired.id} instance={item} workspace={data} />
        ) : (
          <>
            <CvmPageHeader
              title={drill?.p === 'new' ? '创建容器' : '容器'}
              tabs={
                !drill?.p && <CvmTabs label="容器页签" value={active} items={tabs} onChange={setTab} panelId={id} />
              }
              tools={
                !drill?.p && active === 'instances' && data.can_write ? (
                  <TitleActions
                    primary={
                      <button
                        className="btn primary"
                        disabled={!data.hosts.some(supportsCreate)}
                        onClick={() => navigate('cvm', { p: 'new' })}
                      >
                        ＋ 创建容器
                      </button>
                    }
                  >
                    <AdoptHostAction workspace={data} onViewHosts={() => setTab('hosts')} />
                  </TitleActions>
                ) : !drill?.p && active === 'hosts' ? (
                  <TitleActions primary={<AddHost workspace={data} />}>
                    <AdoptHostAction workspace={data} onViewHosts={() => setTab('hosts')} />
                  </TitleActions>
                ) : undefined
              }
            />
            <div
              className="nd-paper-body"
              id={id}
              role={!drill?.p ? 'tabpanel' : undefined}
              aria-label={!drill?.p ? tabs.find(t => t.key === active)?.label : undefined}
            >
              {drill?.p === 'new' ? (
                data.can_write ? (
                  <CreateForm workspace={data} />
                ) : (
                  <EmptyState icon="servers" title="当前账号无权创建容器">
                    当前账号只能查看实例。
                  </EmptyState>
                )
              ) : drill?.p === 'instance' ? (
                <EmptyState icon="servers" title="没有可查看的容器">
                  实例不存在或不在当前账号的可见范围内。
                </EmptyState>
              ) : active === 'networks' ? (
                // 与组合台的位置一致：入口地址在左，出口网络在右。
                <div className="cvm-network-lanes">
                  <EntryPublications workspace={data} />
                  <NetworkPublications workspace={data} />
                </div>
              ) : active === 'hosts' ? (
                <HostList workspace={data} />
              ) : active === 'quotas' ? (
                <QuotaList workspace={data} />
              ) : (
                <BoardPane workspace={data} />
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
