/** Independent original modules (NO vi.mock): original Node SQLite/Prompts vs
 * production Rust Store/Prompts. Covers full output bytes and typed errors. */
import {afterAll,afterEach,beforeAll,it,expect,vi} from 'vitest'
import {mkdtempSync,rmSync,writeFileSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join,relative} from 'node:path'
import * as reference from './upstream/src/app/prompts.ts'
import {promptFile as originalFile} from './upstream/src/app/skill-files.ts'
import {packagePath} from './upstream/src/infra/paths.ts'
import {Store} from './upstream/src/app/store.ts'
import {SettingsSchema} from './upstream/src/policy.ts'
import {RustPrompts,defaultPrompts,builtInParts,builtInBase,composePrompt,promptParts,policySection,nativeErrorMessage,readPromptFile} from './prompts-target-adapter.ts'
import {RustDatabaseSync,createStoreAdapter,cleanupStoreClients,saveStoreEvidence} from './store-target-adapter.ts'
const RustStore=createStoreAdapter(RustDatabaseSync)
const report:any={reference_commit:'6337fc823ce20251c8de8085948165c81aaa701e',valid_compositions:0,version_operations:0,error_cases:0,errors:[],asset_parts:0,file_inputs:0,scope:'independent original Prompts+Node SQLite vs production Rust Prompts+SQLite; no native request/skills/Root installer claim'}
beforeAll(()=>{vi.useFakeTimers({toFake:['Date']});vi.setSystemTime(new Date('2026-01-02T03:04:05.000Z'))})
afterEach(cleanupStoreClients)
afterAll(()=>{vi.useRealTimers();const path=process.env.SUPERAGENT_PROMPTS_DIFFERENTIAL_EVIDENCE;if(path)writeFileSync(path,JSON.stringify(report,null,2)+'\n');saveStoreEvidence('-differential',report.scope)})
function fixture(body:(a:any,b:any,as:any,bs:any)=>void){
 const dir=mkdtempSync(join(tmpdir(),'prompt-diff-'));const a=new Store(join(dir,'reference'));const b=new RustStore(join(dir,'rust'))
 const env=()=>[{key:'DIR',value:'/srv/{mode}/$&/😃',description:{zh:'目录',en:'The directory'},owner:'executor'}]
 try{body(new reference.Prompts(a,()=>100000,env,()=>true),new RustPrompts(b,()=>100000,env,()=>true),a,b)}finally{a.close();b.close();rmSync(dir,{recursive:true,force:true})}
}
function signature(f:()=>unknown){try{return{value:f()}}catch(e:any){return e.issues?{issues:e.issues}:e.code?{code:e.code,values:e.values,message:nativeErrorMessage(e)??e.message}:{type:e.constructor.name,message:e.message}}}
it('matches every original built-in text/base and independent complete role compositions',()=>{
 const original=reference.builtInParts();expect(builtInParts()).toEqual(original);expect(defaultPrompts()).toEqual(reference.defaultPrompts())
 for(const [name,p]of Object.entries(original)){expect(builtInBase(p.text),name).toBe(reference.builtInBase(p.text));report.asset_parts++}
 const samples=['','\n  Team ${{ literal }}\n','😃\u2028\u0085\ufeff','{version} {mode} {rules} $&','One.\nTwo.']
 const settings=[SettingsSchema.parse({}),SettingsSchema.parse({policy:{rules:[]}}),SettingsSchema.parse({eval:{mode:'auto'},policy:{rules:[{id:'a',name:'A\n B',enabled:true,instruction:'Keep\n\n {mode} $&'},{id:'off',name:'Off',enabled:false,instruction:'No'}]}})]
 for(let i=0;i<30;i++){
  const config:any={...reference.defaultPrompts(),communication:samples[i%samples.length],scope:samples[(i+1)%samples.length],delivery:samples[(i+2)%samples.length],skills:{zeta:'Z',alpha:'A'}}
  const part=Object.keys(original)[i%15];config.overrides={[part]:{text:'  Changed '+i+' ${{ literal }} '+((original as any)[part].placeholders??[]).join(' ')}}
  for(const role of ['orchestrator','worker']as const)for(const env of [undefined,'',`E ${i}\n{mode}`]){
   const a=reference.policySection(settings[i%3],config),b=policySection(settings[i%3],config);expect(b).toEqual(a)
   expect(promptParts(config,role,env,b)).toEqual(reference.promptParts(config,role,env,a));expect(composePrompt(config,role,env,b)).toBe(reference.composePrompt(config,role,env,a));report.valid_compositions++
  }
 }
})
it('matches immutable saves, history, rollback, skills, default rebasing and restart-visible KV',()=>fixture((a,b,as,bs)=>{
 for(let i=0;i<12;i++){
  const cfg:any={...reference.defaultPrompts(),scope:`Scope ${i}`,skills:i%2?{alpha:'A',zeta:'Z'}:{zeta:'Z',alpha:'A'},overrides:{'runtime/shared':{text:'  Edited ${{ x }}\n',...(i%3===0?{base:'earlier'}:{})}}}
  expect(b.validate(cfg)).toEqual(a.validate(cfg));expect(b.preview(cfg)).toEqual(a.preview(cfg));expect(b.skills(cfg)).toEqual(a.skills(cfg));expect(b.defaultChanged(cfg)).toEqual(a.defaultChanged(cfg))
  const av=a.save(cfg,i,i%2?'tool':'panel'),bv=b.save(cfg,i,i%2?'tool':'panel');expect(bv).toEqual(av);expect(b.current()).toEqual(a.current());expect(b.history(i%4,i%5)).toEqual(a.history(i%4,i%5));report.version_operations++
  expect(bs.get('prompt-version:'+i)).toEqual(as.get('prompt-version:'+i))
 }
 expect(b.save(b.version(0).config,12,'panel')).toEqual(a.save(a.version(0).config,12,'panel'));report.version_operations++
 expect(signature(()=>b.save(reference.defaultPrompts(),0,'panel'))).toEqual(signature(()=>a.save(reference.defaultPrompts(),0,'panel')))
}))
it('matches complete structured schema issues and semantic errors without JS validation substitution',()=>fixture((a,b)=>{
 const valid=reference.defaultPrompts(), errors:any[]=[null,false,3,'',[],{}, {...valid,other:1},{...valid,skills:null},{...valid,skills:[]},{...valid,communication:3,scope:null,delivery:false,skills:{'Bad Name':3}},{communication:'x',scope:''}]
 for(const name of ['','Bad Name','github','feishu','1abc','__proto__','constructor','é','ok-2'])for(const value of [null,3,'text'])errors.push({...valid,skills:{[name]:value}})
 for(const value of [null,[],3,{}, {text:3},{text:'x',base:null},{text:'x',extra:1},{text:'x',base:3},{text:'x'}, {text:' \ufeff\n '}])errors.push({...valid,overrides:{'runtime/shared':value}})
 for(const name of ['constructor','toString','__proto__','runtime/policy','runtime/environment'])errors.push({...valid,overrides:JSON.parse(JSON.stringify({[name]:{text:'x'}}))})
 const small={communication:'c',scope:'s',delivery:'d',skills:{}}
 for(const key of ['__proto__','constructor','toString','a"b','a\\b','\n','0','😃']){
  errors.push({...small,...JSON.parse(JSON.stringify({[key]:3}))})
  errors.push({...small,overrides:{'runtime/shared':{text:3,base:null,[key]:true}}})
 }
 for(const name of ['ok\n','ok\r','ok\r\n','ok\u2028','ok\u2029'])errors.push({...small,skills:{[name]:'x'}})
 errors.push({...small,overrides:null},{...small,overrides:[]},{...small,overrides:3})
 for(let i=0;i<errors.length;i++){const expected=signature(()=>a.validate(errors[i])),actual=signature(()=>b.validate(errors[i]));report.error_cases++;if(JSON.stringify(actual)!==JSON.stringify(expected))report.errors.push({index:i,input:errors[i],expected,actual});}
 expect(report.errors).toEqual([])
}))

it('matches original packaged UTF8 replacement and line normalization on actual byte files',()=>{
 const dir=mkdtempSync(join(tmpdir(),'prompt-bytes-'))
 try{const inputs=[Buffer.from('One.\r\nTwo.\r\n \t\r\n\r\nThree.\r\n'),Buffer.from([0x61,0x80,0x62]),Buffer.from([0xe2,0x82]),Buffer.from([0xed,0xa0,0x80]),Buffer.from([0xf0,0x80,0x80,0x80]),Buffer.from([0xf4,0x90,0x80,0x80]),Buffer.from([0xef,0xbb,0xbf,0x61]),Buffer.from([0xe2,0x28,0xa1])]
 for(let i=0;i<inputs.length;i++){const path=join(dir,`case-${i}.md`);writeFileSync(path,inputs[i]);expect(readPromptFile(path)).toBe(originalFile(relative(packagePath('prompts'),path.slice(0,-3))));report.file_inputs++}
 }finally{rmSync(dir,{recursive:true,force:true})}
})
