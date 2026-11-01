/** Transport only: native Rust owns every watermark, chain and classifier. */
import {spawn,execFileSync} from 'node:child_process'
import {mkdtempSync,mkdirSync,openSync,closeSync,readSync,writeSync,rmSync,constants} from 'node:fs'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
const pause=new Int32Array(new SharedArrayBuffer(4)),instances=new Set<RustRetryHealth>()
const evidence={instances:0,calls:0,methods:{} as Record<string,number>}
export class RustRetryHealth {
 private directory:string;private input:number;private output:number;private child:any
 private bytes=Buffer.alloc(0);private closed=false
 constructor(){
  const binary=process.env.SUPERAGENT_RETRY_HEALTH_BRIDGE_BIN
  if(!binary)throw new Error('prebuilt remote SUPERAGENT_RETRY_HEALTH_BRIDGE_BIN required')
  this.directory=mkdtempSync(join(tmpdir(),'native-retry-health-'))
  const input=join(this.directory,'input'),output=join(this.directory,'output')
  execFileSync('mkfifo',['-m','600',input,output])
  this.input=openSync(input,constants.O_RDWR|constants.O_NONBLOCK);this.output=openSync(output,constants.O_RDWR|constants.O_NONBLOCK)
  this.child=spawn(binary,[input,output],{stdio:['ignore','ignore','inherit']});instances.add(this);evidence.instances++
 }
 private rpc(method:string,args:any={}) {
  if(this.closed)throw new Error('retry health owner closed')
  evidence.calls++;evidence.methods[method]=(evidence.methods[method]??0)+1
  const bytes=Buffer.from(JSON.stringify({method,...args})+'\n');let offset=0;const until=performance.now()+20000
  while(offset<bytes.length){try{offset+=writeSync(this.input,bytes,offset)}catch(e:any){if(e.code!=='EAGAIN')throw e;if(performance.now()>until)throw new Error('retry health write timeout');Atomics.wait(pause,0,0,1)}}
  for(;;){const at=this.bytes.indexOf(10);if(at>=0){const frame=this.bytes.subarray(0,at);this.bytes=this.bytes.subarray(at+1);return JSON.parse(frame.toString('utf8')).value}
   const buffer=Buffer.allocUnsafe(16384)
   try{const count=readSync(this.output,buffer);if(count)this.bytes=Buffer.concat([this.bytes,buffer.subarray(0,count)]);else Atomics.wait(pause,0,0,1)}catch(e:any){if(e.code!=='EAGAIN')throw e;Atomics.wait(pause,0,0,1)}
   if(performance.now()>until)throw new Error('retry health response timeout');if(this.bytes.length>1024*1024)throw new Error('retry health response limit')
  }
 }
 observe(id:string,event:any){this.rpc('observe',{id,event})}
 restore(id:string,events:readonly any[]){this.rpc('restore',{id,events})}
 clear(id:string){this.rpc('clear',{id})}
 get(id:string){const result=this.rpc('get',{id});return result===null?undefined:result}
 list(){return this.rpc('list')}
 modelEntry(){return this.rpc('entry')}
 async close(){if(this.closed)return;const exited=new Promise<void>((resolve,reject)=>{this.child.once('exit',(code:number)=>code===0?resolve():reject(new Error(`retry health child exit ${code}`)));this.child.once('error',reject)})
  this.rpc('exit');this.closed=true;instances.delete(this);closeSync(this.input);closeSync(this.output);await exited;rmSync(this.directory,{recursive:true,force:true})
 }
}
export async function cleanupRetryHealth(){await Promise.all([...instances].map(instance=>instance.close()))}
export function saveRetryHealthEvidence(){if(process.env.SUPERAGENT_RETRY_HEALTH_EVIDENCE){const p=process.env.SUPERAGENT_RETRY_HEALTH_EVIDENCE;mkdirSync(join(p,'..'),{recursive:true});writeFile(p,JSON.stringify(evidence,null,2)+'\n')}}
import {writeFileSync as writeFile} from 'node:fs'
