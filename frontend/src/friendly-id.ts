export type FriendlyIdKind = 'chain' | 'ingress';

const PREFIX: Record<FriendlyIdKind, string> = {
  chain: 'c-',
  ingress: 'i-',
};

// Alternating consonants and vowels makes the opaque identifier easy to scan and say aloud
// without turning it into a user-facing name. The alphabet deliberately omits q/x/y and consonant
// clusters: `c-lumira` is much calmer in a URL or log line than a compact base-36 token.
const CONSONANTS = 'bcdfghjklmnprstvwz';
const VOWELS = 'aeiou';
const BODY_LENGTH = 6;

const bodyFromBytes = (bytes: Uint8Array): string => {
  if (bytes.length < BODY_LENGTH) throw new Error(`friendly id needs ${BODY_LENGTH} random bytes`);
  let body = '';
  for (let i = 0; i < BODY_LENGTH; i += 1) {
    const alphabet = i % 2 === 0 ? CONSONANTS : VOWELS;
    body += alphabet[bytes[i] % alphabet.length];
  }
  return body;
};

export const friendlyIdFromBytes = (kind: FriendlyIdKind, bytes: Uint8Array): string =>
  `${PREFIX[kind]}${bodyFromBytes(bytes)}`;

export const isFriendlyId = (kind: FriendlyIdKind, value: string): boolean => {
  if (!value.startsWith(PREFIX[kind]) || value.length !== PREFIX[kind].length + BODY_LENGTH) return false;
  const body = value.slice(PREFIX[kind].length);
  return [...body].every((char, i) => (i % 2 === 0 ? CONSONANTS : VOWELS).includes(char));
};

/** Generate a short, pronounceable technical id, retrying local snapshot collisions. */
export function friendlyId(kind: FriendlyIdKind, used: ReadonlySet<string> = new Set()): string {
  const crypto = globalThis.crypto;
  if (!crypto?.getRandomValues) throw new Error('浏览器不支持安全随机数，无法生成 ID');
  // The body is globally unique across both resource kinds. Accept either complete IDs or bare
  // bodies so callers cannot accidentally make collision handling prefix-local.
  const usedBodies = new Set([...used].map(value => (/^[ci]-/.test(value) ? value.slice(2) : value)));
  for (let attempt = 0; attempt < 32; attempt += 1) {
    const id = friendlyIdFromBytes(kind, crypto.getRandomValues(new Uint8Array(BODY_LENGTH)));
    if (!usedBodies.has(id.slice(2))) return id;
  }
  throw new Error('无法生成不重复的 ID，请重试');
}
