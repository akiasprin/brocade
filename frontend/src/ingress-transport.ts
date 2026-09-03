import type { IngressProjection, TransportKind, XhttpMode } from './api';

export type IngressSecurity = 'reality' | 'tls';

export function transportKindFor(security: IngressSecurity, xhttp: boolean): TransportKind {
  if (security === 'tls') return xhttp ? 'vless-tls-xhttp' : 'vless-tls';
  return xhttp ? 'vless-reality-xhttp' : 'vless-reality';
}

/** Keep the public address projection stable across transport changes. Independent XHTTP download
 * settings belong to the XHTTP transport and are handled by that editor. */
export function projectionForTransport(projection: IngressProjection, _kind: TransportKind): IngressProjection {
  return Object.fromEntries(
    (['v4', 'v6'] as const).flatMap(family => {
      const endpoint = projection[family];
      if (!endpoint) return [];
      return [[family, { host: endpoint.host, port: endpoint.port }]];
    }),
  );
}

/** Xray rejects downloadSettings with stream-one at config-build time. */
export function compatibleXhttpMode(mode: XhttpMode, hasDownload: boolean): XhttpMode {
  return hasDownload && mode === 'stream-one' ? 'stream-up' : mode;
}
