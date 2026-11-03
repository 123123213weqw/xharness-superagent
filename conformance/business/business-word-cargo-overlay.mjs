// Separate, byte-exact business dependency layer. Never relax the audited
// workspace-only overlay or normalize arbitrary lockfile changes away.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const bytes = readFileSync(new URL('./business-word-cargo.overlay.json', import.meta.url));
assert.equal(sha(bytes), '3196ffea6333659e1ff3db6ebc2238a8d34c51ded2e6a9c09efee488f8a73810');
const overlay = JSON.parse(bytes);
export function auditedLockBeforeBusinessWords(current) {
  assert.equal(overlay.schema_version, 1);
  assert.equal(overlay.path, 'Cargo.lock');
  assert.equal(overlay.direct_dependency, 'icu_segmenter = "=2.3.0"');
  assert.equal(overlay.existing_versions_sources_checksums_unchanged, true);
  assert.equal(overlay.changes.length, 13);
  assert.equal(sha(current), overlay.after_sha256, 'undeclared business dependency lock change');
  let text = current.toString('utf8');
  for (const change of overlay.changes) {
    const marker = `[[package]]\nname = "${change.name}"\nversion = "${change.version}"\n`;
    assert.ok(change.after.startsWith(marker));
    assert.equal(text.split(change.after).length, 2, `unique exact section: ${change.name}`);
    if (change.before !== null) {
      assert.ok(change.before.startsWith(marker), 'existing package identity changed');
      for (const field of ['source', 'checksum']) {
        const value = section => section.match(new RegExp(`^${field} = .*$`, 'm'))?.[0];
        assert.equal(value(change.before), value(change.after));
      }
    }
    text = text.replace(change.after, change.before ?? '');
  }
  const before = Buffer.from(text);
  assert.equal(sha(before), overlay.base_sha256, 'pre-business dependency bytes changed');
  return before;
}
