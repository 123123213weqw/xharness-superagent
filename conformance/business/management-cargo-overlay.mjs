// Exact product-only edges to already locked transports; no external refresh.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
const sha=b=>createHash('sha256').update(b).digest('hex');
const bytes=readFileSync(new URL('./management-cargo.overlay.json',import.meta.url));
assert.equal(sha(bytes),'220d37265e63d84b8ab5229a757ccd97959ddbd6be9bc2c224254020e413c385');
const overlay=JSON.parse(bytes);
export function auditedLockBeforeManagement(current){
 assert.equal(overlay.schema_version,1);assert.equal(overlay.path,'Cargo.lock');assert.equal(overlay.package,'xharness-superagent');
 assert.deepEqual(overlay.dependencies,['axum']);assert.equal(overlay.external_packages_versions_checksums_unchanged,true);
 assert.equal(sha(current),overlay.after_sha256,'undeclared Management lock change');
 const text=current.toString();const marker='[[package]]\nname = "xharness-superagent"\n';const start=text.indexOf(marker);
 assert.ok(start>=0&&text.indexOf(marker,start+1)===-1);const end=text.indexOf('\n[[package]]',start+marker.length);assert.ok(end>start);
 let section=text.slice(start,end);
 for(const dependency of overlay.dependencies){const line=` "${dependency}",\n`;assert.equal(section.split(line).length,2);section=section.replace(line,'');}
 const before=Buffer.from(text.slice(0,start)+section+text.slice(end));
 assert.equal(sha(before),overlay.base_sha256,'packages or previous audited layers changed');return before;
}
