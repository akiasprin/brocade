export type ModelIdKind = 'chain' | 'ingress';

export interface ModelIdPair {
  ingressId: string;
  chainId: string;
}

const RANDOM_BYTES = 4;
const INGRESS_ID = /^ing-([0-9a-f]{4})$/;
const CHAIN_ID = /^chn-([0-9a-f]{4})-([0-9a-f]{4})$/;

const hex = (bytes: Uint8Array, start: number): string =>
  [...bytes.slice(start, start + 2)].map(byte => byte.toString(16).padStart(2, '0')).join('');

export const modelIdPairFromBytes = (bytes: Uint8Array): ModelIdPair => {
  if (bytes.length < RANDOM_BYTES) throw new Error(`model id pair needs ${RANDOM_BYTES} random bytes`);
  const ingressToken = hex(bytes, 0);
  return {
    ingressId: `ing-${ingressToken}`,
    chainId: `chn-${ingressToken}-${hex(bytes, 2)}`,
  };
};

export const isModelId = (kind: ModelIdKind, value: string): boolean =>
  (kind === 'ingress' ? INGRESS_ID : CHAIN_ID).test(value);

export const modelIdsArePaired = ({ ingressId, chainId }: ModelIdPair): boolean => {
  const ingressToken = INGRESS_ID.exec(ingressId)?.[1];
  const chainIngressToken = CHAIN_ID.exec(chainId)?.[1];
  return ingressToken !== undefined && ingressToken === chainIngressToken;
};

const ingressTokenOf = (value: string): string | null =>
  INGRESS_ID.exec(value)?.[1] ?? CHAIN_ID.exec(value)?.[1] ?? null;

/** Generate one ingress id and its first chain id as a related pair. */
export function modelIdPair(used: ReadonlySet<string> = new Set()): ModelIdPair {
  const crypto = globalThis.crypto;
  if (!crypto?.getRandomValues) throw new Error('浏览器不支持安全随机数，无法生成 ID');
  const usedIngressTokens = new Set([...used].map(ingressTokenOf).filter(token => token !== null));
  for (let attempt = 0; attempt < 32; attempt += 1) {
    const pair = modelIdPairFromBytes(crypto.getRandomValues(new Uint8Array(RANDOM_BYTES)));
    const token = ingressTokenOf(pair.ingressId);
    if (token !== null && !usedIngressTokens.has(token) && !used.has(pair.chainId)) return pair;
  }
  throw new Error('无法生成不重复的 ID，请重试');
}
