// Additive existing-workspace product edges, not an external lock refresh.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const bytes = readFileSync(new URL('./product-cargo.overlay.json', import.meta.url));
assert.equal(sha(bytes), '65fbfaffd0d3ea3a21ad4071542155999afdb1115b1fde95c2ad67ec1d6097b1');
const overlay = JSON.parse(bytes);
export function auditedLockBeforeProductDependency(current, expected) {
  assert.equal(overlay.path, 'Cargo.lock');
  assert.equal(overlay.package, 'xharness-superagent');
  assert.equal(overlay.schema_version, 2);
  assert.deepEqual(overlay.dependencies, ['xharness-coding-tools', 'xharness-platform', 'xharness-process', 'xharness-provider-openai', 'xharness-web']);
  assert.equal(overlay.external_packages_unchanged, true);
  assert.equal(overlay.added_lines, 5);
  assert.equal(overlay.removed_lines, 0);
  assert.equal(overlay.base_sha256, expected);
  assert.equal(sha(current), overlay.after_sha256, 'undeclared product lock change');
  const text = current.toString('utf8');
  const packageStart = '[[package]]\nname = "xharness-superagent"\n';
  const start = text.indexOf(packageStart);
  assert.ok(start >= 0 && text.indexOf(packageStart, start + 1) === -1);
  const end = text.indexOf('\n[[package]]', start + packageStart.length);
  assert.ok(end > start);
  let section = text.slice(start, end);
  for (const dependency of overlay.dependencies) {
    const line = ` "${dependency}",\n`;
    assert.equal(section.split(line).length, 2, `exactly one product-only ${dependency} edge`);
    section = section.replace(line, '');
  }
  const original = Buffer.from(text.slice(0, start) + section + text.slice(end));
  assert.equal(sha(original), expected, 'external packages or audited lock differ');
  return original;
}
