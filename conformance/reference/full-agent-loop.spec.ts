/** Reference-side full published AgentLoop with real JSONL cold recovery.
 * The Rust bridge is the production pure policy, NOT Native Core integration. */
import {it,expect,afterAll} from 'vitest'
import {readFileSync,writeFileSync} from 'node:fs'
import {join} from 'node:path'
import {spawnSync} from 'node:child_process'
import {Context} from '@deepseek-ai/cordis'
import Agents from '@deepseek-ai/dsh-agent'
import SessionsStore from '@deepseek-ai/dsh-session'
import LlmRuntime,{LlmAdapter,createUserMessage} from '@deepseek-ai/dsh-llm'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import Tools from '@deepseek-ai/dsh-tools'
import Skills from '@deepseek-ai/dsh-skill'
import Projections from '@deepseek-ai/dsh-session-projection'
import AgentLoop from '@deepseek-ai/dsh-agent-loop'
import JsonlPersistence from '@deepseek-ai/dsh-session-persistence-jsonl'
import {apply as skillPlugin} from './skill-tool-sdk.mjs'
import {Store} from './upstream/src/app/store.ts'
import {Prompts} from './upstream/src/app/prompts.ts'
import {Sessions} from './upstream/src/app/sessions.ts'
import {environmentOf} from './upstream/src/app/execution-environment.ts'
const report:any={passed:false,actualPublishedAgentLoop:true,actualPublishedJsonlBackend:true,manualPreStepOrLifecycle:false,paidModelCalls:0,nativeRustLoopParity:false,productionPolicyDifferential:false,acceptedRubrics:0,requests:[],events:[],statuses:[],errors:[],policyCases:[],ports:{model:'scripted adapter only',workingDirectory:'isolated fixture cwd',nonSkillTools:'declarations only; bodies reject execution',appTools:'empty for scoped catalog oracle',effects:'not exercised'}}
afterAll(()=>writeFileSync(process.env.FULL_LOOP_EVIDENCE!,JSON.stringify(report,null,2)+'\n'))
it('full loop, real skill dispatch, opaque UTF16 and exact JSONL cold prefix with production policy differential',async()=>{
 const root=process.env.COLD_LOOP_DIR!;const phase=process.env.COLD_LOOP_PHASE!
 expect(['create','resume']).toContain(phase);report.phase=phase;report.pid=process.pid
 const data=JSON.parse(readFileSync(new URL('./full-agent-loop-fixture.json',import.meta.url),'utf8'));data.cwd=root
 const store=new Store(join(root,'app.sqlite'));const ctx=new Context();let handle:any;let product:any
 const answer='fixture answer \udfff'
 class ScriptedModel extends LlmAdapter {
  async *stream(options:any){
   options.signal?.throwIfAborted();const {signal,...request}=options;report.requests.push(structuredClone(request))
   const last=request.messages.at(-1)
   if(last.role==='user'&&last.content.some((b:any)=>b.type==='text'&&b.text==='load full custom')){
    const block={type:'tool-call',id:'fixture-skill-call',name:'skill',arguments:JSON.stringify({name:'dsh-superagent-custom'})}
    yield {type:'block-start',index:0,blockType:'tool-call'}
    yield {type:'tool-call-delta',index:0,id:block.id,name:block.name,argumentsDelta:block.arguments}
    yield {type:'block-end',index:0,block};yield {type:'finish',reason:{kind:'tool-calls'}}
   }else{
    yield {type:'block-start',index:0,blockType:'text'};yield {type:'text-delta',index:0,text:answer}
    yield {type:'block-end',index:0,block:{type:'text',text:answer}};yield {type:'finish',reason:{kind:'stop'}}
   }
  }
 }
 try{
  await ctx.plugin(SessionsStore);await ctx.plugin(Agents);await ctx.plugin(LlmRuntime)
  await ctx.plugin(SystemPrompt);await ctx.plugin(Tools);await ctx.plugin(Skills);await ctx.plugin(Projections)
  ctx.provide('workingDirectory',{ensure:async()=>data.cwd})
  for(const name of ['bash','read','read_image','job_output','job_list','job_kill'])ctx.tools.register({name,description:'Not executed in this fixture',parameters:{type:'object'},output:{schema:{},render:()=>[]},execute:async()=>{throw Error('Unexpected fixture tool execution: '+name)}})
  await ctx.inject(['agents','tools','skills','workingDirectory'],(scope:any)=>skillPlugin(scope,{catalogDescriptionMaxLength:500}))
  const loader=ctx.tools.get('skill')
  ctx.llm.registerAdapter(['fixture'],new ScriptedModel())
  await ctx.plugin(JsonlPersistence,{root:join(root,'sessions'),compression:process.env.FULL_LOOP_COMPRESSION??'none'})
  await ctx.plugin(AgentLoop,{agents:[],maxParallelToolCalls:1})
  const prompts=new Prompts(store,()=>100000,()=>environmentOf({deployment:{} as any,sources:{environments:()=>[]} as any,settings:data.settings}),()=>false)
  if(phase==='create')prompts.save(data.configs[0],0,'fixture');else expect(prompts.current().revision).toBe(1)
  product=new Sessions({context:ctx,store,prompts,effects:{handle:()=>()=>{}},health:{set:()=>{}},masterId:'main',lifetime:new AbortController().signal,closing:()=>false,tools:()=>[],environment:()=>{throw Error('Main must not read worker environment')}} as any)
  const setup=async(scope:any,agent:any)=>{
   await product.decorate(agent)
   scope.on('agent/status',({status}:any)=>{report.statuses.push(status)})
   scope.on('agent/error',({error}:any)=>{report.errors.push({name:error?.name,code:error?.code})})
   let baseline:any
   // The inner observer returns fallback unchanged; the outer observer records
   // the actual plugin decision without supplying any replacement decision.
   scope.on('agent/pre-step',async(_input:any,next:any)=>{const decision=await next();baseline=structuredClone(decision);return decision})
   scope.on('agent/pre-step',async({messages,signal}:any,next:any)=>{
    const events=agent.session.snapshotEvents();const visible=[...agent.session.surface.nodes]
    const snapshot=await ctx.skills.snapshot({scope:agent,cwd:data.cwd,signal});const skills=[]
    for(const summary of snapshot.skills){const skill=await ctx.skills.get(summary.name,{scope:agent,cwd:data.cwd,signal});expect(skill).toBeDefined();skills.push({...skill,modelInvocable:skill!.invocation.modelInvocable,userInvocable:skill!.invocation.userInvocable})}
    baseline=undefined;const actual=await next();expect(baseline).toBeDefined()
    const beforeIds=new Set((baseline.messages??[]).map((m:any)=>m.id))
    const ids=[...new Set((actual.messages??[]).filter((m:any)=>!beforeIds.has(m.id)).map((m:any)=>m.id))]
    report.policyCases.push(structuredClone({request:{decision:baseline,claimed:messages,events,visible,snapshot:{skills,complete:snapshot.complete},loaderVisible:ctx.tools.get('skill',agent)===loader,limit:500,ids},expected:{ok:true,value:{decision:actual,allocated:ids.length}}}))
    return actual
   },{prepend:true})
  }
  handle=phase==='create'?await ctx.agents.create({sessionId:'main',agentOptions:{provider:'fixture',model:'scripted'},meta:{cwd:data.cwd},setup}):await ctx.agents.resume({resumeSessionId:'main',agentOptions:{provider:'fixture',model:'scripted'},setup})
  const agent=handle.agent
  async function turn(body:string){agent.followup(createUserMessage({content:[{type:'text',text:body}],source:{kind:'user'}}));await agent.whenIdle();expect(agent.status).toBe('idle');expect(report.errors).toEqual([])}
  if(phase==='create'){
   const opaque=createUserMessage({content:[{type:'text',text:'opaque \ud800 context'}],source:{kind:'fixture-context',opaque:{'\ud800':'\udfff',literal:'\\ud800',replacement:'\ufffd'}} as any})
   const reserved=createUserMessage({content:[{type:'text',text:'reserved runtime context candidate'}],source:{kind:'runtime-context'}})
   agent.inject(reserved);agent.inject(opaque);await turn('hello \ud800');await turn('load full custom')
   expect(report.requests).toHaveLength(3)
   const catalog=report.requests[0].messages.find((m:any)=>m.source?.kind==='skill-catalog')
   const entry=catalog.source.entries.find((e:any)=>e.name==='dsh-superagent-custom')
   expect(entry.description.charCodeAt(entry.description.length-1)).toBe(0xd83d)
   expect(report.requests[0].messages.find((m:any)=>m.id===opaque.id)).toEqual(opaque)
   expect(report.policyCases[0].request.claimed.some((m:any)=>m.id===reserved.id)).toBe(true)
   expect(report.requests[0].messages.some((m:any)=>m.id===reserved.id)).toBe(false)
   report.reservedRuntimeContextCandidateRemovedBeforeModel=true
   const result=report.requests[2].messages.find((m:any)=>m.role==='tool'&&m.toolCallId==='fixture-skill-call')
   expect(result).toBeDefined();expect(result.isError??false).toBe(false)
   expect(result.content[0].text).toContain('<skill_content name="dsh-superagent-custom">')
   expect(agent.session.snapshotEvents().filter((e:any)=>e.type==='user/message'&&e.data.source.kind==='skill-catalog')).toHaveLength(1)
   report.beforeEvents=agent.session.snapshotEvents();writeFileSync(join(root,'before-events.json'),JSON.stringify(report.beforeEvents))
  }else{
   const before=JSON.parse(readFileSync(join(root,'before-events.json'),'utf8'));const restored=agent.session.snapshotEvents()
   expect(restored.slice(0,before.length)).toEqual(before)
   expect(restored.slice(before.length)).toEqual([{seq:before.length,time:expect.any(Number),type:'session/end-seed',data:{}}]);report.restoreBoundary=restored.at(-1)
   await turn('after cold restart');expect(report.requests).toHaveLength(1)
   const catalogs=report.requests[0].messages.filter((m:any)=>m.source?.kind==='skill-catalog');expect(catalogs).toHaveLength(1)
   const description=catalogs[0].source.entries.find((e:any)=>e.name==='dsh-superagent-custom').description
   expect(description.charCodeAt(description.length-1)).toBe(0xd83d)
   const opaque=report.requests[0].messages.find((m:any)=>m.source?.opaque)
   expect(opaque.source.opaque).toEqual({'\ud800':'\udfff',literal:'\\ud800',replacement:'\ufffd'});expect(opaque.content[0].text).toBe('opaque \ud800 context')
   expect(report.requests[0].messages.some((m:any)=>m.role==='assistant'&&m.content.some((b:any)=>b.type==='text'&&b.text===answer))).toBe(true)
   expect(agent.session.snapshotEvents().filter((e:any)=>e.type==='user/message'&&e.data.source.kind==='skill-catalog')).toHaveLength(1)
   prompts.save(data.configs[1],1,'fixture');await turn('/dsh-superagent-custom');expect(report.requests).toHaveLength(2)
   expect(report.requests[1].messages.some((m:any)=>m.source?.kind==='skill-invocation')).toBe(true)
   expect(agent.session.snapshotEvents().filter((e:any)=>e.type==='user/message'&&e.data.source.kind==='skill-catalog')).toHaveLength(2)
  }
  report.events=agent.session.snapshotEvents();report.surface=[...agent.session.surface.nodes];await ctx.sessionPersistence.flush()
  expect(report.statuses).toEqual(['running','idle','running','idle']);expect(report.errors).toEqual([])
  expect(report.policyCases.length).toBe(phase==='create'?3:2)
  // A prepended recorder must actually surround the plugin, not merely observe
  // fallback after it: the first fresh policy cut must allocate the catalog.
  if(phase==='create')expect(report.policyCases[0].expected.value.allocated).toBe(1)
  const bridge=spawnSync(process.env.SKILL_CATALOG_BRIDGE_BIN!,[],{input:report.policyCases.map((c:any)=>JSON.stringify(c.request)).join('\n')+'\n',encoding:'utf8',timeout:30000,maxBuffer:32*1024*1024})
  expect(bridge.error).toBeUndefined();expect(bridge.status,bridge.stderr).toBe(0)
  expect(bridge.stdout.trim().split('\n').map((line:string)=>JSON.parse(line))).toEqual(report.policyCases.map((c:any)=>c.expected))
  report.productionPolicyDifferential=true;report.passed=true
 }finally{if(handle)await handle.dispose();if(product)product.dispose();await ctx.fiber.dispose();store.close()}
})
