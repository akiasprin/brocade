import type { CvmContainerRoute, CvmRouteTarget } from '../../api';

export const targetKey = (target: CvmRouteTarget): string =>
  target.chain_id ? `chain:${target.chain_id}` : (target.interface_id ?? '');
export const routeTarget = (key: string): CvmRouteTarget =>
  key.startsWith('chain:') ? { chain_id: key.slice(6) } : { interface_id: key };
/** Replacing the discriminated target must remove the previous target field. */
export const retarget = (route: CvmContainerRoute, key: string): CvmContainerRoute => ({
  destination: route.destination,
  ...(route.service ? { service: route.service } : {}),
  ...routeTarget(key),
});
