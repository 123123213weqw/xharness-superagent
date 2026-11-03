// One declared direct edge to an already locked URL validator. This does not
// refresh dependencies or change the preceding audited lock layers.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
const bytes=readFileSync(new URL('./feishu-cargo.overlay.json',import.meta.url));
assert.equal(sha(bytes),'d9680d211583e4d4577f60944caee2f01d63a49d50b6ad791e6a684717542274');
const overlay=JSON.parse(bytes);
export function auditedLockBeforeFeishu(current){
 assert.equal(overlay.schema_version,1);assert.equal(overlay.path,'Cargo.lock');assert.equal(overlay.package,'xharness-superagent');
 assert.equal(overlay.dependency,'url');assert.equal(overlay.version,'2.5.8');assert.equal(overlay.external_packages_versions_checksums_unchanged,true);
 assert.equal(sha(current),overlay.after_sha256,'undeclared Feishu lock change');
 const text=current.toString();const marker='[[package]]\nname = "xharness-superagent"\n';const start=text.indexOf(marker);
 assert.ok(start>=0&&text.indexOf(marker,start+1)===-1);const end=text.indexOf('\n[[package]]',start+marker.length);assert.ok(end>start);
 const section=text.slice(start,end);const line=' "url",\n';assert.equal(section.split(line).length,2);
 const before=Buffer.from(text.slice(0,start)+section.replace(line,'')+text.slice(end));
 assert.equal(sha(before),overlay.base_sha256,'packages or earlier audited layers changed');return before;
}
