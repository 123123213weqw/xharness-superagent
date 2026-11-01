/** Real native grammar/time arithmetic vs the pinned reference isoTime and
 * Node 24.9/V8 Date.parse; no target build and no string canonicalization. */
import {spawnSync} from 'node:child_process'
import {writeFile} from 'node:fs/promises'
import {isoTime} from './reference/upstream/src/core/model.ts'
const binary=process.argv[2];if(!binary)throw Error('Pass a prebuilt remote superagent-primitives binary')
const cases=[];const add=value=>cases.push({op:'time',value});const addMs=value=>cases.push({op:'time_milliseconds',value})
for(const value of ['', 'invalid','0','1','2','4','12','13','31','32','49','50','99','100','001','0000','0001',' 1 ','2026','2026-10','2026-02-30','2026-11-01T00:00:00',
  '2015-02-29','2024-02-31','2024-13-01','2024-00-01','2024-01-00','2024-01-32','2024-01-01T24:00Z','2024-01-01T24:00:00.000Z','2024-01-01T24:00:01Z','2024-01-01T23:59:60Z',
  '2024-01-01T00:00:00.0000000001Z','2024-01-01T00:00:00.00000000000012345Z',
  '2024-01-01t01:02:03z','2024-01-01T01:02:03+0800','2024-01-01T01:02:03+08:00','2024-01-01T01:02:03+08','2024-01-01 01:02:03',' 2024-01-01 ',
  'January 1','Jan 1 20','1 Jan 20','20 Jan 1','Thu, 01 Jan 1970 00:00:00 GMT','Wed Oct 31 2018 12:34:56 GMT+0800 (China Standard Time)',
  '1/2/2024','2024/2/1','01-02-03','03:04 Jan 1 2024','Jan 1 2024 3::','Jan 1 2024 3:04:05.6 pm GMT-8','Jan 1 2024 GMT-0830','Jan 1 2024 GMT+99:99',
  'DecFoo 1 2024','x1','x 1','1x','1 x','Jan 1 (nested (ignored)) 2024','(prefix) 2024-01-01','2024-01-01\0junk','2024\u2028-01-01','2024\u0085-01-01',
  '+275760-09-13T00:00:00.000Z','+275760-09-13T00:00:00.001Z','-271821-04-20T00:00:00.000Z','-271821-04-19T23:59:59.999Z','+010000-01-01','-000001-01-01','-000000-01-01',
  '2024-03-10T02:30:00','2024-11-03T01:30:00','2011-12-30T12:00:00','1900-01-01T00:00:00','1800-01-01T00:00:00'])add(value)
for(const y of ['0','1','12','32','49','50','99','100','999','1900','2000','2024','10000','275760','999999999'])for(const m of ['1','01','2','02','12','13'])for(const sep of ['-','/',' '])add(`${y}${sep}${m}${sep}1`)
for(const y of ['0000','0099','1900','2000','2024','2026','+010000','+262145','-262145','+275760','-271821'])for(const m of ['01','02','04','09','12'])for(const d of ['01','28','29','30','31'])for(const suffix of ['','T00:00:00Z','T23:59:59.1234Z','T00:00:00+08:00','T00:00:00'])add(`${y}-${m}-${d}${suffix}`)
for(const s of ['Jan 1 2024','2024-01-01','1/2/03'])for(const h of [0,1,12,23,24,25])for(const tail of [':00',':01:02.003',':00 AM',':00 PM',':00 GMT+8',':00 +01:30'])add(`${s} ${h}${tail}`)
let seed=0x7e8;const random=()=>{seed^=seed<<13;seed^=seed>>>17;seed^=seed<<5;return seed>>>0}
const tokens=['2024','1','01','0','999999999','Jan','GMT','UTC','T','Z','+','-',':','.',' ','/','(',')','PM','junk','\u2028','\u0085','\0']
for(let i=0;i<10000;i++){let s='';for(let j=0,n=random()%10;j<n;j++)s+=tokens[random()%tokens.length];add(s)}
for(let i=0;i<500;i++){const date=new Date(Math.trunc((random()/0xffffffff*2-1)*8640000000000000));add(date.toString());add(date.toUTCString())}
for(const value of [-8640000000000001,-8640000000000000,-8639999999999999,-62167219200000,-1,0,1,253402300800000,8639999999999999,8640000000000000,8640000000000001])addMs(value)
for(let i=0;i<1000;i++)addMs(Math.trunc((random()/0xffffffff*2-1)*8640000000000000))
const run=spawnSync(binary,[],{input:cases.map(x=>JSON.stringify(x)).join('\n')+'\n',encoding:'utf8',maxBuffer:64*1024*1024});if(run.error||run.status!==0)throw run.error??Error(run.stderr)
const lines=run.stdout.trim().split('\n');if(lines.length!==cases.length)throw Error('Response count mismatch')
const differences=[]
for(const [i,request]of cases.entries()){
  let expected;if(request.op==='time'){const ms=Date.parse(request.value);expected={iso:isoTime(request.value),milliseconds:Number.isFinite(ms)?ms:null}}
  else{try{expected={iso:new Date(request.value).toISOString()}}catch{expected={error:'Invalid time value'}}}
  const target=JSON.parse(lines[i]);if(JSON.stringify(expected)!==JSON.stringify(target)&& (expected.iso!==target.iso||expected.milliseconds!==target.milliseconds||expected.error!==target.error))differences.push({request,reference:expected,target})
}
const report={schema_version:1,reference_commit:'6337fc823ce20251c8de8085948165c81aaa701e',scope:'Node24/V8 date grammar, millisecond conversion, ISO overflow/TimeClip, legacy names and numeric dates, local-zone DST/discontinuities, extended years; finite case coverage only',timezone:process.env.TZ??Intl.DateTimeFormat().resolvedOptions().timeZone,versions:process.versions,cases:cases.length,passed:cases.length-differences.length,differences}
if(process.argv[3])await writeFile(process.argv[3],JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify({...report,differences:differences.slice(0,12)}));if(differences.length)process.exitCode=1
