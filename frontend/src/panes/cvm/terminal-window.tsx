import { useEffect } from 'react';
import { useQuery } from '@tanstack/react-query';
import { ApiError, fetchCvm } from '../../api';
import { ErrorBox, Loading } from '../../ui/bits';
import { QUERY, guestTerminalAccess } from './queries';
import CvmTerminal from './terminal';
import './styles.css';

export default function TerminalWindow({ id }: { id: string }) {
  const query = useQuery({ queryKey: QUERY, queryFn: fetchCvm, refetchInterval: 20_000 });
  useEffect(() => {
    window.opener = null;
  }, []);
  if (query.isPending) return <Loading />;
  if (query.error && (!query.data || (query.error instanceof ApiError && [401, 403].includes(query.error.status))))
    return <ErrorBox error={query.error} />;
  const workspace = query.data;
  const instance = workspace?.instances.find(i => i.desired.id === id);
  if (!workspace || !instance) return <p role="alert">实例不存在或无权访问。</p>;
  const host = workspace.hosts.find(h => h.node_id === instance.node_id);
  const access = guestTerminalAccess(instance, host, workspace.can_use);
  if (!access.enabled) return <p role="alert">{access.reason}</p>;
  return (
    <CvmTerminal
      id={id}
      name={instance.desired.spec.name}
      hostName={host?.name ?? instance.node_id}
      warning={query.error ? '实例状态刷新失败，下次刷新时重试。' : undefined}
      onClose={() => {
        window.close();
        if (!window.closed) {
          const url = new URL(location.href);
          url.searchParams.delete('webshell');
          url.hash = `#/cvm/instance/${encodeURIComponent(id)}`;
          location.assign(url);
        }
      }}
    />
  );
}
