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
