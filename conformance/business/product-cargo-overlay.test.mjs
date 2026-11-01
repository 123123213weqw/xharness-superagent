import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFileSync } from 'node:fs';
import { auditedLockBeforeProductDependency } from './product-cargo-overlay.mjs';
import { auditedLockBeforeBusinessWords } from './business-word-cargo-overlay.mjs';
const current = auditedLockBeforeBusinessWords(readFileSync(new URL('../../Cargo.lock', import.meta.url)));
const overlay = JSON.parse(readFileSync(new URL('./product-cargo.overlay.json', import.meta.url)));
test('exact product workspace edge reconstructs audited lock', () => {
  const before = auditedLockBeforeProductDependency(current, overlay.base_sha256);
  assert.equal(current.length - before.length, overlay.dependencies.reduce((size, dependency) => size + Buffer.byteLength(` "${dependency}",\n`), 0));
});
test('unrelated lock edits, wrong base and changed dependency are rejected', () => {
  for (const changed of [Buffer.concat([current, Buffer.from('\n')]), Buffer.from(current.toString().replace(' "xharness-provider-openai",\n', ' "different-dependency",\n'))]) {
    assert.notDeepEqual(changed, current);
    assert.throws(() => auditedLockBeforeProductDependency(changed, overlay.base_sha256));
  }
  assert.throws(() => auditedLockBeforeProductDependency(current, '0'.repeat(64)));
});
