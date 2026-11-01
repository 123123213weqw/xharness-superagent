/** Actual unmodified published SDK callbacks/SkillRegistry/Session, compared to
 * production Rust catalog decisions. This is NOT native Core transport parity. */
import {it,expect,afterAll} from 'vitest'
import {readFileSync,writeFileSync,mkdtempSync,rmSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {spawnSync} from 'node:child_process'
import {Context} from '@deepseek-ai/cordis'
import SkillRegistry from '@deepseek-ai/dsh-skill'
import {createScope} from '@deepseek-ai/dsh-scope'
import {Session} from '@deepseek-ai/dsh-session'
import {agentEvents} from '@deepseek-ai/dsh-agent'
import {createUserMessage} from '@deepseek-ai/dsh-llm'
import {apply} from './skill-tool-sdk.mjs'
import {Store} from './upstream/src/app/store.ts'
import {Prompts} from './upstream/src/app/prompts.ts'
import {Sessions} from './upstream/src/app/sessions.ts'
import {environmentOf} from './upstream/src/app/execution-environment.ts'
const report:any={passed:false,production_Rust_catalog_protocol:true,actual_SDK_callbacks:true,real_Cordis_SkillRegistry_Session:true,original_Sessions_Prompts_SQL:true,prototype_216_files_unchanged:true,full_AgentLoop_executed:false,native_catalog_consumer_installed:false,full_provider_UI_config_UTF16_transport:false,comparisons:[],json_comparisons:0,all_96_Rubrics_accepted:0}
afterAll(()=>writeFileSync(process.env.SKILL_CATALOG_EVIDENCE!,JSON.stringify(report,null,2)+'\n'))
const cases:any[]=[]
function verify(){
 const result=spawnSync(process.env.SKILL_CATALOG_BRIDGE_BIN!,[],{input:cases.map(c=>JSON.stringify(c.request)).join('\n')+'\n',encoding:'utf8',timeout:60000,maxBuffer:64*1024*1024})
 expect(result.error).toBeUndefined();expect(result.status,result.stderr).toBe(0)
 const outputs=result.stdout.trim().split('\n').map(line=>JSON.parse(line));expect(outputs.length).toBe(cases.length)
 for(let i=0;i<cases.length;i++)expect(outputs[i],cases[i].name).toEqual(cases[i].expected)
 report.passed=true;report.json_comparisons=cases.filter(c=>c.json).length
}
async function fixture(limit=500,withProduct=true){
 const data=JSON.parse(readFileSync(join(process.env.NATIVE_PROMPT_TRACE_DIR!,'live-skills.json'),'utf8'));const folder=mkdtempSync(join(tmpdir(),'catalog-rust-'));const store=new Store(join(folder,'app.sqlite'));const ctx=new Context();await ctx.plugin(SkillRegistry)
 const prompts=new Prompts(store,()=>100000,()=>environmentOf({deployment:{} as any,sources:{environments:()=>[]} as any,settings:data.settings}),()=>false)
 prompts.save(data.configs[0],0,'fixture')
 const sections:any[]=[];let loader:any;let visible:any
 const tools:any={register:(tool:any)=>{loader=tool;visible=tool;return()=>{}},get:()=>visible,restrict:()=>()=>{},guard:()=>()=>{}}
 const agent:any={id:'main',status:'idle',session:Session.create('main')};const scope=createScope(ctx,agent)
 agent.ctx=scope.ctx.extend({systemPrompt:{section:(section:any)=>{sections.push(section);return()=>{}}},tools,workingDirectory:{ensure:async()=>data.cwd}})
 const sessions:any=new Sessions({context:ctx,store,prompts,effects:{handle:()=>()=>{}},health:{set:()=>{}},masterId:'main',lifetime:new AbortController().signal,closing:()=>false,tools:()=>[],environment:()=>{throw Error('Main must not read worker environment')}} as any)
 sessions.registerTool=()=>()=>{} // unrelated application tools are explicit adapters
 await agent.ctx.inject(['skills'],(injected:any)=>apply(injected,{catalogDescriptionMaxLength:limit}))
 if(withProduct)await sessions.decorate(agent)
 const signal=new AbortController().signal;let step=0
 agent.session.append('turn/start',{turn:1})
 const text=(body:string,kind='user')=>createUserMessage({content:[{type:'text',text:body}],source:{kind} as any})
 async function observe(name:string,claimed:any[],nextDecision:any={kind:'enter',messages:claimed},commit=true){
  if(withProduct)sections[0].text({signal}) // actual Sessions revision/skill refresh
  const history=agent.session.snapshotEvents();const surface=[...agent.session.surface.nodes]
  const snapshot=await ctx.skills.snapshot({scope:agent,cwd:data.cwd,signal});const skills=[]
  for(const summary of snapshot.skills){const skill=await ctx.skills.get(summary.name,{scope:agent,cwd:data.cwd,signal});expect(skill).toBeDefined();skills.push({...skill,modelInvocable:skill!.invocation.modelInvocable,userInvocable:skill!.invocation.userInvocable})}
  const actual:any=await agentEvents(agent.ctx,agent).waterfall('agent/pre-step',{messages:claimed,signal,turn:1,step:step+1},()=>Promise.resolve(nextDecision))
  const beforeIds=new Set((nextDecision.messages??[]).map((m:any)=>m.id));const ids=[...new Set((actual.messages??[]).filter((m:any)=>!beforeIds.has(m.id)).map((m:any)=>m.id))]
  const request={decision:nextDecision,claimed,events:history,visible:surface,snapshot:{skills,complete:snapshot.complete},loaderVisible:visible===loader,limit,ids}
  const expected={ok:true,value:{decision:actual,allocated:ids.length}}
  cases.push({name,request:structuredClone(request),expected:structuredClone(expected)})
  report.comparisons.push({name,claimed:structuredClone(claimed),expected:structuredClone(actual),snapshot_complete:snapshot.complete,visible:surface})
  if(commit&&actual.kind==='enter'){
   agent.session.append('step/start',{turn:1,step:++step});for(const m of actual.messages)agent.session.append('user/message',m,{surfaceOp:'append'});agent.session.append('step/end',{turn:1,step})
  }
  return actual
 }
 return{data,ctx,scope,agent,prompts,text,observe,register:async(skill:any)=>{await agent.ctx.inject(['skills'],(injected:any)=>{injected.skills.register(skill)})},shadow:()=>{visible={...loader}},unshadow:()=>{visible=loader},close:async()=>{sessions.dispose();await scope.dispose();await ctx.fiber.dispose();store.close();rmSync(folder,{recursive:true,force:true})}}
}
it('matches full catalog candidate/drop/replace and direct-invocation decisions with actual SDK and lone surrogates',async()=>{
 const f=await fixture()
 try{
  const first=await f.observe('first-direct-and-dedup',[f.text('/dsh-superagent-custom /dsh-superagent-custom /usr/bin /unknown')]);expect(first.messages.map((m:any)=>m.source.kind)).toEqual(['user','skill-catalog','skill-invocation'])
  const custom=first.messages[1].source.entries.find((e:any)=>e.name==='dsh-superagent-custom');expect(custom.description.charCodeAt(custom.description.length-1)).toBe(0xd83d)
  expect((await f.observe('unchanged-no-input',[])).messages).toEqual([])
  await f.observe('forged-runtime-gesture',[f.text('/dsh-superagent-custom','runtime-context')])
  f.prompts.save(f.data.configs[1],1,'fixture');await f.observe('version-update',[])
  const catalog=f.agent.session.snapshotEvents().filter((e:any)=>e.type==='user/message'&&e.data.source.kind==='skill-catalog').at(-1)
  f.agent.session.append('user/message',f.text('hidden catalog','runtime-context'),{surfaceOp:{op:'replace',startSeq:catalog.seq,endSeq:catalog.seq},sourceEventSeqs:[catalog.seq]})
  await f.observe('hidden-visible-catalog-republish',[])
  f.shadow();await f.observe('shadow-empty-update-and-direct',[f.text('/dsh-superagent-custom')])
  f.agent.session=Session.create('main',f.agent.session.snapshotEvents());await f.observe('actual-Session-restore-no-duplicate',[])
  f.unshadow();const restored=await f.observe('unshadow-republish',[])
  const duplicate=structuredClone(restored.messages[0]);duplicate.content[0].text='must drop this frame';await f.observe('drop-current-candidate',[duplicate])
  const sameId=structuredClone(duplicate);sameId.source={kind:'runtime-context'};await f.observe('drop-all-matching-candidate-IDs',[duplicate,sameId,f.text('keep')])
  const config=structuredClone(f.data.configs[1]);config.skills.custom='gamma candidate instructions';f.prompts.save(config,2,'fixture')
  const proposal=await f.observe('actual-uncommitted-candidate',[],undefined,false);const candidate=structuredClone(proposal.messages[0]);candidate.content[0].text='preserve \ud800 framing';candidate.source.extra={lone:'\udfff'}
  const kept=await f.observe('matching-candidate-preserve-full-envelope',[candidate]);expect(kept.messages[0]).toEqual(candidate)
  const next=structuredClone(config);next.skills.custom='delta replacement';f.prompts.save(next,3,'fixture');await f.observe('stale-candidate-replace-in-position',[f.text('before'),candidate,f.text('after'),structuredClone(candidate)])
  const invalid=structuredClone(f.text('malformed catalog'));invalid.source={kind:'skill-catalog',entries:[{name:'not-usable',description:9}]};await f.observe('malformed-candidate-kept-without-forgery',[invalid])
  await f.observe('reject-preserved',[f.text('/dsh-superagent-custom')],{kind:'reject',reason:{kind:'fixture',private:'\ud800'}})
  const incomplete=f.ctx.skills.registerProvider(()=>({name:'incomplete-fixture',list:async()=>({candidates:[],complete:false}),get:async()=>undefined}))
  try{await f.observe('actual-provider-incomplete-still-direct',[f.text('/dsh-superagent-custom')])}finally{incomplete()}
  for(const [name,base,invocation]of [
   ['only-user',{kind:'url',url:'https://example.invalid/a?x=<>&'}, {modelInvocable:false,userInvocable:true}],
   ['only-model',{kind:'opaque',description:'<&>\ud800'}, {modelInvocable:true,userInvocable:false}],
   ['neither',undefined,{modelInvocable:false,userInvocable:false}],
  ]as const)await f.register({name,description:'  description\twith <>&\ud800 ',content:'literal \udc00 <instructions>',source:'runtime',resourceBase:base,invocation})
  await f.observe('real-invocation-policies-and-URL-opaque-resources',[f.text('/only-user /only-model /neither /dsh-superagent-github')])
 }finally{await f.close()}
 for(const limit of [3,4,8,500]){
  const g=await fixture(limit,false)
  try{
   await g.register({name:'edge',source:'runtime',provider:'r&<',description:'\ufeff\tA🚀<&>\ud800'+('x'.repeat(510))+'\u2029 ',content:'\udfffbody'})
   g.shadow();expect((await g.observe(`initial-empty-shadow-${limit}`,[])).messages).toEqual([])
   const empty=structuredClone(g.text('initial catalog candidate'));empty.source={kind:'skill-catalog',form:'catalog',entries:[]}
   expect((await g.observe(`initial-empty-candidate-preserved-${limit}`,[empty],undefined,false)).messages).toEqual([empty])
   const stale=structuredClone(empty);stale.source.entries=[{name:'old',description:'gone'}]
   expect((await g.observe(`initial-stale-candidate-drop-${limit}`,[stale],undefined,false)).messages).toEqual([]);g.unshadow()
   const first=await g.observe(`UTF16-truncation-${limit}`,[g.text('/edge')]);const candidate=structuredClone(first.messages[1]);candidate.source.entries=[];await g.observe(`drop-empty-initial-candidate-${limit}`,[candidate])
  }finally{await g.close()}
 }
 // Exercise the exact same non-scalar representation used by the product
 // catalog bridge, including UTF16 keys and JSON.stringify number ordering.
 const raw=['{"2":0,"1":-0,"01":1,"\\ud800":"\\udfff","\\ud800":"last"}','[1e999,-1e999,9007199254740993,1e-7,1e21]','"\\ud83d\\ude80"','"\\ud800"','{"__proto__":{"own":true},"4294967295":2,"0":3}']
 let seed=0x51a22620;const random=()=>{seed=(Math.imul(seed,1664525)+1013904223)>>>0;return seed}
 for(let i=0;i<300;i++){const s=Array.from({length:random()%24},()=>String.fromCharCode(random()%65536)).join('');const o=JSON.parse('{"10":1,"2":2}');o[s]=[s,random()/7];raw.push(JSON.stringify(o))}
 for(const [i,text]of raw.entries())cases.push({name:`lossless-JSON-${i}`,json:true,request:{op:'json',text},expected:{ok:true,value:JSON.stringify(JSON.parse(text))}})
 for(const text of ['"\\x"','"\\u1"','[1,]','{"a":1,}','01','1.','1e+','true false','NaN'])cases.push({name:`invalid-JSON-${text}`,json:true,request:{op:'json',text},expected:{ok:false,error:'skill catalog bridge request failed'}})
 for(let i=0;i<32;i++){const units=Array.from({length:300},()=>random()%65536);cases.push({name:`UTF16-quote-${i}`,json:true,request:{op:'quote',units},expected:{ok:true,value:units.map(u=>String.fromCharCode(u)).join('')}})}
 verify()
},120000)
