// Source protocol oracle, not SDK/full-kind/product acceptance. Trigger/channel
// functions and Commands are imported unchanged; setup schema and policy sync
// are isolated byte-unchanged because full kind import requires unrelated SDKs.
import {mkdtemp,writeFile,readFile,chmod,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {stripTypeScriptTypes} from 'node:module';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
import {z} from 'zod';
import {feishuTrigger,withMentionNames} from '../reference/upstream/src/sources/feishu-trigger.ts';
import {sendFeishu,feishuChatLink} from '../reference/upstream/src/sources/feishu-channel.ts';
import {Commands} from '../reference/upstream/src/infra/commands.ts';
import {sourceContext} from '../reference/upstream/src/sources/host.ts';
const sha=text=>createHash('sha256').update(text).digest('hex');
const source=await readFile(new URL('../reference/upstream/src/sources/feishu.ts',import.meta.url),'utf8');
const deploymentSource=await readFile(new URL('../reference/upstream/src/deployment.ts',import.meta.url),'utf8');
const slice=(source,start,end)=>{const from=source.indexOf(start);assert.ok(from>=0);const to=source.indexOf(end,from);assert.ok(to>from);return source.slice(from,to);};
const botSchema=slice(deploymentSource,'export const FeishuBotSchema =','\n');
const configSchema=slice(source,'const FeishuConfigSchema =','\nexport type FeishuConfig');
const memberSchema=slice(source,'const MembersSchema =','\nconst FeishuConfigSchema');
const run=slice(source,'      async run(host, part, signal) {','\n      },\n    },\n  },').trim()+'\n}';
const policySchema=slice(source,'    schema: z.object({','\n    fields:').trim().slice('schema: '.length).replace(/,$/,'');
const policy=new Function('z','return '+policySchema)(z);
const policies=[{}, {allowed:[{id:'ou_alice',name:' 好 '}],bots:[{id:'cli_bot'}],syncChat:'oc_sync'}, {allowed:[{id:'ou_one'},{id:'ou_one'}]}, {bots:[{id:'github-login'}]}, {unknown:true}, {syncChat:'oc_'}, {allowed:[{id:'ou_one',name:null}]}].map(input=>{const parsed=policy.safeParse(input);return{input,...parsed.success?{value:parsed.data}:{error:true}};});
const schemaCode=stripTypeScriptTypes(botSchema.replace('export const','const')+'\n'+configSchema+'\n'+memberSchema);
const {config,members}=new Function('z',schemaCode+'\nreturn {config:FeishuConfigSchema,members:MembersSchema};')(z);
// Only the method-to-function wrapper and type erasure change syntax; the
// original sync body is retained, including its actual sourceContext call.
const syncCode=stripTypeScriptTypes(run.replace(/^async run\(/,'async function run('));
const sync=new Function('MembersSchema','sourceContext','feishuKind',syncCode+';return run;')(members,sourceContext,{kind:'feishu',health:['feishu','history']});
const epoch=String(Date.UTC(2026,10,2,0,0));
const base={message_id:'om_one',chat_id:'oc_chat',chat_type:'group',sender_id:'ou_alice',sender_type:'user',content:'你好 @_user_1，补充 @_user_1 😀',create_time:epoch,mentions:[{id:'ou_bot',key:'@_user_1',name:'机器人'}]};
const events=[
 ['group-mention',base],['direct',{...base,chat_type:'p2p'}],['bot',{...base,sender_id:'cli_other',sender_type:'bot',mentions:[]}],
 ['unaddressed',{...base,mentions:[]}],['self-bot',{...base,sender_id:'ou_bot'}],['self-app',{...base,sender_id:'cli_app'}],
 ['thread-reply',{...base,root_id:'om_root',parent_id:'om_parent',thread_id:'omt_topic',message_app_link:'https://applink.feishu.cn/client/message/open?x=1'}],
 ['missing-mentions',Object.fromEntries(Object.entries(base).filter(([key])=>key!=='mentions'))],
 ['invalid-time',{...base,create_time:'not-a-date'}],['invalid-type',{...base,sender_type:'app'}],['null-root',{...base,root_id:null}],['invalid-url',{...base,message_app_link:'not a url'}],
].map(([name,input])=>{try{return{name,input,value:feishuTrigger(input,'ou_bot','cli_app')??null};}catch(error){return{name,input,error:true,message:String(error.message)};}});
const configCases=[{}, {cli:' lark ',chats:[{chatId:'oc_one',listen:'all',name:' 名字 '}]},{chats:[{chatId:'oc_one',listen:'all'},{chatId:'oc_one',listen:'mentions'}]},{chats:[{chatId:'bad',listen:'all'}]},{readPageSize:0},{linkTimeoutMs:90001},{otherChats:'all'},{chats:[{chatId:'oc_one',listen:'all',name:null}]}].map(input=>{const parsed=config.safeParse(input);return{input,...parsed.success?{value:parsed.data}:{error:true}};});
const scopes=[
 [{direct:true},{}],[{direct:true},{directMessages:false}],[{chatId:'oc_one'},{chats:[{chatId:'oc_one',listen:'all'}]}],
 [{chatId:'oc_one'},{chats:[{chatId:'oc_one',listen:'mentions'}]}],[{chatId:'oc_other'},{otherChats:'mentions'}],[{chatId:'oc_other'},{}],
].map(([meta,input])=>{const c=config.parse(input);let value;if(meta.direct===true)value=c.directMessages?{listened:true,observed:false,row:'direct'}:{listened:false,reason:'direct_messages_not_allowed'};else {const chat=c.chats.find(c=>c.chatId===meta.chatId);value=chat?{listened:true,observed:chat.listen==='all',row:chat.chatId}:c.otherChats==='mentions'?{listened:true,observed:false,row:'other'}:{listened:false,reason:'chat_not_allowed'};}return{meta,input,value};});
// Scope is exercised below against the original exact method, not the planner
// above alone, which keeps fixture values explicit and independently readable.
const scope=slice(source,'  scope({ meta }, config) {','\n  },\n  // A reply').trim()+'\n}';
const scopeFn=new Function('return '+scope.replace(/^scope\(/,'function('))();
for(const row of scopes)assert.deepEqual(scopeFn({meta:row.meta},config.parse(row.input)),row.value);
const before={allowed:[{id:'ou_keep',name:'Old'},{id:'ou_remove',name:'Gone'}],syncChat:'oc_sync',bots:[{id:'cli_bot'}]};
const syncCases=[
 ['preserve-update-append',{users:[{member_id:'ou_new',name:'新用户'},{member_id:'ou_keep',name:'新名字 😀'}],has_more:false}],
 ['empty-list',{users:[],has_more:false}],['incomplete',{users:[],has_more:true}],['truncated',{users:[],has_more:false,truncations:[{}]}],
 ['missing-has-more',{users:[]}],['invalid-member',{users:[{member_id:'ou_new'}],has_more:false}],['null-truncations',{users:[],has_more:false,truncations:null}],
 ['duplicate-new-members',{users:[{member_id:'ou_new',name:'first'},{member_id:'ou_new',name:'last'}],has_more:false}],
];
const channel={source:'feishu',thread:'feishu:oc_chat',address:{chatId:'oc_chat',messageId:'om_one'}};
const replies=[
 ['sent',{ok:true,data:{message_id:'om_reply'}},'正文 😀'],['sent-no-payload',{ok:true},'正文 😀'],['withdrawn',{ok:false,error:{code:230011,message:'withdrawn'}},'正文'],
 ['not-found',{ok:false,error:{code:230099,message:'Not Found'}},'正文'],['membership',{ok:false,error:{code:232011,message:'not joined'}},'正文'],
 ['uncoded-withdrawn',{ok:false,error:{message:'withdrawn'}},'正文'],['transient',{ok:false,error:{code:999,message:'timeout'}},'正文'],
 ['over-limit',{ok:true,data:{}},'修复 '.repeat(101)],
];
const program=`#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
root=Path(os.environ['FEISHU_ORACLE_ROOT']);args=sys.argv[1:]
assert args[-4:]==['--as','bot','--format','json'],args
with (root/'trace.jsonl').open('a') as f:f.write(json.dumps(args,ensure_ascii=False)+'\\n')
print((root/'reply.json').read_text())
`;
const dir=await mkdtemp(join(tmpdir(),'superagent-feishu-oracle-'));const synced=[],sent=[];
try{
 const cli=join(dir,'lark fixture');await writeFile(cli,program);await chmod(cli,0o700);
 const deployment={repository:'owner/repo',github:{cli:'unused',login:'robot'},feishu:{cli,appId:'cli_app',botId:'ou_bot',name:'机器人',brand:'feishu'}};
 for(const [name,data] of syncCases){
  await writeFile(join(dir,'reply.json'),JSON.stringify({ok:true,data}));await writeFile(join(dir,'trace.jsonl'),'');
  const commands=new Commands(()=>deployment,{FEISHU_ORACLE_ROOT:dir},new AbortController().signal);
  let outcome;try{outcome={value:await sync({context:()=>({commands})},before,new AbortController().signal)};}catch(error){outcome={error:true,message:String(error.message)};}
  const trace=(await readFile(join(dir,'trace.jsonl'),'utf8')).trim().split('\n').filter(Boolean).map(s=>JSON.parse(s));synced.push({name,before,data,outcome,trace});
 }
 for(const [name,response,text] of replies){
  await writeFile(join(dir,'reply.json'),JSON.stringify(response));await writeFile(join(dir,'trace.jsonl'),'');
  const commands=new Commands(()=>deployment,{FEISHU_ORACLE_ROOT:dir},new AbortController().signal);
  const message={id:'receipt-one',channel,text};let outcome;try{outcome={value:await sendFeishu({commands},message)};}catch(error){outcome={error:true,message:String(error.message),...error.code!==undefined?{code:error.code}:{}};}
  const trace=(await readFile(join(dir,'trace.jsonl'),'utf8')).trim().split('\n').filter(Boolean).map(s=>JSON.parse(s));sent.push({name,message,response,outcome,trace});
 }
}finally{await rm(dir,{recursive:true,force:true});}
const output={reference_commit:'6337fc823ce20251c8de8085948165c81aaa701e',scope:'Source boundary only; original trigger/channel/Commands imports plus isolated unchanged Config/Members/scope/sync method with type erasure and method wrapper; no SDK stubs, full-kind/runtime/product approval',isolated_sha256:{botSchema:sha(botSchema),configSchema:sha(configSchema),memberSchema:sha(memberSchema),syncMethod:sha(run),scopeMethod:sha(scope),policySchema:sha(policySchema)},events,policies,config:configCases,scopes,sync:synced,replies:sent,links:[['feishu','oc_中 文/?'],['lark','oc_one']].map(([brand,chat])=>({brand,chat,value:feishuChatLink(brand,chat)})),mentions:withMentionNames('A A B',[{id:'x',key:'A',name:'甲'},{id:'y',key:'B',name:'乙'}])};
const file=new URL('./feishu-source-oracle.json',import.meta.url);
if(process.argv.includes('--check'))assert.deepEqual(JSON.parse(await readFile(file,'utf8')),output);else await writeFile(file,JSON.stringify(output,null,2)+'\n');
console.log(`${events.length} trigger, ${configCases.length} config, ${policies.length} policy, ${scopes.length} scope, ${synced.length} member-sync, ${sent.length} reply, 2 link and 1 mention Source cases verified.`);
