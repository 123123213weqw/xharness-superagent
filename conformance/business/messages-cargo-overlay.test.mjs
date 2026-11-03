import {auditedLockBeforeManagement} from './management-cargo-overlay.mjs';
import assert from 'node:assert/strict';
import {test} from 'node:test';
import {readFileSync} from 'node:fs';
import {auditedLockBeforeMessages} from './messages-cargo-overlay.mjs';
const current=auditedLockBeforeManagement(readFileSync(new URL('../../Cargo.lock',import.meta.url)));
test('Messages direct transport edges reconstruct the exact previous lock',()=>{
 const before=auditedLockBeforeMessages(current);assert.equal(current.length-before.length,Buffer.byteLength(' "async-stream",\n "reqwest",\n'));
 assert.equal((current.toString().match(/^\[\[package\]\]$/gm)??[]).length,(before.toString().match(/^\[\[package\]\]$/gm)??[]).length);
});
test('Messages overlay rejects package, edge and unrelated byte mutations',()=>{
 const s=current.toString();for(const edited of [s+'\n',s.replace('name = "reqwest"','name = "other-transport"'),s.replace('name = "xharness-superagent"','name = "other-product"')]){
  assert.notEqual(edited,s);assert.throws(()=>auditedLockBeforeMessages(Buffer.from(edited)));
 }
});
