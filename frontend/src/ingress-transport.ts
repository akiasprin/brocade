import type { IngressProjection, TransportKind, XhttpMode } from './api';

const isXhttp = (kind: TransportKind) => kind === 'vless-reality-xhttp' || kind === 'vless-tls-xhttp';

export type IngressSecurity = 'reality' | 'tls';

export function transportKindFor(security: IngressSecurity, xhttp: boolean): TransportKind {
  if (security === 'tls') return xhttp ? 'vless-tls-xhttp' : 'vless-tls';
  return xhttp ? 'vless-reality-xhttp' : 'vless-reality';
}

/** Keep public download routing across transport changes, while retaining an origin listener only
 * for the one shape that actually creates it. Leaving XHTTP removes an unsupported download. */
export function projectionForTransport(projection: IngressProjection, kind: TransportKind): IngressProjection {
  return Object.fromEntries(
    (['v4', 'v6'] as const).flatMap(family => {
      const endpoint = projection[family];
      if (!endpoint) return [];
      const download = isXhttp(kind)
        ? endpoint.download
          ? {
              ...endpoint.download,
              origin_port: kind === 'vless-reality-xhttp' ? endpoint.download.origin_port : null,
            }
          : null
        : null;
      return [[family, { host: endpoint.host, port: endpoint.port, download }]];
    }),
  );
}

/** Xray rejects downloadSettings with stream-one at config-build time. */
export function compatibleXhttpMode(mode: XhttpMode, hasDownload: boolean): XhttpMode {
  return hasDownload && mode === 'stream-one' ? 'stream-up' : mode;
}
