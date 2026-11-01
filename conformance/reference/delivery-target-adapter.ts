/** Test transport only: original host methods, event streams and Promise
 * settlements cross FIFO. Rust owns every delivery/dedup/history decision. */
import { spawn, execFileSync } from 'node:child_process'
import { constants, openSync, closeSync, readSync, writeSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { performance } from 'node:perf_hooks'
import { freezeMessage } from './upstream/node_modules/@deepseek-ai/dsh-llm/lib/index.js'
const pause = new Int32Array(new SharedArrayBuffer(4))
const instances = new Set<RustAgentDelivery>()
const commands:Record<string,number>={}, callbacks:Record<string,number>={}
export class RustAgentDelivery {
 private directory:string; private input:number; private output:number; private child:any
 private bytes=Buffer.alloc(0); private closed=false; private scheduled=false
 private sequence=0; private nextPort=0; private nextCall=0; private nextError=0
 private agentKeys=new WeakMap<object,number>(); private sessionKeys=new WeakMap<object,number>()
 private agents=new Map<number,any>(); private handles=new Map<number,any>(); private calls=new Map<number,any>()
 private errors=new Map<number,any>(); private waits=new Map<number,{resolve:(v:any)=>void;reject:(e:any)=>void}>()
 constructor(private readonly ctx:any) {
  const binary=process.env.SUPERAGENT_DELIVERY_BRIDGE_BIN
  if(!binary)throw new Error('SUPERAGENT_DELIVERY_BRIDGE_BIN must name a prebuilt remote target')
  this.directory=mkdtempSync(join(tmpdir(),'native-delivery-'))
  const input=join(this.directory,'input'), output=join(this.directory,'output')
  execFileSync('mkfifo',['-m','600',input,output])
  this.input=openSync(input,constants.O_RDWR|constants.O_NONBLOCK)
  this.output=openSync(output,constants.O_RDWR|constants.O_NONBLOCK)
  this.child=spawn(binary,[input,output],{stdio:['ignore','ignore','inherit']})
  instances.add(this)
  ctx.on('session/event',(session:any,event:any)=>{if(!this.closed){this.rpc('observe',{session:this.session(session),event});this.progress()}})
 }
 private session(session:object) {let key=this.sessionKeys.get(session);if(key===undefined){key=++this.sequence;this.sessionKeys.set(session,key)}return key}
 private agent(agent:any) {
  let key=this.agentKeys.get(agent)
  if(key===undefined){key=++this.sequence;this.agentKeys.set(agent,key);this.agents.set(key,agent);this.rpc('agent',{key,session:this.session(agent.session),id:agent.id})}
  return key
 }
 private exception(error:any):any {
  if(error.identity!==undefined)return this.errors.get(error.identity)
  if(error.suppressed)return new SuppressedError(this.exception(error.error),this.exception(error.suppressed),'An error was suppressed during disposal.')
  if(error.display?.startsWith('Error: '))return new Error(error.display.slice(7))
  return error.display
 }
 private failure(error:any) {const identity=++this.nextError;this.errors.set(identity,error);return {display:String(error),identity}}
 private write(value:any) {
  const bytes=Buffer.from(JSON.stringify(value)+'\n');let offset=0;const until=performance.now()+20000
  while(offset<bytes.length){try{offset+=writeSync(this.input,bytes,offset)}catch(error:any){if(error.code!=='EAGAIN')throw error;if(performance.now()>until)throw new Error('delivery write timeout');Atomics.wait(pause,0,0,1)}}
 }
 private read():any {
  const until=performance.now()+20000
  for(;;){const newline=this.bytes.indexOf(10);if(newline>=0){const frame=this.bytes.subarray(0,newline);this.bytes=this.bytes.subarray(newline+1);return JSON.parse(frame.toString('utf8'))}
   const buffer=Buffer.allocUnsafe(65536)
   try{const length=readSync(this.output,buffer);if(length)this.bytes=Buffer.concat([this.bytes,buffer.subarray(0,length)]);else Atomics.wait(pause,0,0,1)}catch(error:any){if(error.code!=='EAGAIN')throw error;Atomics.wait(pause,0,0,1)}
   if(performance.now()>until)throw new Error(`delivery response timeout child=${this.child.pid}`)
   if(this.bytes.length>64*1024*1024)throw new Error('delivery frame limit')
  }
 }
 private port(invoke:()=>any,convert:(v:any)=>any=(v)=>v??null) {
  const id=++this.nextPort
  // Invoke the actual host synchronously; only its Promise settlement is sent
  // later. No JS retry, seen set, history scanning or Agent routing here.
  const promise=invoke()
  Promise.resolve(promise).then(value=>{if(!this.closed){this.rpc('settle',{id,value:convert(value)});this.progress()}},error=>{if(!this.closed){this.rpc('settle',{id,error:this.failure(error)});this.progress()}})
  return {pending:id}
 }
 private callback(method:string,args:any[]):any {
  callbacks[method]=(callbacks[method]??0)+1
  if(method==='flush')return this.port(()=>this.ctx.sessionPersistence.flush())
  if(method==='open')return this.port(()=>this.ctx.sessionPersistence.open(args[0],'read'),handle=>{const id=++this.sequence;this.handles.set(id,handle);return id})
  if(method==='read')return this.port(()=>this.handles.get(args[0]).read(args[1],args[2]))
  if(method==='dispose')return this.port(()=>this.handles.get(args[0])[Symbol.asyncDispose]())
  if(method==='signal')return this.calls.get(args[0]).signal.throwIfAborted()
  if(method==='authorize'||method==='accepted')return this.calls.get(args[0])[method]()
  const agent=this.agents.get(args[0])
  if(method==='isCurrent')return this.ctx.agents.get(agent.id)===agent
  if(method==='pending')return [...agent.inbox.nextTurn,...agent.inbox.nextStep]
  if(method==='steer'||method==='followup'||method==='inject')return agent[method](freezeMessage(args[1]))
  throw new Error(`unknown delivery callback ${method}`)
 }
 private rpc(method:string,args:any={}):any {
  commands[method]=(commands[method]??0)+1;this.write({kind:'command',method,...args})
  for(;;){const frame=this.read();if(frame.kind==='callback'){try{this.write({value:this.callback(frame.method,frame.args)??null})}catch(error){this.write({error:this.failure(error)})}}
   else if(frame.kind==='result')return frame.value;else throw new Error('unknown delivery frame')}
 }
 private progress() {
  if(this.scheduled||this.closed)return;this.scheduled=true
  queueMicrotask(()=>{this.scheduled=false;if(this.closed)return;const result=this.rpc('poll')
   for(const [id,value]of Object.entries<any>(result.completed)){const wait=this.waits.get(Number(id));if(!wait)throw new Error('unknown delivery completion');this.waits.delete(Number(id));if(value.error!==undefined)wait.reject(this.exception(value.error));else wait.resolve(value.value)}
   if(this.waits.size&&result.external_pending===0)this.progress()
  })
 }
 private wait(method:string,args:any) {const result=this.rpc(method,args);const promise=new Promise<any>((resolve,reject)=>this.waits.set(result.ticket,{resolve,reject}));this.progress();return promise}
 holds(agent:any,requestId:string):Promise<boolean>{return this.wait('holds',{agent:this.agent(agent),request:requestId})}
 send(agent:any,requestId:string,text:string,mode:'queue'|'steer'|{inject:string},signal:AbortSignal,authorize?:()=>void,accepted?:()=>void):Promise<void>{
  const call=++this.nextCall;this.calls.set(call,{signal,authorize,accepted})
  return this.wait('send',{agent:this.agent(agent),request:requestId,text,mode,call,authorize:!!authorize,accepted:!!accepted}).finally(()=>this.calls.delete(call))
 }
 close() {if(this.closed)return;this.closed=true;this.write({kind:'command',method:'close'});this.child.kill('SIGTERM');closeSync(this.input);closeSync(this.output);rmSync(this.directory,{recursive:true,force:true});instances.delete(this);for(const wait of this.waits.values())wait.reject(new Error('delivery test transport closed'));this.waits.clear()}
}
export function cleanupDelivery(){for(const instance of [...instances])instance.close()}
export function saveDeliveryEvidence(){if(process.env.SUPERAGENT_DELIVERY_EVIDENCE)writeFileSync(process.env.SUPERAGENT_DELIVERY_EVIDENCE,JSON.stringify({schema_version:1,scope:'Rust AgentDelivery protocol against original host ports, not the complete NativeSessions or DSH Agent machine. Original test callback counts only.',commands,callbacks},null,2)+'\n')}
