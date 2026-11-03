import assert from 'node:assert/strict';
import {test} from 'node:test';
import {readFileSync} from 'node:fs';
import {auditedLockBeforeManagement} from './management-cargo-overlay.mjs';
const current=readFileSync(new URL('../../Cargo.lock',import.meta.url));
test('Management direct transport edges reconstruct the exact previous lock',()=>{
 const before=auditedLockBeforeManagement(current);assert.equal(current.length-before.length,Buffer.byteLength(' "axum",\n'));
 assert.equal((current.toString().match(/^\[\[package\]\]$/gm)??[]).length,(before.toString().match(/^\[\[package\]\]$/gm)??[]).length);
});
test('Management overlay rejects package, edge and unrelated byte mutations',()=>{
 const s=current.toString();for(const edited of [s+'\n',s.replace('name = "axum"','name = "other-transport"'),s.replace('name = "xharness-superagent"','name = "other-product"')]){
  assert.notEqual(edited,s);assert.throws(()=>auditedLockBeforeManagement(Buffer.from(edited)));
 }
});
