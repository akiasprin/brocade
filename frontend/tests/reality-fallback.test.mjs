import assert from 'node:assert/strict';
import test from 'node:test';
import { fallbackLimitDraft, fallbackLimitsFromDraft } from '../src/reality-fallback.ts';

test('named presets seed their documented values without becoming custom', () => {
  const balanced = fallbackLimitDraft({ mode: 'balanced' });
  assert.equal(balanced.mode, 'balanced');
  assert.deepEqual(balanced.upload, {
    afterBytes: '1048576',
    bytesPerSec: '262144',
    burstBytesPerSec: '524288',
  });

  const strict = fallbackLimitDraft({ mode: 'strict' });
  assert.equal(strict.mode, 'strict');
  assert.equal(strict.download.bytesPerSec, '262144');
});

test('custom fallback limits round-trip every upload and download field', () => {
  const policy = {
    mode: 'custom',
    upload: { after_bytes: 12, bytes_per_sec: 34, burst_bytes_per_sec: 56 },
    download: { after_bytes: 78, bytes_per_sec: 90, burst_bytes_per_sec: 123 },
  };
  assert.deepEqual(fallbackLimitsFromDraft(fallbackLimitDraft(policy)), policy);
});

test('custom limits reject zero rates, smaller bursts, fractions and unsafe integers', () => {
  const base = fallbackLimitDraft({ mode: 'balanced' });
  base.mode = 'custom';

  assert.equal(fallbackLimitsFromDraft({ ...base, upload: { ...base.upload, bytesPerSec: '0' } }), null);
  assert.equal(fallbackLimitsFromDraft({ ...base, download: { ...base.download, burstBytesPerSec: '1' } }), null);
  assert.equal(fallbackLimitsFromDraft({ ...base, upload: { ...base.upload, afterBytes: '1.5' } }), null);
  assert.equal(fallbackLimitsFromDraft({ ...base, upload: { ...base.upload, afterBytes: '9007199254740992' } }), null);
});

test('off, balanced and strict remain named policies', () => {
  for (const mode of ['off', 'balanced', 'strict']) {
    assert.deepEqual(fallbackLimitsFromDraft(fallbackLimitDraft({ mode })), { mode });
  }
});
