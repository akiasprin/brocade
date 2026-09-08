import assert from 'node:assert/strict';
import test from 'node:test';
import { externalImportCanSave, serverNameAfterAddressChange } from '../src/external-outbound.ts';

test('an untouched server name follows the complete address instead of freezing on its first character', () => {
  let address = '';
  let serverName = '';
  for (const nextAddress of ['d', 'do', 'dow', 'download.example.com']) {
    serverName = serverNameAfterAddressChange(address, serverName, nextAddress);
    address = nextAddress;
  }
  assert.equal(serverName, 'download.example.com');
});

test('a manually overridden server name stops following address changes', () => {
  assert.equal(
    serverNameAfterAddressChange('edge.example.com', 'certificate.example.net', 'new-edge.example.com'),
    'certificate.example.net',
  );
});

test('import mode can save only the currently parsed link', () => {
  assert.equal(externalImportCanSave('import', true), true);
  assert.equal(externalImportCanSave('import', false), false);
  assert.equal(externalImportCanSave('manual', false), true);
});

test('VLESS Encryption validates complete keys, modes, padding and rejects incomplete values', async () => {
  const { vlessEncryptionIsValid: valid } = await import('../src/external-outbound.ts');
  const key = 'A'.repeat(43);
  assert.equal(valid('none'), true);
  for (const mode of ['native', 'xorpub', 'random']) {
    for (const rtt of ['0rtt', '1rtt']) assert.equal(valid(`mlkem768x25519plus.${mode}.${rtt}.${key}`), true);
  }
  assert.equal(valid(`mlkem768x25519plus.native.0rtt.100-111-1111.75-0-111.${key}`), true);
  for (const value of ['', 'garbage', 'mlkem768x25519plus.native.1rtt.', `mlkem768x25519plus.native.600s.${key}`, `mlkem768x25519plus.native.1rtt.${key}.100-1-1`]) assert.equal(valid(value), false);
});
