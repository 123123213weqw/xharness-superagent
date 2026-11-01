import {auditedLockBeforeFeishu} from './feishu-cargo-overlay.mjs';
import assert from 'node:assert/strict';
import {test} from 'node:test';
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {auditedLockBeforeBusinessWords} from './business-word-cargo-overlay.mjs';
const current = auditedLockBeforeFeishu(readFileSync(new URL('../../Cargo.lock', import.meta.url)));
test('declared six new packages and seven feature-edge sections reconstruct prior lock exactly', () => {
  const before = auditedLockBeforeBusinessWords(current);
  assert.equal(createHash('sha256').update(before).digest('hex'), '6de2b3f070b89320e17f75adf88f2bb4ca2c7e3d6ce6e96a3d844d89f63af359');
  assert.equal((current.toString().match(/^\[\[package\]\]$/gm) ?? []).length - (before.toString().match(/^\[\[package\]\]$/gm) ?? []).length, 6);
});
test('unrelated append, changed version, checksum, dependency edge and missing package fail closed', () => {
  const source = current.toString();
  for (const edited of [source+'\n', source.replace('name = "libm"\nversion = "0.2.16"', 'name = "libm"\nversion = "0.2.15"'), source.replace('77745e017f5edba1', '0000000000000000'), source.replace(' "icu_segmenter",\n', ''), source.replace('name = "libm"', 'name = "missing-libm"')]) {
    assert.notEqual(edited, source);
    assert.throws(() => auditedLockBeforeBusinessWords(Buffer.from(edited)));
  }
});
