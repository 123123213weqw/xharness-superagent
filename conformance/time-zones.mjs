/** Broad IANA-zone edge matrix, independent of the eight grammar fuzz runs. */
import {spawnSync} from 'node:child_process'
import {writeFile} from 'node:fs/promises'
const binary=process.argv[2];if(!binary)throw Error('Pass the prebuilt remote primitives executable')
const zones=['UTC',...Intl.supportedValuesOf('timeZone')],values=[]
for(const y of ['2026','2037','2099','2100','2101','2200','2400','+010000','+262145','-262145','-271821','+275760'])for(let m=1;m<=12;m++)for(const d of ['01','28'])for(const time of ['00:00:00','02:30:00'])values.push(`${y}-${String(m).padStart(2,'0')}-${d}T${time}`)
const differences=[],by_zone=[];let count=0
for(const zone of zones){
  process.env.TZ=zone
  const requests=values.map(value=>({op:'time',value}))
  const run=spawnSync(binary,[],{input:requests.map(x=>JSON.stringify(x)).join('\n')+'\n',encoding:'utf8',env:{...process.env,TZ:zone},maxBuffer:16*1024*1024})
  if(run.error||run.status!==0)throw run.error??Error(run.stderr)
  const lines=run.stdout.trim().split('\n');if(lines.length!==values.length)throw Error('Response count mismatch')
  let failed=0
  for(const [i,value]of values.entries()){
    const ms=Date.parse(value),expected={iso:Number.isFinite(ms)?new Date(ms).toISOString():value,milliseconds:Number.isFinite(ms)?ms:null},target=JSON.parse(lines[i]);count++
    if(expected.iso!==target.iso||expected.milliseconds!==target.milliseconds){failed++;differences.push({zone,value,reference:expected,target})}
  }
  by_zone.push({zone,cases:values.length,passed:values.length-failed,failed})
}
const report={schema_version:1,reference_commit:'6337fc823ce20251c8de8085948165c81aaa701e',scope:'all Node24 Intl supported IANA names: modern, 2037, transition-table boundary, century/leap, recurring far future and ancient LMT cases; finite coverage, not a universal proof of TZ aliases/platforms',node:process.versions.node,v8:process.versions.v8,tzdata:process.versions.tz,zones:zones.length,cases:count,passed:count-differences.length,by_zone,differences}
if(process.argv[3])await writeFile(process.argv[3],JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify({...report,by_zone:undefined,differences:differences.slice(0,12)}));if(differences.length)process.exitCode=1
