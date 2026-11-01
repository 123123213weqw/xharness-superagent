/** Independent traces run both machines; no expected classifier sent to Rust. */
import {afterEach,expect,it} from 'vitest'
import {RetryHealth as Reference} from './upstream/src/app/retry-health.ts'
import {RustRetryHealth,cleanupRetryHealth} from './retry-health-target-adapter.ts'
afterEach(cleanupRetryHealth)
const retry=(seq:number,changes:any={},time=seq)=>({type:'llm/retry',seq,time,data:{turn:0,step:0,retryId:'chain',provider:'provider',retry:1,failure:{type:'budget_exceeded'},...changes}})
function compare(events:any[]){const a=new Reference(),b=new RustRetryHealth();for(const e of events){a.observe('id',e);b.observe('id',e);expect(b.list()).toEqual(a.list());expect(b.get('id')).toEqual(a.get('id'))}return b.list()}
it.each([0,0.9,1.9,253402300799999,253402300800000,8.64e15])('ISO TimeClip at %s',time=>{compare([retry(0,{},time)])})
it.each([['401'],[['403']],['0x1ad'],'\ufeff401\u00a0','+403.0e0','0b110101101','0o655','429junk','+0x191',{},null])('Number(status) coercion %j',status=>{compare([retry(1,{failure:{type:'budget_exceeded',status}})])})
it.each([
 'xBudget has been exceeded!','_Budget has been exceeded!','éBudget has been exceeded!','Budget has been exceeded?','BUDGET HAS BEEN EXCEEDED.',
 ' '.repeat(8192-'Budget has been exceeded'.length)+'Budget has been exceeded'+'ignored beyond UTF16 boundary',
 '😀'.repeat(4083)+'Budget has been exceeded!',
])('ASCII word boundaries / UTF16 prefix %#',message=>{compare([retry(1,{failure:{message}})])})
it.each([
 {error:{},type:'budget_exceeded'}, {error:[],type:'budget_exceeded'},
 {error:{code:'budget_exceeded'},type:'context'}, {error:null,code:'budget_exceeded'},
])('error record shadows outer predicate %j',body=>{compare([retry(1,{failure:{message:JSON.stringify(body)}})])})
it('watermark precedes data validation and clear never creates a missing session',()=>{
 const a=new Reference(),b=new RustRetryHealth();for(const h of [a,b]){h.clear('absent');h.observe('z',retry(1));h.observe('a',retry(1));h.observe('z',{type:'unknown',seq:9,time:9,data:null});h.observe('z',retry(8,{retry:8}));h.restore('z',[retry(1)]);h.clear('a');h.observe('a',retry(10,{retry:10}));h.observe('a',retry(11,{step:1}));}expect(b.list()).toEqual(a.list());expect(b.list().map((s:any)=>s.sessionId)).toEqual(['a','z'])
})
it.each([
 '{"ignored":"\\ud800","type":"budget_exceeded"}',
 '{"\\udfff":"irrelevant","error":{"code":"budget_exceeded"}}',
 '{"ignored":'+ '['.repeat(1500)+'null'+']'.repeat(1500)+',"type":"budget_exceeded"}',
 '{"ignored":1e9999,"type":"budget_exceeded"}',
 '{"type":"budget_exceeded","type":null}',
 '{"error":{"type":"budget_exceeded"},"error":null,"type":"none"}',
 '{"error":{"type":"budget_exceeded"},"error":[] ,"type":"budget_exceeded"}',
 '{"type":"budget_exceeded",}', '{"type":"budget_exceeded"} trailing',
 '{"ignored":[1,],"type":"budget_exceeded"}',
 '{"ignored":01,"type":"budget_exceeded"}',
 '{"ignored":.1,"type":"budget_exceeded"}',
 '{"ignored":truex,"type":"budget_exceeded"}',
 '{"ignored":"\\q","type":"budget_exceeded"}',
])('bounded JSON.parse projection %#',message=>{compare([retry(1,{failure:{message}})])})
it('keeps an exact UTF16 boundary and never treats its sliced high surrogate as EOF',()=>{
 const phrase='Budget has been exceeded'
 const exact=' '.repeat(8192-phrase.length)+phrase+'tail'
 expect(compare([retry(1,{failure:{message:exact}})])).toHaveLength(1)
 const half=' '.repeat(8191-phrase.length)+phrase+'😀'
 expect(compare([retry(1,{failure:{message:half}})])).toHaveLength(0)
})

// Execute the EXACT reference private formatter with an isolated host sink;
// constructor registration has no active Source/Agent/provider side effects.
it.each([0,1,6,7])('exact Sessions models HealthEntry for %s waits',async count=>{
 const {Sessions}=await import('./upstream/src/app/sessions.ts')
 const reference=new Reference(),target=new RustRetryHealth()
 for(let i=0;i<count;i++){
  const event=retry(1,{provider:i===1?'bad provider secret':'offline',retry:i+1},(count-i)*1000)
  reference.observe(`worker-${i}`,event);target.observe(`worker-${i}`,event)
 }
 let actual:any
 const sessions=new Sessions({context:{on(){}},effects:{handle(){}},health:{set(value:any){actual=value}}}as any)
 ;(sessions as any).retryHealth=reference
 ;(sessions as any).updateModelHealth()
 expect(target.modelEntry()).toEqual(actual)
 expect(JSON.stringify(target.modelEntry())).not.toMatch(/PRIVATE|bad provider secret/)
 sessions.dispose()
})
