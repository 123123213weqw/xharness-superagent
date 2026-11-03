// Exact product-only edges to already locked transports; no external refresh.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
const sha=b=>createHash('sha256').update(b).digest('hex');
const bytes=readFileSync(new URL('./messages-cargo.overlay.json',import.meta.url));
assert.equal(sha(bytes),'623163e880bd1fe69f09c7972c56b17466c33793346e4b401f21c728b96a0387');
const overlay=JSON.parse(bytes);
export function auditedLockBeforeMessages(current){
 assert.equal(overlay.schema_version,1);assert.equal(overlay.path,'Cargo.lock');assert.equal(overlay.package,'xharness-superagent');
 assert.deepEqual(overlay.dependencies,['async-stream','reqwest']);assert.equal(overlay.external_packages_versions_checksums_unchanged,true);
 assert.equal(sha(current),overlay.after_sha256,'undeclared Messages lock change');
 const text=current.toString();const marker='[[package]]\nname = "xharness-superagent"\n';const start=text.indexOf(marker);
 assert.ok(start>=0&&text.indexOf(marker,start+1)===-1);const end=text.indexOf('\n[[package]]',start+marker.length);assert.ok(end>start);
 let section=text.slice(start,end);
 for(const dependency of overlay.dependencies){const line=` "${dependency}",\n`;assert.equal(section.split(line).length,2);section=section.replace(line,'');}
 const before=Buffer.from(text.slice(0,start)+section+text.slice(end));
 assert.equal(sha(before),overlay.base_sha256,'packages or previous audited layers changed');return before;
}
