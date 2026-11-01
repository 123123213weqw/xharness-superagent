/** Actual original Sessions + real Cordis/Scope/SkillRegistry + published skill
 * loader, compared to durable outcomes AND actual Rust Provider model text. */
import {it,expect,afterAll} from 'vitest'
import {readFileSync,writeFileSync,mkdtempSync,rmSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {Context} from '@deepseek-ai/cordis'
import SkillRegistry from '@deepseek-ai/dsh-skill'
import {createScope} from '@deepseek-ai/dsh-scope'
import {apply} from './skill-tool-sdk.mjs'
import {Store} from './upstream/src/app/store.ts'
import {Prompts} from './upstream/src/app/prompts.ts'
import {Sessions} from './upstream/src/app/sessions.ts'
import {environmentOf} from './upstream/src/app/execution-environment.ts'
const report={original_sessions:true,real_Cordis_Scope_SkillRegistry:true,actual_supplemental_skill_tool:true,supplemental_version:'0.2.1-alpha.2',no_module_mocks:true,durable_outcomes_compared:0,registration_snapshots:0,model_outputs_compared:0,model_rendering_exact:false,oversize_and_error_model_parity_accepted:false,catalog_callbacks_executed:false,unrelated_tool_and_working_directory_adapters:'explicit fixtures'}
afterAll(()=>writeFileSync(process.env.LIVE_SKILLS_ORACLE_EVIDENCE!,JSON.stringify(report,null,2)+'\n'))
it('matches real registered scope versions, original same-revision service restart, UTF16 descriptions and real skill loader outcomes',async()=>{
 const data=JSON.parse(readFileSync(join(process.env.NATIVE_PROMPT_TRACE_DIR!,'live-skills.json'),'utf8'))
 const path=mkdtempSync(join(tmpdir(),'live-skills-oracle-'));const store=new Store(join(path,'app.sqlite'));const ctx=new Context()
 let fiber=ctx.plugin(SkillRegistry);await fiber
 let feishu=false
 const prompts=new Prompts(store,()=>100000,()=>environmentOf({deployment:{} as any,sources:{environments:()=>[]} as any,settings:data.settings}),()=>feishu)
 prompts.save(data.configs[0],0,'fixture')
 const sections:any[]=[];let loader:any
 const toolAdapters:any={register:(tool:any)=>{loader=tool;return ()=>{}},get:()=>loader,restrict:()=>()=>{},guard:()=>()=>{}}
 const agent:any={id:'main',status:'idle',session:{header:{},snapshotEvents:()=>[]}}
 const scope=createScope(ctx,agent)
 agent.ctx=scope.ctx.extend({systemPrompt:{section:(section:any)=>{sections.push(section);return ()=>{const i=sections.indexOf(section);if(i>=0)sections.splice(i,1)}}},tools:toolAdapters})
 const sessions:any=new Sessions({context:ctx,store,prompts,effects:{handle:()=>()=>{}},health:{set:()=>{}},masterId:'main',lifetime:new AbortController().signal,closing:()=>false,tools:()=>[],environment:()=>{throw new Error('Main must not read Worker environment')}} as any)
 sessions.registerTool=()=>()=>{}
 const pluginContext:any={tools:toolAdapters,get skills(){return ctx.skills},workingDirectory:{ensure:async()=>data.cwd},on:()=>()=>{}}
 apply(pluginContext)
 const signal=new AbortController().signal
 async function snapshot(index:number,version=prompts.current()){
  const names=prompts.skills(version.config).map(([name])=>'dsh-superagent-'+name)
  expect(data.registration_snapshots[index].map((s:any)=>s.name)).toEqual(names)
  const actual=await ctx.skills.list({scope:agent})
  expect(actual.map((s:any)=>s.name).sort()).toEqual([...names].sort())
  expect(await ctx.skills.list()).toEqual([])
  for(const summary of data.registration_snapshots[index]){
   const skill=await ctx.skills.get(summary.name,{scope:agent,signal})
   expect(skill).toBeDefined()
   expect({name:skill!.name,description_utf16:Array.from({length:skill!.description.length},(_,i)=>skill!.description.charCodeAt(i)),source:skill!.source,provider:skill!.provider,resource_base:skill!.resourceBase??null}).toEqual(summary)
  }
  report.registration_snapshots++
 }
 async function load(index:number,name:string){
  const value=await loader.execute({name:'dsh-superagent-'+name},{agent,signal})
  const rendered=loader.output.render({},value)[0].text
  expect(data.durable_tool_results[index].metadata).toEqual(value)
  expect(data.durable_tool_results[index].content).toBe(rendered)
  expect(data.loaded[index]).toBe(rendered)
  for (const request of data.requests.slice(index+1)) expect(request.messages.filter((m:any)=>m.role==='tool')[index].content).toBe(rendered)
  report.model_outputs_compared++
  report.durable_outcomes_compared++
 }
 try{
  await sessions.decorate(agent)
  expect(sections).toHaveLength(1)
  expect(store.get('session-prompts:main')).toBeUndefined()
  await snapshot(0)
  expect(data.registration_snapshots[0][1].description_utf16.at(-1)).toBe(0xd83d)
  for(const request of data.requests)expect(request.tools[0]).toEqual({name:loader.name,description:loader.description,parameters:loader.parameters})
  sections[0].text({signal});await load(0,'custom')
  prompts.save(data.configs[1],1,'fixture')
  await snapshot(0,prompts.version(1)) // current version changes, but no new model request yet
  sections[0].text({signal});await snapshot(1);await load(1,'custom')
  await expect(loader.execute({name:'dsh-superagent-old'},{agent,signal})).rejects.toThrow('unknown or no longer available')
  feishu=true
  await fiber.dispose();fiber=ctx.plugin(SkillRegistry);await fiber
  let ready=false
  for(let i=0;i<100;i++){if((await ctx.skills.list({scope:agent})).some((s:any)=>s.name==='dsh-superagent-feishu')){ready=true;break}await new Promise(resolve=>setImmediate(resolve))}
  expect(ready).toBe(true)
  await snapshot(2);await load(2,'feishu');await load(3,'github')
  expect(store.get('session-prompts:main')).toEqual(data.revisions)
  expect(report.model_outputs_compared).toBe(4);report.model_rendering_exact=true
  sessions.dispose();await scope.dispose()
  expect(await ctx.skills.list({scope:agent})).toEqual([])
 }finally{sessions.dispose();await scope.dispose();await ctx.fiber.dispose();store.close();rmSync(path,{recursive:true,force:true})}
})
