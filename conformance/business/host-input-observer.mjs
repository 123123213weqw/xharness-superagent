// Reconstruct the exact audited baseline before the existing 86-path check.
// This explicitly declared local product addition is NOT a canonical import.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const bytes = readFileSync(new URL('./host-input-observer.overlay.json', import.meta.url));
assert.equal(sha(bytes), 'dc94b6e594628da3bbfd03cf88253ef2be4d94f2ddac83846adb6b37972ae7af', 'product overlay manifest changed');
const overlay = JSON.parse(bytes);
export function auditedHostBeforeProductObserver(current, expectedBaseHash) {
  assert.equal(overlay.schema_version, 1);
  assert.equal(overlay.path, 'crates/xharness-host/src/runtime.rs');
  assert.equal(overlay.kind, 'product_local_overlay_not_canonical_import');
  assert.equal(overlay.added_lines, 17);
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
