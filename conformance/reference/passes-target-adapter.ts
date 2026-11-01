/** Test transport only. Rust owns Passes scheduling, state, health decisions and
 * timers; JS supplies original observation callbacks and a fake-clock driver. */
import { spawn, execFileSync } from 'node:child_process'
import { constants, openSync, closeSync, readSync, writeSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { performance } from 'node:perf_hooks'
const sleep = new Int32Array(new SharedArrayBuffer(4))
const instances = new Set<RustPasses>()
const commands: Record<string,number> = {}, callbacks: Record<string,number> = {}
function exception(display: string): any { return display.startsWith('Error: ') ? new Error(display.slice(7)) : display }
function checked(result: any) { if (result.error !== undefined) throw exception(result.error); return result }
export class RustPasses {
  private directory: string
  private input: number
  private output: number
  private child: any
  private bytes = Buffer.alloc(0)
  private waits = new Map<number, { resolve:(value:any)=>void; reject:(error:any)=>void }>()
  private idleId = 0
  private scheduled = false
  private closed = false
  constructor(private readonly host: any) {
    const binary=process.env.SUPERAGENT_PASSES_BRIDGE_BIN
    if(!binary) throw new Error('SUPERAGENT_PASSES_BRIDGE_BIN must name a prebuilt remote target')
    this.directory=mkdtempSync(join(tmpdir(),'native-passes-'))
    const input=join(this.directory,'input'),output=join(this.directory,'output')
    execFileSync('mkfifo',['-m','600',input,output])
    this.input=openSync(input,constants.O_RDWR|constants.O_NONBLOCK)
    this.output=openSync(output,constants.O_RDWR|constants.O_NONBLOCK)
    this.child=spawn(binary,[input,output],{stdio:['ignore','ignore','inherit']})
    instances.add(this)
  }
  private write(value:any) {
    const bytes=Buffer.from(JSON.stringify(value)+'\n');let offset=0;const until=performance.now()+20000
    while(offset<bytes.length) { try {offset+=writeSync(this.input,bytes,offset)} catch(error:any) {if(error.code!=='EAGAIN')throw error;if(performance.now()>until)throw new Error('passes transport write timeout');Atomics.wait(sleep,0,0,1)} }
  }
  private read() {
    const until=performance.now()+20000
    for(;;) {
      const newline=this.bytes.indexOf(10)
      if(newline>=0) {const frame=this.bytes.subarray(0,newline);this.bytes=this.bytes.subarray(newline+1);return JSON.parse(frame.toString('utf8'))}
      const buffer=Buffer.allocUnsafe(65536)
      try {const length=readSync(this.output,buffer);if(length)this.bytes=Buffer.concat([this.bytes,buffer.subarray(0,length)]);else Atomics.wait(sleep,0,0,1)} catch(error:any) {if(error.code!=='EAGAIN')throw error;Atomics.wait(sleep,0,0,1)}
      if(performance.now()>until)throw new Error(`passes transport response timeout child=${this.child.pid}`)
      if(this.bytes.length>64*1024*1024)throw new Error('passes transport frame limit')
    }
  }
  private callback(method:string,args:any[]) {
    callbacks[method]=(callbacks[method]??0)+1
    const h=this.host
    if(method==='configured')return h.configured()
    if(method==='closing')return h.closing()
    if(method==='aborted')return h.lifetime.aborted
    if(method==='now')return new Date().toISOString()
    if(method==='receive')return h.engine.receive(...args)
    if(method==='kick')return h.effects.kick()
    if(method==='evaluateDue')return h.engine.evaluateDue()
    if(method==='healthState')return h.health.get()?.state??null
    if(method==='healthSet')return h.health.set(...args)
    if(method==='effectsIdle') {
      const id=++this.idleId
      // Execute the JS host's hot prefix, but never use its result to decide
      // Rust pump/idle state. Only the external promise's settlement crosses.
      const promise=h.effects.idle()
      Promise.resolve(promise).then(()=>{if(!this.closed){this.rpc('settleIdle',{id});this.progress()}},error=>{if(!this.closed){this.rpc('settleIdle',{id,error:String(error)});this.progress()}})
      return {pending:id}
    }
    throw new Error(`unknown passes observation ${method}`)
  }
  private rpc(method:string,args:any={}) {
    commands[method]=(commands[method]??0)+1
    this.write({kind:'command',method,...args})
    for(;;) {
      const frame=this.read()
      if(frame.kind==='callback') {
        try {this.write({value:this.callback(frame.method,frame.args)??null})} catch(error) {this.write({error:String(error)})}
      } else if(frame.kind==='result')return frame.value
      else throw new Error('unknown passes frame')
    }
  }
  private complete(result:any) {
    for(const [key,value] of Object.entries<any>(result.completed??{})) {
      const wait=this.waits.get(Number(key));if(!wait)throw new Error('unknown native pass completion')
      this.waits.delete(Number(key))
      if(value.error!==undefined)wait.reject(exception(value.error))
      else wait.resolve(value.slots?.map((slot:any)=>slot.status==='fulfilled'?{status:'fulfilled',value:undefined}:{status:'rejected',reason:exception(slot.error)}))
    }
    return result
  }
  private progress() {
    if(this.scheduled||this.closed)return
    this.scheduled=true
    queueMicrotask(()=>{
      this.scheduled=false;if(this.closed)return
      const result=this.complete(this.rpc('poll'))
      if((this.waits.size && result.external_idle===0)||result.jobs_pending)this.progress()
    })
  }
  private wait(method:string) {
    const result=this.rpc(method)
    const value=new Promise<any>((resolve,reject)=>this.waits.set(result.ticket,{resolve,reject}))
    this.progress();return value
  }
  pump():Promise<void>{return this.wait('pump')}
  request():void{checked(this.rpc('request'));this.progress()}
  idle():Promise<void>{return this.wait('idle')}
  stop():Promise<PromiseSettledResult<void>[]>{return this.wait('stop')}
  start():void{checked(this.rpc('start'));this.progress()}
  receive(trigger:any):any{const result=checked(this.rpc('receive',{trigger}));this.progress();return result.result}
  nextMs():number|null{return this.complete(this.rpc('poll')).next_ms}
  async advance(ms:number) {this.complete(this.rpc('advance',{ms}));this.progress();await Promise.resolve()}
  close() {
    if(this.closed)return
    this.closed=true;this.write({kind:'command',method:'close'});this.child.kill('SIGTERM')
    closeSync(this.input);closeSync(this.output);rmSync(this.directory,{recursive:true,force:true});instances.delete(this)
    for(const wait of this.waits.values())wait.reject(new Error('passes test transport closed'))
    this.waits.clear()
  }
}
export function cleanupPasses(){for(const instance of [...instances])instance.close()}
/** Drive native paused Tokio clocks at the same boundaries as fake JS timers.
 * This controls time only: it never performs an evaluation or coalesces calls. */
export async function advancePassClocks(ms:number, jsAdvance:(ms:number)=>Promise<unknown>) {
  let left=ms
  while(left>0) {
    const deadlines=[...instances].map(instance=>instance.nextMs()).filter((value):value is number=>value!==null)
    const next=deadlines.length?Math.min(...deadlines):left
    const chunk=Math.min(left,Math.max(1,next))
    await jsAdvance(chunk)
    for(const instance of [...instances])await instance.advance(chunk)
    left-=chunk
  }
}
export function savePassesEvidence(){if(process.env.SUPERAGENT_PASSES_EVIDENCE)writeFileSync(process.env.SUPERAGENT_PASSES_EVIDENCE,JSON.stringify({schema_version:1,scope:'Rust-owned Passes scheduling/timers/completion/health against original host callbacks; fake clock control is transport-only, no full Runtime/Source/provider claim',commands,callbacks},null,2)+'\n')}
