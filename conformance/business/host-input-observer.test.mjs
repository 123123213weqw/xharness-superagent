import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import { auditedHostBeforeProductObserver } from './host-input-observer.mjs';
const current = readFileSync(new URL('../../crates/xharness-host/src/runtime.rs', import.meta.url));
const overlay = JSON.parse(readFileSync(new URL('./host-input-observer.overlay.json', import.meta.url)));
test('local addition reconstructs the exact frozen audited Host bytes', () => {
  const original = auditedHostBeforeProductObserver(current, overlay.base_sha256);
  assert.equal(createHash('sha256').update(original).digest('hex'), overlay.base_sha256);
  assert.equal(original.includes('pub fn observe_prepared_input('), false);
});
test('undeclared modification outside the local addition is rejected', () => {
  assert.throws(() => auditedHostBeforeProductObserver(Buffer.concat([current, Buffer.from('\n')]), overlay.base_sha256));
});
test('modification inside the local addition is rejected', () => {
  const changed = Buffer.from(current.toString().replace('self.require_execution()?;\n        Ok(self.running_from_prepared(PreparedDurableTurn {', 'Ok(self.running_from_prepared(PreparedDurableTurn {'));
  assert.notDeepEqual(changed, current);
  assert.throws(() => auditedHostBeforeProductObserver(changed, overlay.base_sha256));
});
test('wrong import-chain base and unpatched baseline are rejected', () => {
  assert.throws(() => auditedHostBeforeProductObserver(current, '0'.repeat(64)));
  const original = auditedHostBeforeProductObserver(current, overlay.base_sha256);
  assert.throws(() => auditedHostBeforeProductObserver(original, overlay.base_sha256));
});
