import { describe, expect, it } from 'vitest';
import type { GroupCertificate } from '../src/api';
import { certificateSigningLabel } from '../src/panes/nodes';

const certificate = (signingMethod: GroupCertificate['signing_method'], issuer: string): GroupCertificate => ({
  id: 'certificate-1',
  status: 'serving',
  origin: 'spare',
  signing_method: signingMethod,
  issuer,
  issued_at: '2026-09-06T00:00:00Z',
  expires_at: '2126-09-06T00:00:00Z',
  sha256: '0'.repeat(64),
  attempts: 0,
  last_error: null,
  last_attempt_at: null,
});

describe('certificate signing label', () => {
  it('uses the frozen signing method instead of guessing from the issuer name', () => {
    expect(certificateSigningLabel(certificate('self-signed', 'Harbor Edge Root CA 1875A7F1'))).toBe('自签证书');
    expect(certificateSigningLabel(certificate('public-ca', 'Brocade Self-Signed'))).toBe("Let's Encrypt");
  });
});
