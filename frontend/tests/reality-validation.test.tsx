import { describe, expect, it } from 'vitest';
import {
  realityFingerprintIsValid,
  realityPublicKeyIsValid,
  realityShortIdIsValid,
} from '../src/reality';

describe('REALITY field validation', () => {
  it('accepts exactly a canonical 32-byte base64url public key', () => {
    expect(realityPublicKeyIsValid('AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA')).toBe(true);
    expect(realityPublicKeyIsValid('AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA')).toBe(false);
    expect(realityPublicKeyIsValid('AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA')).toBe(false);
    expect(realityPublicKeyIsValid('AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=')).toBe(false);
  });

  it('requires one to eight complete hexadecimal bytes for shortId', () => {
    for (const value of ['00', '7a3f41d9c02be611', 'ABCDEF']) {
      expect(realityShortIdIsValid(value), value).toBe(true);
    }
    for (const value of ['', '0', 'abc', '0123456789abcdef00', 'not-hex']) {
      expect(realityShortIdIsValid(value), value).toBe(false);
    }
  });

  it('accepts pinned Xray fingerprints while refusing unsafe and unknown values', () => {
    expect(realityFingerprintIsValid('chrome')).toBe(true);
    expect(realityFingerprintIsValid('randomized')).toBe(true);
    expect(realityFingerprintIsValid('hellochrome_120')).toBe(false);
    expect(realityFingerprintIsValid('unsafe')).toBe(false);
    expect(realityFingerprintIsValid('hellogolang')).toBe(false);
    expect(realityFingerprintIsValid('future-browser')).toBe(false);
  });
});
