// 「容器」首页：组合台。入口、出口与计算分别读取，各自刷新；任何一份读取失败都如实显示。
import { stackHosts } from '../queries';
import { useQuery } from '@tanstack/react-query';
import { useState } from 'react';
import { fetchCvmEntries, fetchCvmNicCatalog, fetchCvmChainCatalog, type CvmWorkspace } from '../../../api';
import { EmptyState, ErrorBox, Loading } from '../../../ui/bits';
import { useNarrow } from '../../../ui/viewport';
import { NIC_CATALOG } from '../network-common';
import { CompositionBoard } from './board';
import { ENTRY_BOARD } from './inspector';
import { exitTiles, ruleLinks } from './model';
import { PhoneBoard } from './phone';

export function BoardPane({ workspace }: { workspace: CvmWorkspace }) {
  const narrow = useNarrow();
  const entries = useQuery({ queryKey: ENTRY_BOARD, queryFn: fetchCvmEntries, refetchInterval: 15_000 });
  const catalog = useQuery({ queryKey: NIC_CATALOG, queryFn: fetchCvmNicCatalog, refetchInterval: 60_000 });
  const [selected, setSelected] = useState<string | null>(null);
  const chains = useQuery({
    queryKey: ['cvm-chain-egress', selected],
    queryFn: () => fetchCvmChainCatalog(selected ?? undefined),
    refetchInterval: 15_000,
  });
  if (entries.isPending) return <Loading />;
  if (!entries.data) return <ErrorBox error={entries.error} />;
  const instances = workspace.instances.filter(instance => instance.reserved && instance.desired.power !== 'deleted');
  if (!instances.length && !stackHosts(workspace).length)
    return (
      <EmptyState icon="servers" title="还没有可用的计算宿主">
        机器管理员接入 Cube 宿主并授权后，这里会出现入口、计算与出口。
      </EmptyState>
    );
  return (
    <>
      {entries.error && <ErrorBox error={entries.error} />}
      {catalog.error && <ErrorBox error={catalog.error} />}
      {chains.error && <ErrorBox error={chains.error} />}
      {narrow ? (
        <PhoneBoard
          chains={chains.data}
          workspace={workspace}
          instances={instances}
          entries={entries.data}
          exits={exitTiles(catalog.data, workspace.hosts)}
          rules={ruleLinks(instances)}
        />
      ) : (
        <CompositionBoard
          chains={chains.data}
          workspace={workspace}
          entries={entries.data}
          catalog={catalog.data}
          selected={selected}
          onSelect={setSelected}
        />
      )}
    </>
  );
}
