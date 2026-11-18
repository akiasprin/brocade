import type { CvmHost, CvmNicCatalog } from '../../api';

/** Display-only names never imply access to a provider's management capabilities. */
export function networkProviderName(catalog: CvmNicCatalog | undefined, node: string, hosts: CvmHost[] = []) {
  const scoped = catalog?.provider_names;
  return (
    (scoped && Object.hasOwn(scoped, node) ? scoped[node] : undefined) ??
    catalog?.providers.find(provider => provider.node_id === node)?.name ??
    hosts.find(host => host.node_id === node)?.name ??
    node
  );
}
