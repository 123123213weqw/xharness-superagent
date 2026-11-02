// Unchanged original Feishu text/context/link functions through actual Commands
// and an explicitly offline CLI endpoint. This is not SDK/image/product approval.
import {mkdtemp,readFile,writeFile,chmod,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';import {join} from 'node:path';import assert from 'node:assert/strict';
import {Commands} from '../reference/upstream/src/infra/commands.ts';
import {readFeishu,feishuRead,readFeishuLink,feishuThreadContext,feishuResource,feishuView,feishuReferences} from '../reference/upstream/src/sources/feishu-read.ts';
const reference={triggerId:'accepted',chatId:'oc_chat',messageId:'om_source',at:'2026-10-31T00:00:00.123Z'};
const message=(id='om_source',extra={})=>({message_id:id,chat_id:'oc_chat',content:'Hello @_bot 😀',msg_type:'text',sender:{id:'ou_one',name:'名字',sender_type:'user'},create_time:'1793404800123',mentions:[{id:'ou_bot',key:'@_bot',name:'机器人'}],...extra});
const history=(id,extra={})=>({message_id:id,chat_id:'oc_chat',sender:{id:'ou_one',sender_type:'user'},create_time:'1793404800123',body:{content:'{"text":"History 😀"}'},...extra});
const ok=data=>({ok:true,data});
const base=()=>({source:ok({items:[{message_id:'om_source',chat_id:'oc_chat',root_id:'om_root',parent_id:'om_parent',thread_id:'omt_topic'}]}),message:ok({messages:[message(),message('om_root'),message('om_parent')]}),thread:ok({messages:[message('om_root'),message()],has_more:false,thread_id:'omt_topic'}),earlier:ok({items:[history('om_source'),history('om_before')],has_more:true,page_token:'old/中文 😀'}),later:ok({items:[history('om_source'),history('om_after')],has_more:false})});
const cases=[];
function add(name,mode,resource='feishu/accepted/message',edit=()=>{},ref=reference){const responses=base();edit(responses);cases.push({name,mode,resource,reference:ref,pageSize:3,order:'asc',responses});}
for(const section of ['message','root','parent','thread','around'])add(section,'context',`feishu/accepted/${section}`);
add('raw-message','raw');add('raw-thread-skips-invalid','raw','feishu/accepted/thread',r=>{r.thread.data.messages.push({message_id:'om_bad'});r.thread.data.has_more=true;r.thread.data.page_token='next/😀';});
add('thread-cursor','context','feishu/accepted/thread/next%2F%E4%B8%AD%E6%96%87');
add('wrong-trigger','context','feishu/other/message');add('invalid-resource','context','feishu/accepted/message/cursor');add('malformed-cursor','context','feishu/accepted/thread/%ZZ');
add('wrong-source-chat','context',undefined,r=>{r.source.data.items[0].chat_id='oc_other';});
add('source-deleted','context',undefined,r=>{r.source.data.items[0].deleted=true;});
add('duplicate-deleted-first','context',undefined,r=>{r.source.data.items.unshift({...r.source.data.items[0],deleted:true});});
add('source-missing','context',undefined,r=>{r.source.data.items=[];});
add('source-invalid-unrequested','context',undefined,r=>{r.source.data.items.push({message_id:'bad',chat_id:'oc_chat'});});
add('source-null-thread','context',undefined,r=>{r.source.data.items[0].thread_id=null;});
add('missing-root','raw','feishu/accepted/root',r=>{delete r.source.data.items[0].root_id;});
add('no-thread','context','feishu/accepted/thread',r=>{delete r.source.data.items[0].thread_id;});
add('context-no-thread-neighbours','thread-context','feishu/accepted/thread',r=>{delete r.source.data.items[0].thread_id;});
add('context-root','thread-context','feishu/accepted/root');
add('target-missing','context',undefined,r=>{r.message.data.messages=[];});
add('target-wrong-chat','context',undefined,r=>{r.message.data.messages[0].chat_id='oc_other';});
add('target-deleted','context',undefined,r=>{r.message.data.messages[0].deleted=true;});
add('target-null-content','context',undefined,r=>{r.message.data.messages[0].content=null;});
add('target-invalid-unrequested','raw',undefined,r=>{r.message.data.messages.push({message_id:'om_bad'});});
add('thread-wrong-topic','context','feishu/accepted/thread',r=>{r.thread.data.thread_id='omt_other';});
add('thread-wrong-chat','context','feishu/accepted/thread',r=>{r.thread.data.messages[0].chat_id='oc_other';});
add('thread-wrong-message-topic','context','feishu/accepted/thread',r=>{r.thread.data.messages[0].thread_id='omt_other';});
add('thread-missing-cursor','context','feishu/accepted/thread',r=>{r.thread.data.has_more=true;});
add('thread-stuck-cursor','context','feishu/accepted/thread/same',r=>{r.thread.data.has_more=true;r.thread.data.page_token='same';});
add('around-old-page','context','feishu/accepted/around/old%2F%E4%B8%AD%E6%96%87%20%F0%9F%98%80');
add('around-stuck-ends','context','feishu/accepted/around/same',r=>{r.earlier.data.page_token='same';});
add('around-invalid-skips-foreign-filters','context','feishu/accepted/around',r=>{r.earlier.data.items.push({message_id:'bad'});r.later.data.items.push(history('om_foreign',{chat_id:'oc_other'}));});
add('around-missing-time','context','feishu/accepted/around',()=>{}, {triggerId:'accepted',chatId:'oc_chat',messageId:'om_source'});
add('link','link',undefined,r=>{r.message.data.messages[0].message_app_link='https://applink.feishu.cn/client/message/open?messageId=om_source';});
add('link-wrong-chat','link',undefined,r=>{r.message.data.messages[0].message_app_link='https://example.test/message';r.message.data.messages[0].chat_id='oc_other';});
add('link-invalid-url','link',undefined,r=>{r.message.data.messages[0].message_app_link='not a url';});
add('link-missing','link');add('api-error','context',undefined,r=>{r.source={ok:false,error:{code:999,message:'temporarily unavailable'}};});
const resourceInputs=['feishu/accepted/message','feishu/accepted/root','feishu/accepted/parent','feishu/accepted/thread','feishu/accepted/around/older%2F%E4%B8%AD%E6%96%87%20%F0%9F%98%80','feishu/accepted/message/cursor','feishu//thread','feishu/accepted/thread/%ZZ','feishu/accepted/unknown','feishu/accepted/thread/a/b'];
const resources=resourceInputs.map(input=>{try{return {input,value:JSON.parse(JSON.stringify(feishuResource(input)))}}catch(error){return {input,error:true}}});
const views=[message(),message('om_deleted',{deleted:true}),message('om_bot',{sender:{id:'cli_bot',name:'',sender_type:'bot'},create_time:'unreadable',parent_id:'om_root'}),message('om_empty',{content:'',sender:{id:'ou_empty',sender_type:'user'}})].map(input=>({input,value:feishuView(input)}));
const directory=await mkdtemp(join(tmpdir(),'superagent-feishu-read-'));const cli=join(directory,'lark reader');
try{
 await writeFile(cli,await readFile(new URL('../../crates/xharness-superagent/tests/fixtures/feishu-reader.py',import.meta.url),'utf8'));await chmod(cli,0o700);
 const deployment={github:{cli:'unused',login:'robot'},repository:'owner/repo',feishu:{cli,appId:'cli_app',botId:'ou_bot',brand:'feishu',name:'Reader'}};
 for(const row of cases){
  await writeFile(join(directory,'responses.json'),JSON.stringify(row.responses));await writeFile(join(directory,'trace.jsonl'),'');
  const commands=new Commands(()=>deployment,{FEISHU_READER_ROOT:directory},new AbortController().signal);
  try{
   let value;
   if(row.mode==='link')value=await readFeishuLink(commands,row.reference);
   else if(row.mode==='raw')value=await readFeishu(commands,row.reference,row.resource,row.pageSize,row.order);
   else if(row.mode==='thread-context'){
    const t={id:row.reference.triggerId,source:'feishu',at:row.reference.at,meta:{chatId:row.reference.chatId,messageId:row.reference.messageId}};
    value=await feishuThreadContext.read(commands,t,row.resource.split('/')[2],undefined,{readPageSize:row.pageSize});
   }else value=await feishuRead(commands,row.reference,row.resource,row.pageSize);
   row.outcome={value:JSON.parse(JSON.stringify(value))};
  }catch(error){row.outcome={error:true};}
  row.trace=(await readFile(join(directory,'trace.jsonl'),'utf8')).trim().split('\n').filter(Boolean).map(s=>JSON.parse(s));
 }
}finally{await rm(directory,{recursive:true,force:true});}
const output={reference_commit:'6337fc823ce20251c8de8085948165c81aaa701e',scope:'Unchanged imported feishu-read/trigger/image-module definitions plus actual Commands and offline CLI endpoint; text/context/link only, no image service or full product approval',cases,resources,views,references:feishuReferences(reference)};
const file=new URL('./feishu-read-oracle.json',import.meta.url);if(process.argv.includes('--check'))assert.deepEqual(JSON.parse(await readFile(file,'utf8')),output);else await writeFile(file,JSON.stringify(output,null,2)+'\n');
console.log(`${cases.length} original Feishu read/context/link CLI cases, ${resources.length} resource boundaries and ${views.length} message projections verified.`);
