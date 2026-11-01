/** Independent reference/target Passes with recording ports; no expected value
 * is produced by Rust or replayed as a native callback response. */
import { afterAll, afterEach, beforeEach, expect, it, vi } from 'vitest'
import { Passes as Reference } from './upstream/src/app/passes.ts'
import { RustPasses, advancePassClocks, cleanupPasses } from './passes-target-adapter.ts'
const advance=vi.advanceTimersByTimeAsync.bind(vi)
vi.spyOn(vi,'advanceTimersByTimeAsync').mockImplementation(ms=>advancePassClocks(ms,advance))
const trigger={id:'t1',source:'feishu',threads:[{source:'feishu',id:'chat:one'}],actor:{source:'feishu',id:'person'},human:true,at:'2026-10-01T00:00:00.000Z',text:'hi',meta:{}}
function fixture(Type:any) {
 const h:any={trace:[],value:{state:'starting',detail:''},configured:true,closing:false,errors:[],mode:'stored',received:[],evalHook:undefined,kickHook:undefined,idleHook:undefined,controller:new AbortController(),spawned:[]}
 const host={configured:()=>{if(h.configuredError)throw h.configuredError;return h.configured},closing:()=>{if(h.closingError)throw h.closingError;return h.closing},lifetime:h.controller.signal,
  engine:{receive:(value:any)=>{h.trace.push('receive');h.received.push(value.id);if(h.receiveError)throw h.receiveError;return h.mode},evaluateDue:()=>{h.trace.push('eval');h.evalHook?.();if(h.errors.length){const error=h.errors.shift();if(error!==null)throw error}}},
  effects:{kick:()=>{h.trace.push('kick');h.kickHook?.()},idle:()=>{h.trace.push('idle');return h.idleHook?.()??Promise.resolve()}},
  health:{get:()=>h.value,set:(value:any)=>{h.trace.push(`health:${value.state}`);h.value=value}}}
 h.passes=new Type(host);return h
}
const pairs:any[]=[]
function pair(){const pair=[fixture(Reference),fixture(RustPasses)];pairs.push(pair);return pair}
async function idle(pair:any[]){await Promise.all(pair.map(h=>h.passes.idle()))}
function same(pair:any[]){for(const key of ['trace','value','received'])expect(pair[1][key]).toEqual(pair[0][key])}
function normalized(slots:any[]){return slots.map(slot=>slot.status==='fulfilled'?{status:'fulfilled'}:{status:'rejected',reason:String(slot.reason)})}
beforeEach(()=>vi.useFakeTimers({toFake:['Date','setInterval','clearInterval'],now:Date.parse('2026-11-01T00:00:00.000Z')}))
afterEach(async()=>{for(const p of pairs.splice(0))await Promise.all(p.map(h=>h.passes.stop()));cleanupPasses();vi.useRealTimers()})
afterAll(()=>vi.restoreAllMocks())

it('matches eager evaluation and four simultaneous caller coalescing',async()=>{
 const p=pair();const waits=p.map(h=>[h.passes.pump(),h.passes.pump(),h.passes.pump(),h.passes.pump()]);
 for(const h of p)expect(h.trace).toEqual(['kick','eval'])
 await Promise.all(waits.flat());await idle(p);same(p);expect(p[0].trace.filter((s:string)=>s==='eval')).toHaveLength(2)
})
it('matches requested failure, direct success not recovering health, then requested recovery',async()=>{
 const p=pair();for(const h of p){h.errors.push(new Error('database is locked'));h.passes.request()}await idle(p);same(p)
 await Promise.all(p.map(h=>h.passes.pump()));same(p);expect(p[0].value.state).toBe('error')
 for(const h of p)h.passes.request();await idle(p);same(p);expect(p[0].value.state).toBe('ready')
})
it('runs a coalesced followup after rejection and returns both stop settlements',async()=>{
 const p=pair();const results=await Promise.all(p.map(async h=>{h.errors.push('raw failure');const one=h.passes.pump(),two=h.passes.pump(),stop=h.passes.stop();const slots=await Promise.allSettled([one,two]);return {slots:normalized(slots),stop:normalized(await stop)}}));
 expect(results[1]).toEqual(results[0]);same(p)
})
it('matches receive closing, duplicate, filtered and stored boundaries',async()=>{
 const p=pair();for(const h of p){h.closing=true;expect(()=>h.passes.receive(trigger)).toThrow('Orchestrator is stopping');h.closing=false;h.mode='duplicate';expect(h.passes.receive(trigger)).toBe('duplicate');h.mode='filtered';expect(h.passes.receive(trigger)).toBe('filtered')}
 await idle(p);same(p);for(const h of p){h.mode='stored';expect(h.passes.receive(trigger)).toBe('stored')}await idle(p);same(p)
})
it('propagates a synchronous receive failure without requesting a pass',async()=>{
 const p=pair();for(const h of p){h.receiveError=new Error('write failed');expect(()=>h.passes.receive(trigger)).toThrow('write failed')}await idle(p);same(p)
})
it('checks closing again after the effects kick',async()=>{
 const p=pair();for(const h of p)h.kickHook=()=>{h.closing=true};await Promise.all(p.map(h=>h.passes.pump()));await idle(p);same(p);expect(p[0].trace).not.toContain('eval')
})
it('matches setup skipped work and the error-recovery request callback',async()=>{
 const p=pair();for(const h of p){h.configured=false;h.value={state:'error',detail:'previous failure'};h.passes.request()}await idle(p);same(p);expect(p[0].trace).not.toContain('kick');expect(p[0].value.state).toBe('ready')
})
it('does not let stopped alone suppress on-demand pumps or receives',async()=>{
 const p=pair();await Promise.all(p.map(h=>h.passes.stop()));await Promise.all(p.map(h=>h.passes.pump()));for(const h of p)h.passes.receive(trigger);await idle(p);same(p)
})
it('does not clear an error after lifetime abortion, but still reports new failure',async()=>{
 const p=pair();for(const h of p){h.controller.abort();h.errors.push(new Error('after abort'));h.passes.request()}await idle(p);for(const h of p)h.passes.request();await idle(p);same(p);expect(p[0].value.state).toBe('error')
})
it('matches synchronous evaluation reentry before the pumping assignment',async()=>{
 const p=pair();for(const h of p){let entered=false;h.evalHook=()=>{if(!entered){entered=true;h.spawned.push(h.passes.pump())}}}
 await Promise.all(p.map(h=>h.passes.pump()));await Promise.all(p.flatMap(h=>h.spawned));await idle(p);same(p);expect(p[0].trace.filter((s:string)=>s==='eval')).toHaveLength(2)
})
it('propagates effects idle rejection without rewriting runtime health',async()=>{
 const p=pair();for(const h of p)h.idleHook=()=>Promise.reject(new Error('effects idle failed'))
 const results=await Promise.allSettled(p.map(h=>h.passes.idle()));expect(normalized(results)[1]).toEqual(normalized(results)[0]);same(p)
})
it('waits for a held effects idle while stop itself does not wait effects',async()=>{
 const p=pair();const releases:(()=>void)[]=[];for(const h of p)h.idleHook=()=>new Promise<void>(resolve=>releases.push(resolve));
 const waits=p.map(h=>h.passes.idle());for(let i=0;releases.length<2&&i<20;i++)await Promise.resolve();expect(releases).toHaveLength(2)
 await Promise.all(p.map(h=>h.passes.stop()));for(const resolve of releases)resolve();await Promise.all(waits);same(p)
})
it('starts at 2000ms, not immediately or at 1000ms, and advances through positive timer ticks',async()=>{
 const p=pair();for(const h of p)h.passes.start();await vi.advanceTimersByTimeAsync(1999);same(p);expect(p[0].trace).toEqual([])
 await vi.advanceTimersByTimeAsync(1);await idle(p);same(p);expect(p[0].trace.filter((s:string)=>s==='eval')).toHaveLength(1)
 await vi.advanceTimersByTimeAsync(8000);await idle(p);same(p);expect(p[0].trace.filter((s:string)=>s==='eval')).toHaveLength(5)
})
it('gates periodic work while setup/closing is false and retains request health timestamps',async()=>{
 const p=pair();for(const h of p){h.configured=false;h.value={state:'error',detail:'old'};h.passes.start()}await vi.advanceTimersByTimeAsync(4000);await idle(p);same(p)
 for(const h of p){h.configured=true;h.errors.push(new Error('tick failure'))}await vi.advanceTimersByTimeAsync(2000);await idle(p);same(p)
 await vi.advanceTimersByTimeAsync(2000);await idle(p);same(p);for(const h of p)h.closing=true;await vi.advanceTimersByTimeAsync(2000);await idle(p);same(p)
})
it('retains the reference repeated-start/latest-handle-only stop quirk',async()=>{
 const p=pair();for(const h of p)h.passes.start();await vi.advanceTimersByTimeAsync(1000);for(const h of p)h.passes.start();await vi.advanceTimersByTimeAsync(2000);await idle(p);same(p)
 await Promise.all(p.map(h=>h.passes.stop()));await vi.advanceTimersByTimeAsync(1000);await idle(p);same(p);expect(p[0].trace.filter((s:string)=>s==='eval')).toHaveLength(3)
})
it('keeps independent error reasons for many coalesced request/direct-pump batches',async()=>{
 const p=pair();for(let step=0;step<32;step++){
  const waits:any[]=[];for(const h of p){if(step%3===0)h.errors.push(new Error('batch '+step));h.passes.request();waits.push(h.passes.pump().catch((error:any)=>String(error)));if(step%2===0)h.passes.request()}
  await Promise.all(waits);await idle(p);same(p)
 }
})
it('does not publish a coalesced requested failure before an earlier direct pump await resumes',async()=>{
 const p=pair();const snapshots=await Promise.all(p.map(async h=>{
  const first=h.passes.pump();h.errors.push(new Error('followup failure'));h.passes.request();
  await first;const early={health:{...h.value},trace:[...h.trace]};
  await h.passes.idle();return {early,final:{health:{...h.value},trace:[...h.trace]}}
 }));expect(snapshots[1]).toEqual(snapshots[0]);same(p)
})

it('propagates configured/closing callback failures through pump and request promises',async()=>{
 const p=pair();for(const h of p)h.configuredError=new Error('configuration broken')
 let results=await Promise.allSettled(p.map(h=>h.passes.pump()));expect(normalized(results)[1]).toEqual(normalized(results)[0]);
 for(const h of p)h.passes.request();await idle(p);same(p)
 for(const h of p){h.configuredError=undefined;h.closingError=new Error('closing read failed')}
 results=await Promise.allSettled(p.map(h=>h.passes.pump()));expect(normalized(results)[1]).toEqual(normalized(results)[0]);
 for(const h of p)h.passes.request();await idle(p);same(p);for(const h of p)h.closingError=undefined
})
it('catches kick failure and a synchronously throwing effects idle port',async()=>{
 const p=pair();for(const h of p)h.kickHook=()=>{throw new Error('kick failed')};for(const h of p)h.passes.request();await idle(p);same(p)
 for(const h of p)h.idleHook=()=>{throw new Error('sync idle failure')};const results=await Promise.allSettled(p.map(h=>h.passes.idle()));expect(normalized(results)[1]).toEqual(normalized(results)[0]);same(p)
})
