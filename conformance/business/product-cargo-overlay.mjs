// One local workspace edge, not an upstream import or external lock refresh.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const bytes = readFileSync(new URL('./product-cargo.overlay.json', import.meta.url));
assert.equal(sha(bytes), 'a9d3589f11bf05079e60842d549d869819db4a2016ba4a74e2bf24db86b21343');
const overlay = JSON.parse(bytes);
export function auditedLockBeforeProductDependency(current, expected) {
  assert.equal(overlay.path, 'Cargo.lock');
  assert.equal(overlay.package, 'xharness-superagent');
  assert.equal(overlay.dependency, 'xharness-process');
  assert.equal(overlay.external_packages_unchanged, true);
  assert.equal(overlay.addition, ' "xharness-process",\n');
  assert.equal(overlay.added_lines, 1);
  assert.equal(overlay.removed_lines, 0);
  assert.equal(overlay.base_sha256, expected);
  assert.equal(sha(current), overlay.after_sha256, 'undeclared product lock change');
  const needle = overlay.context_before + overlay.addition + overlay.context_after;
  const text = current.toString('utf8');
  const start = text.indexOf(needle);
  assert.ok(start >= 0 && text.indexOf(needle, start + 1) === -1);
  const original = Buffer.from(text.slice(0, start) + overlay.context_before + overlay.context_after + text.slice(start + needle.length));
  assert.equal(sha(original), expected, 'external packages or audited lock differ');
  return original;
}
