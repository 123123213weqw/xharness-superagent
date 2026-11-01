/** Offline actual Native Rust traces vs original Prompts/Mandate/environment and
 * the lock-pinned real DSH system-prompt renderer. No vi.mock and no Rust oracle. */
import {it,expect,afterAll} from 'vitest'
import {readFileSync,writeFileSync,mkdtempSync,rmSync} from 'node:fs'
import {join} from 'node:path'
import {tmpdir} from 'node:os'
import {Prompts} from './upstream/src/app/prompts.ts'
import {Sessions} from './upstream/src/app/sessions.ts'
import {Store} from './upstream/src/app/store.ts'
import {mandateInstructions} from './upstream/src/executor/dsh.ts'
import {environmentOf,ticketEnvironment} from './upstream/src/app/execution-environment.ts'
import {renderPrompt} from './upstream/node_modules/@deepseek-ai/dsh-system-prompt/lib/index.js'
const dir=process.env.NATIVE_PROMPT_TRACE_DIR!
const report={reference_commit:'6337fc823ce20251c8de8085948165c81aaa701e',sdk:'@deepseek-ai/dsh-system-prompt@0.2.1-alpha.2',native_provider_requests:0,main:0,worker:0,physical_harness_audits:0,original_sessions_decoration_requests:0,no_mocks:true,scope:'offline Native request composition and physical Harness snapshots, original Sessions.decorate boundary with fake Cordis/tool adapters; not live skills, Root, Source registry/deployment or all-phase refresh'}
afterAll(()=>{writeFileSync(process.env.NATIVE_PROMPT_ORACLE_EVIDENCE!,JSON.stringify(report,null,2)+'\n')})
function fixture(role:'main'|'worker',fn:(data:any,prompts:Prompts)=>void){const path=mkdtempSync(join(tmpdir(),'native-oracle-'));const store=new Store(join(path,'app.sqlite'));try{const data=JSON.parse(readFileSync(join(dir,role+'.json'),'utf8'));expect(store.settings()).toEqual(data.settings);const env=()=>environmentOf({deployment:{} as any,sources:{environments:()=>[]} as any,settings:data.settings});fn(data,new Prompts(store,()=>100000,env,()=>false))}finally{store.close();rmSync(path,{recursive:true,force:true})}}
function check(data:any,i:number,sections:any[]){const expected=renderPrompt({sections,variables:{}} as any);expect(data.trace.systems[i]).toBe(expected);expect(data.trace.harness_headers[i].system).toBe(expected);expect(data.trace.normal_replay_plaintext_omitted).toBe(true);report.native_provider_requests++;report.physical_harness_audits++}
it('matches both actual Native Main generations with latest original Prompt configs and complete literal product text',()=>fixture('main',(data,p)=>{for(let i=0;i<2;i++){const body=`[dsh-superagent prompt revision ${i+1}]\n${p.compose(data.configs[i],'orchestrator')}\n`;check(data,i,[{name:'dsh-superagent-policy',text:body,interpolate:false}]);report.main++}expect(data.revisions).toEqual([1,2])}))
it('matches actual Native Worker host base, live inferred/explicit Mandate, real executor variable definitions and deleted-ticket fallback',()=>fixture('worker',(data,p)=>{for(let i=0;i<3;i++){const vars=i<2?ticketEnvironment({deployment:{} as any,settings:data.settings,ticket:data.tickets[i],trigger:data.triggers[i],sources:{environments:()=>[{kind:{kind:'github'},config:{},environment:{variables:[{key:'GITHUB_REPO',description:{zh:'仓库',en:'The repository'}}],values:({trigger}:any)=>({GITHUB_REPO:trigger.repo})}}]} as any}):undefined;const mandate=i<2?mandateInstructions(data.tickets[i].mandate):'';const revision=i===0?0:1;const body=`[dsh-superagent prompt revision ${revision}]\n${p.compose(data.configs[revision],'worker',vars)}\n${mandate}`;check(data,i,[{name:'host-base',text:data.bases[i],interpolate:false},{name:'dsh-superagent-policy',text:body,interpolate:false}]);report.worker++}expect(data.revisions).toEqual([0,1])}))

// Exercise the ORIGINAL Sessions decoration boundary, not a reimplementation of
// its text callback. Only unrelated tools and Cordis service adapters are fake.
function decorationFixture(name:string, fn:(data:any,store:Store,prompts:Prompts,h:any)=>Promise<void>) {
  const path=mkdtempSync(join(tmpdir(),'decoration-oracle-'));
  const store=new Store(join(path,'app.sqlite'));
  const data=JSON.parse(readFileSync(join(dir,name+'.json'),'utf8'));
  const prompts=new Prompts(store,()=>100000,()=>environmentOf({deployment:{} as any,sources:{environments:()=>[]} as any,settings:data.settings}),()=>false);
  const sections:any[]=[];
  const registeredSkills:any[]=[];
  let current:any;
  const context:any={on:()=>()=>{},logger:{warn:()=>{throw new Error('unexpected release failure')}}};
  const host:any={context,store:{tickets:{get:(id:string)=>{expect(id).toBe('t');return current}},settings:()=>store.settings(),get:store.get.bind(store),set:store.set.bind(store),keys:store.keys.bind(store)},prompts,effects:{handle:()=>()=>{}},health:{set:()=>{}},masterId:'main',lifetime:new AbortController().signal,closing:()=>false,tools:()=>[],environment:()=>{throw new Error('deleted Ticket must use original default environment')}};
  const sessions:any=new Sessions(host);
  sessions.registerTool=()=>()=>{};
  sessions.workOnly=()=>[];
  const agent:any={id:'worker-t',status:'idle',session:{header:{},snapshotEvents:()=>[]},ctx:{systemPrompt:{section:(section:any)=>{sections.push(section);return ()=>{const i=sections.indexOf(section);if(i>=0)sections.splice(i,1)}}},inject:(_:any,apply:any)=>{const disposers:(()=>void)[]=[];apply({skills:{register:(skill:any)=>{registeredSkills.push(skill);const d=()=>{const i=registeredSkills.indexOf(skill);if(i>=0)registeredSkills.splice(i,1)};disposers.push(d);return d}}});return Object.assign(Promise.resolve(),{dispose:async()=>{for(const d of disposers)d()}})}}};
  const h={sessions,agent,sections,registeredSkills,setTicket:(ticket:any)=>{current=ticket},setEnvironment:(f:any)=>{host.environment=f},decorate:()=>sessions.decorate(agent),render:()=>sections.map(s=>({...s,text:s.text({signal:new AbortController().signal})}))};
  return fn(data,store,prompts,h).finally(()=>{sessions.dispose();store.close();rmSync(path,{recursive:true,force:true})});
}
it('matches original Sessions.decorate when Ticket disappears after acknowledged decoration but before its first request',()=>decorationFixture('worker-before-first-deletion',async(data,store,p,h)=>{
  h.setTicket({id:'t',mandate:{strength:'inferred'}});
  await h.decorate();
  expect(h.sections).toHaveLength(1);
  expect(h.sections[0].interpolate).toBe(false);
  expect(h.registeredSkills.length).toBeGreaterThan(0);
  expect(store.get('session-prompts:worker-t')).toBeUndefined();
  expect(data.kv_absent_at_decoration).toBe(true);
  h.setTicket(undefined);
  check(data,0,[{name:'host-base',text:data.base,interpolate:false},...h.render()]);
  expect(store.get('session-prompts:worker-t')).toEqual(data.revisions);
  report.worker++;
  report.original_sessions_decoration_requests++;
}));
it('matches original skipped decoration until an explicit ensure/decorate, preserving current Host base and no premature KV',()=>decorationFixture('worker-initially-missing',async(data,store,p,h)=>{
  await h.decorate();
  expect(h.sections).toHaveLength(0);
  check(data,0,[{name:'host-base',text:data.bases[0],interpolate:false},...h.render()]);
  h.setTicket(data.ticket);
  check(data,1,[{name:'host-base',text:data.bases[1],interpolate:false},...h.render()]);
  expect(store.get('session-prompts:worker-t')).toBeUndefined();
  expect(data.kv_absent_before_explicit_ensure).toBe(true);
  await h.decorate();
  expect(h.sections).toHaveLength(1);
  expect(store.get('session-prompts:worker-t')).toBeUndefined();
  h.setEnvironment((ticket:any)=>ticketEnvironment({deployment:{} as any,settings:data.settings,ticket,trigger:data.trigger,sources:{environments:()=>[{kind:{kind:'github'},config:{},environment:{variables:[{key:'GITHUB_REPO',description:{zh:'仓库',en:'The repository'}}],values:({trigger}:any)=>({GITHUB_REPO:trigger.repo})}}]} as any}));
  check(data,2,[{name:'host-base',text:data.bases[2],interpolate:false},...h.render()]);
  expect(store.get('session-prompts:worker-t')).toEqual(data.revisions);
  report.worker+=3;
  report.original_sessions_decoration_requests+=3;
}));
