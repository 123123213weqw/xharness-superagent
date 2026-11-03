// Public Native message mapping compared with the unmodified pinned projection.
// No SDK/provider invocation, session creation, external services or store copy.
import {readFile,writeFile} from 'node:fs/promises';
import assert from 'node:assert/strict';
import {stripTypeScriptTypes} from 'node:module';
import {createHash} from 'node:crypto';
// context.ts's unrelated prompt imports require the full SDK graph. Execute
// only this standalone, byte-unchanged original function, with Node's own TS
// type erasure. No SDK stubs or rewritten assertions/branches are involved.
const source=await readFile(new URL('../reference/upstream/src/app/context.ts',import.meta.url),'utf8');
const original=source.slice(source.indexOf('export function transcriptEntry('));
assert.match(original,/^export function transcriptEntry\(event: SessionEvent\) \{[\s\S]*\n\}\s*$/);
const {transcriptEntry}=await import('data:text/javascript;base64,'+Buffer.from(stripTypeScriptTypes(original)).toString('base64'));
const text={type:'text',text:'你好 😀'};
const call={type:'tool-call',id:'canonical-call',name:'orchestrator_read',arguments:'{"resource":"ticket/own"'};
const cases=[
 {name:'user',reference:{type:'user/message',data:{role:'user',content:[text]}},native:{type:'user/message',data:{message:{role:'user',content:text.text,id:'hidden-delivery',provider_items:[{private:'provider-header'}]}}}},
 {name:'assistant-call-reasoning',reference:{type:'assistant/message',data:{message:{role:'assistant',content:[text,{type:'reasoning',text:'private-reasoning'},call]}}},native:{type:'assistant/message',data:{turn:1,step:1,message:{role:'assistant',content:text.text,reasoning:'private-reasoning',tool_calls:[{id:call.id,provider_call_id:'private-provider-call',index:0,name:call.name,arguments_json:call.arguments}],provider_items:[{private:'provider-header'}]}}}},
 {name:'reasoning-only',reference:{type:'assistant/message',data:{message:{role:'assistant',content:[{type:'reasoning',text:'private-reasoning'}]}}},native:{type:'assistant/message',data:{turn:1,step:1,message:{role:'assistant',content:'',reasoning:'private-reasoning'}}}},
 {name:'tool-result',reference:{type:'tool/result',data:{message:{role:'tool',content:[text]}}},native:{type:'tool/result',data:{turn:1,step:1,result:{call_id:call.id,outcome:'success',content:text.text,metadata:{private:'provider-header'}}}}},
 {name:'non-message',reference:{type:'step/start',data:{turn:1,step:1}},native:{type:'step/start',data:{turn:1,step:1}}},
].map(item=>({...item,expected:transcriptEntry(item.reference)??null}));
const output={reference_commit:'6337fc823ce20251c8de8085948165c81aaa701e',original_function_sha256:createHash('sha256').update(original).digest('hex'),scope:'Unchanged standalone transcriptEntry, Node type erasure only; explicit Native data mapping, not whole-module/SDK/full-product acceptance',cases};
const file=new URL('./transcript-oracle.json',import.meta.url);
if(process.argv.includes('--check'))assert.deepEqual(JSON.parse(await readFile(file,'utf8')),output);else await writeFile(file,JSON.stringify(output,null,2)+'\n');
console.log(`${cases.length} original public transcript projections verified.`);
