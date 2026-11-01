/** Independent recording ports. Expected decisions are never sent to Rust. */
import { afterEach, expect, it } from 'vitest'
import { Context } from './upstream/node_modules/@deepseek-ai/cordis/lib/index.js'
import { AgentDelivery as Reference } from './upstream/src/app/delivery.ts'
import { RustAgentDelivery, cleanupDelivery } from './delivery-target-adapter.ts'
afterEach(cleanupDelivery)
const deferred=<T=void>()=>{let resolve!:(value:T)=>void,reject!:(error:any)=>void;const promise=new Promise<T>((a,b)=>{resolve=a;reject=b});return{promise,resolve,reject}}
const event=(data:any)=>({type:'user/message',seq:0,time:0,surfaceOp:'append',data})
const old=(id:string,source:any)=>({id,role:'user',content:[],source})
function fixture(Delivery:any){
 const ctx=new Context(), trace:any[]=[],history:any[]=[], session={}, controller=new AbortController()
 const f:any={ctx,trace,history,session,controller,readHook:undefined,openHook:undefined,flushHook:undefined,closeHook:undefined,primitiveHook:undefined}
 const inbox={nextTurn:[]as any[],nextStep:[]as any[]}
 const agent:any={id:'same-id',session,inbox}
 for(const [name,target]of [['followup','nextTurn'],['steer','nextStep'],['inject','nextStep']]as const)agent[name]=(message:any)=>{f.primitiveHook?.(name,message);trace.push([name,message]);expect(Object.isFrozen(message)).toBe(true);expect(Object.isFrozen(message.source)).toBe(true);inbox[target].push(message)}
 const agents=new Map([[agent.id,agent]]);ctx.reflect.provide('agents',agents)
 ctx.reflect.provide('sessionPersistence',{
  flush:()=>{trace.push(['flush']);return f.flushHook?.()??Promise.resolve()},
  open:(id:string,mode:string)=>{trace.push(['open',id,mode]);if(f.openHook)return f.openHook();return Promise.resolve({read:(offset:number,length:number)=>{trace.push(['read',offset,length]);return f.readHook?.(offset,length)??Promise.resolve({events:history.slice(offset,offset+length)})},[Symbol.asyncDispose]:()=>{trace.push(['close']);return f.closeHook?.()??Promise.resolve()}})},
 })
 const delivery=new Delivery(ctx)
 const send=(id='request',mode:any='steer',authorize?:()=>void,accepted?:()=>void)=>delivery.send(agent,id,'payload',mode,controller.signal,authorize,accepted)
 const holds=(id='request')=>delivery.holds(agent,id)
 const consume=(message:any)=>{inbox.nextTurn.splice(0);inbox.nextStep.splice(0);const e=event(message);history.push(e);ctx.emit('session/event',session,e)}
 return Object.assign(f,{agent,agents,inbox,delivery,send,holds,consume})
}
const pair=()=>[fixture(Reference),fixture(RustAgentDelivery)]
const same=(p:any[])=>expect(p[1].trace).toEqual(p[0].trace)
const sends=(f:any)=>f.trace.filter((v:any)=>['followup','steer','inject'].includes(v[0]))
it('starts history flush in the hot prefix but defers all Agent methods until history settles',async()=>{
 const p=pair(),gates=p.map(()=>deferred());for(let i=0;i<p.length;i++)p[i].flushHook=()=>gates[i].promise
 const waits=p.map(f=>f.send());same(p);expect(p[0].trace).toEqual([['flush']]);for(const f of p)f.flushHook=undefined;gates.forEach(g=>g.resolve());await Promise.all(waits);same(p)
})
it('runs authorization then acceptance then ack flush, including duplicate sends',async()=>{
 const p=pair();for(let i=0;i<2;i++)await Promise.all(p.map(f=>f.send('request','queue',()=>f.trace.push(['authorize']),()=>f.trace.push(['accepted']))));same(p);expect(sends(p[0])).toHaveLength(1);expect(p[0].trace.slice(-3)).toEqual([['authorize'],['accepted'],['flush']])
})
it('preserves an authorization throw by identity and allows a subsequent authorized retry',async()=>{
 const p=pair();for(const f of p){const error=new Error('authorization denied');await expect(f.send('request','steer',()=>{throw error})).rejects.toBe(error);expect(sends(f)).toHaveLength(0)}same(p);await Promise.all(p.map(f=>f.send()));same(p)
})
it('preserves an acceptance throw after admission without rerouting duplicate input',async()=>{
 const p=pair();for(const f of p){const error=new Error('accepted callback failed');await expect(f.send('request','queue',undefined,()=>{throw error})).rejects.toBe(error);expect(f.trace.at(-1)[0]).toBe('followup')}same(p);await Promise.all(p.map(f=>f.send()));same(p);expect(sends(p[0])).toHaveLength(1)
})
it('does not replay cancellation-removed queue input as already consumed',async()=>{
 const p=pair();await Promise.all(p.map(f=>f.send('request','queue')));for(const f of p){f.inbox.nextTurn.splice(0);expect(await f.holds()).toBe(false)}await Promise.all(p.map(f=>f.send()));same(p);expect(sends(p[0])).toHaveLength(2)
})
it('retains inject seen identity after removal only for the same live Session',async()=>{
 const p=pair();await Promise.all(p.map(f=>f.send('notice',{inject:'summary'})));for(const f of p){f.inbox.nextStep.splice(0);expect(await f.holds('notice')).toBe(true)}await Promise.all(p.map(f=>f.send('notice')));same(p);expect(sends(p[0])).toHaveLength(1)
})
it('does not let a changed retry payload or mode replace an already pending message',async()=>{
 const p=pair();await Promise.all(p.map(f=>f.send('request','queue')));await Promise.all(p.map(f=>f.delivery.send(f.agent,'request','different text',{inject:'different'},f.controller.signal)));same(p);expect(sends(p[0])).toHaveLength(1)
})
it('matches raw identities with empty and colon-containing request IDs without Sessions hashing',async()=>{
 const p=pair();for(const id of ['',':','notice:x:hash','请求🦀'])await Promise.all(p.map(f=>f.send(id)));same(p);expect(sends(p[0]).map((v:any)=>v[1].id)).toEqual(['dsh-superagent:','dsh-superagent::','dsh-superagent:notice:x:hash','dsh-superagent:请求🦀'])
})
it('coalesces one initial history load across concurrent sends and holds',async()=>{
 const p=pair();for(const f of p){const results=await Promise.all([f.send(),f.send(),f.holds(),f.send('other','queue')]);expect(results[2]).toBe(true)}same(p);expect(p[0].trace.filter((v:any)=>v[0]==='open')).toHaveLength(1)
})
it('uses all event counts to advance pages and finds messages beyond 1024 events',async()=>{
 const p=pair();for(const f of p)f.history.push(...Array.from({length:1024},()=>({type:'other',data:{}})),event(old('legacy',{kind:'user',rpcId:'request'})))
 await Promise.all(p.map(f=>f.send()));same(p);expect(sends(p[0])).toHaveLength(0);expect(p[0].trace).toContainEqual(['read',1024,512]);expect(p[0].trace).toContainEqual(['read',1025,512])
})
it('keeps a live message observed during a held initial history read',async()=>{
 const p=pair(),started=p.map(()=>deferred()),gates=p.map(()=>deferred<any>());for(let i=0;i<p.length;i++)p[i].readHook=()=>{started[i].resolve();return gates[i].promise}
 const waits=p.map(f=>f.send());await Promise.all(started.map(s=>s.promise));for(let i=0;i<p.length;i++){p[i].consume(old('dsh-superagent:request',{kind:'plugin',plugin:'dsh-superagent'}));gates[i].resolve({events:[]})}await Promise.all(waits);same(p);expect(sends(p[0])).toHaveLength(0)
})
it('rejects disposed/replaced live Agent object after history even when its durable ID matches',async()=>{
 const p=pair(),started=p.map(()=>deferred()),gates=p.map(()=>deferred<any>());for(let i=0;i<p.length;i++)p[i].readHook=()=>{started[i].resolve();return gates[i].promise}
 const waits=p.map(f=>f.send('request','steer',()=>{throw new Error('should not authorize')}));const outcomes=Promise.allSettled(waits);await Promise.all(started.map(s=>s.promise));for(let i=0;i<p.length;i++){p[i].agents.set('same-id',{...p[i].agent});gates[i].resolve({events:[]})}const result=await outcomes;for(const r of result){expect(r.status).toBe('rejected');if(r.status==='rejected')expect(r.reason.message).toBe('Delivery target was disposed')}same(p)
})
it('rejects an already-aborted signal without even opening history, preserving its reason',async()=>{
 const p=pair();for(const f of p){const reason={custom:'abort'};f.controller.abort(reason);await expect(f.send()).rejects.toBe(reason);expect(f.trace).toEqual([])}same(p)
})
it('aborts after history without authorization or delivery and keeps the shared cache usable by holds',async()=>{
 const p=pair(),started=p.map(()=>deferred()),gates=p.map(()=>deferred<any>());for(let i=0;i<p.length;i++)p[i].readHook=()=>{started[i].resolve();return gates[i].promise}
 const reasons=p.map(()=>new Error('stopped')),waits=p.map(f=>f.send());const outcomes=Promise.allSettled(waits);await Promise.all(started.map(s=>s.promise));for(let i=0;i<p.length;i++){p[i].controller.abort(reasons[i]);gates[i].resolve({events:[]})}const results=await outcomes;results.forEach((r,i)=>{expect(r.status).toBe('rejected');if(r.status==='rejected')expect(r.reason).toBe(reasons[i])});expect(await p[0].holds()).toBe(false);expect(await p[1].holds()).toBe(false);same(p)
})
it('does not check abortion again after acceptance or roll back delivery during final flush',async()=>{
 const p=pair();await Promise.all(p.map(f=>f.send('request','queue',undefined,()=>{f.controller.abort();f.trace.push(['accepted'])})));same(p);expect(sends(p[0])).toHaveLength(1)
})
it('retries failed initial flush without opening or routing, preserving the thrown identity',async()=>{
 const p=pair();for(const f of p){const error=new Error('initial flush failed');f.flushHook=()=>Promise.reject(error);await expect(f.send()).rejects.toBe(error);expect(f.trace).toEqual([['flush']]);f.flushHook=undefined}await Promise.all(p.map(f=>f.send()));same(p)
})
it('retries failed history open without trying to dispose a nonexistent handle',async()=>{
 const p=pair();for(const f of p){const error=new Error('open failed');f.openHook=()=>Promise.reject(error);await expect(f.send()).rejects.toBe(error);expect(f.trace).not.toContainEqual(['close']);f.openHook=undefined}await Promise.all(p.map(f=>f.send()));same(p)
})
it('awaits a failing history disposer and retries cached loading, rather than acknowledging',async()=>{
 const p=pair();for(const f of p){const error=new Error('close failed');f.closeHook=()=>Promise.reject(error);await expect(f.send()).rejects.toBe(error);expect(sends(f)).toHaveLength(0);f.closeHook=undefined}await Promise.all(p.map(f=>f.send()));same(p)
})
it('keeps both errors when reading and asynchronous disposal fail together',async()=>{
 const p=pair();for(const f of p){const read=new Error('read failed'),close=new Error('close failed');f.readHook=()=>Promise.reject(read);f.closeHook=()=>Promise.reject(close);try{await f.send();throw new Error('must reject')}catch(error:any){expect(error).toBeInstanceOf(SuppressedError);expect(error.error).toBe(close);expect(error.suppressed).toBe(read)}f.readHook=undefined;f.closeHook=undefined}await Promise.all(p.map(f=>f.send()));same(p)
})
it('does not mark failed inject calls accepted or seen before a successful primitive',async()=>{
 const p=pair();for(const f of p){const error=new Error('primitive failed');f.primitiveHook=()=>{throw error};await expect(f.send('request',{inject:'summary'},undefined,()=>{throw new Error('must not accept')})).rejects.toBe(error);f.primitiveHook=undefined;expect(await f.holds()).toBe(false)}await Promise.all(p.map(f=>f.send('request',{inject:'summary'})));same(p);expect(sends(p[0])).toHaveLength(1)
})
it('keys the history cache by Session object rather than Agent object or durable ID',async()=>{
 const p=pair();await Promise.all(p.map(f=>f.send('notice',{inject:'summary'})));for(const f of p){f.inbox.nextStep.splice(0);const sameSession={...f.agent,inbox:{nextStep:[],nextTurn:[]}};f.agents.set(f.agent.id,sameSession);expect(await f.delivery.holds(sameSession,'notice')).toBe(true);const restoredSession={...sameSession,session:{}};f.agents.set(f.agent.id,restoredSession);expect(await f.delivery.holds(restoredSession,'notice')).toBe(false)}same(p)
})
it('preserves primitive thrown values as well as Error objects in authorization',async()=>{
 const p=pair();for(const f of p)for(const value of [null,undefined,0,'not an Error'])await expect(f.send('request','steer',()=>{throw value})).rejects.toBe(value);same(p);expect(sends(p[0])).toHaveLength(0)
})
it('does not lose earlier-page identities when a later page fails and the retried scan changes',async()=>{
 const p=pair();for(const f of p){const marker=event(old('dsh-superagent:request',{kind:'dsh-superagent'}));let failed=false;f.readHook=(offset:number)=>{if(offset===0)return Promise.resolve({events:[marker,...Array.from({length:511},()=>({type:'other',data:{}}))]});failed=true;return Promise.reject(new Error('second page failed'))};await expect(f.send()).rejects.toThrow('second page failed');expect(failed).toBe(true);f.readHook=undefined;await f.send();expect(sends(f)).toHaveLength(0)}same(p)
})
it('preserves bounded hot flush reentry without a registry borrow or a second delivery',async()=>{
 const p=pair();for(const f of p){let entered=false;f.flushHook=()=>{if(!entered){entered=true;f.nested=f.send('request','queue')}return Promise.resolve()}}
 const waits=p.map(f=>f.send());await Promise.all(waits);await Promise.all(p.map(f=>f.nested));same(p);expect(sends(p[0])).toHaveLength(1)
})
