// Reconstruct the exact audited baseline before the existing 86-path check.
// This explicitly declared test observation fence is NOT a canonical import.
// It does not remove or change an original assertion or any Runtime code.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const bytes = readFileSync(new URL('./flush-observation.overlay.json', import.meta.url));
assert.equal(sha(bytes), '3be22f620d71279be82ecf90356a501c288c44122435f4d1a18c311cc6ecc147', 'product overlay manifest changed');
const overlay = JSON.parse(bytes);
export function auditedHostTestBeforeFlushObservation(current, expectedBaseHash) {
  assert.equal(overlay.schema_version, 1);
  assert.equal(overlay.path, 'crates/xharness-host/tests/observed_input.rs');
  assert.equal(overlay.kind, 'test_observation_fence_only_all_original_assertions_retained');
  assert.equal(overlay.added_lines, 9);
  assert.equal(overlay.removed_lines, 0);
  assert.equal(overlay.base_sha256, expectedBaseHash, 'product overlay base differs from audited import chain');
  assert.equal(sha(current), overlay.after_sha256, 'current product Host differs from declared overlay');
  const text = current.toString('utf8');
  const needle = overlay.context_before + overlay.addition + overlay.context_after;
  const start = text.indexOf(needle);
  assert.ok(start >= 0 && text.indexOf(needle, start + 1) === -1, 'product overlay anchor missing or ambiguous');
  const original = Buffer.from(text.slice(0, start) + overlay.context_before + overlay.context_after + text.slice(start + needle.length));
  assert.equal(sha(original), expectedBaseHash, 'product overlay does not reconstruct exact audited Host');
  return original;
}
