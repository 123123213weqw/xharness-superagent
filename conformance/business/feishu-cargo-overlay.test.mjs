import assert from 'node:assert/strict';
import {test} from 'node:test';
import {readFileSync} from 'node:fs';
import {auditedLockBeforeFeishu} from './feishu-cargo-overlay.mjs';
import {auditedLockBeforeMessages} from './messages-cargo-overlay.mjs';
const current=auditedLockBeforeMessages(readFileSync(new URL('../../Cargo.lock',import.meta.url)));
test('one direct url edge reconstructs the exact preceding business lock',()=>{
 const before=auditedLockBeforeFeishu(current);assert.equal(current.length-before.length,Buffer.byteLength(' "url",\n'));
 assert.equal((current.toString().match(/^\[\[package\]\]$/gm)??[]).length,(before.toString().match(/^\[\[package\]\]$/gm)??[]).length);
});
test('appended bytes, package version and dependency mutations fail closed',()=>{
 const source=current.toString();for(const edited of [source+'\n',source.replace('name = "url"\nversion = "2.5.8"','name = "url"\nversion = "2.5.9"'),source.replace('name = "xharness-superagent"','name = "different-product"')]){
  assert.notEqual(edited,source);assert.throws(()=>auditedLockBeforeFeishu(Buffer.from(edited)));
 }
});
