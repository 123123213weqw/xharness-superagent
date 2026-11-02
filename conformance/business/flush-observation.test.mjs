import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {test} from 'node:test';
import {auditedHostTestBeforeFlushObservation} from './flush-observation.mjs';
const current=readFileSync(new URL('../../crates/xharness-host/tests/observed_input.rs',import.meta.url));
const overlay=JSON.parse(readFileSync(new URL('./flush-observation.overlay.json',import.meta.url)));
test('bounded observation fence reconstructs exact imported test and every original assertion',()=>{
 const original=auditedHostTestBeforeFlushObservation(current,overlay.base_sha256);
 assert.equal(createHash('sha256').update(original).digest('hex'),overlay.base_sha256);
 assert.deepEqual(current.toString().match(/^.*assert[^\n]*$/gm),original.toString().match(/^.*assert[^\n]*$/gm));
 assert.equal(current.toString().split(overlay.addition).length,2);
});
test('weakened original assertions, changed wait and unrelated edits fail closed',()=>{
 for(const edited of [current.toString()+'\n',current.toString().replace('> before); // not a global','>= before); // not a global'),current.toString().replace('<= before {','< before {'),current.toString().replace('assert_eq!(p.calls.load(Ordering::SeqCst), 2);','assert_eq!(p.calls.load(Ordering::SeqCst), 3);')]){
  assert.notEqual(edited,current.toString());assert.throws(()=>auditedHostTestBeforeFlushObservation(Buffer.from(edited),overlay.base_sha256));
 }
});
test('wrong import base and missing observation fence fail closed',()=>{
 assert.throws(()=>auditedHostTestBeforeFlushObservation(current,'0'.repeat(64)));
 const original=auditedHostTestBeforeFlushObservation(current,overlay.base_sha256);
 assert.throws(()=>auditedHostTestBeforeFlushObservation(original,overlay.base_sha256));
});
