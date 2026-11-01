/** No compiler or paid models. Run with an ALREADY remote-built policy bridge.
 * Refuses existing output paths; separate child Node processes prove cold read. */
import {readFileSync,writeFileSync,mkdirSync,readdirSync,lstatSync,realpathSync} from 'node:fs'
import {fileURLToPath} from 'node:url'
import {join,resolve,dirname,relative} from 'node:path'
import {createHash} from 'node:crypto'
import {spawnSync} from 'node:child_process'
const sha=b=>createHash('sha256').update(b).digest('hex')
const repo=fileURLToPath(new URL('../',import.meta.url))
const [cacheArg,outputArg,bridgeArg]=process.argv.slice(2)
if(!cacheArg||!outputArg||!bridgeArg)throw Error('Usage: node conformance/run-full-agent-loop.mjs PACKAGES_DIR NEW_OUTPUT_DIR REMOTE_BUILT_BRIDGE')
if(process.platform!=='linux'||process.arch!=='x64')throw Error('This fixture currently validates Linux x64 only; other platforms not_run')
const packages=resolve(cacheArg),output=resolve(outputArg),bridge=resolve(bridgeArg)
const lock=JSON.parse(readFileSync(join(repo,'conformance/reference/full-agent-loop-source.json'),'utf8'))
function tree(folder){
 const result={}
 function walk(path){for(const name of readdirSync(path).sort()){
  if(name==='node_modules')continue
  const p=join(path,name),stat=lstatSync(p)
  if(stat.isSymbolicLink())throw Error('Unexpected package symlink')
  if(stat.isDirectory())walk(p);else if(stat.isFile())result[relative(folder,p)]=sha(readFileSync(p));else throw Error('Non-file package artifact')
 }}walk(folder);return result
}
for(const item of lock.packages){
 const dir=join(packages,item.cache_name,'package');const actual=tree(dir)
 if(JSON.stringify(Object.entries(actual).sort())!==JSON.stringify(Object.entries(item.files).sort()))throw Error('Package tree integrity mismatch: '+item.name)
 const tarball=readFileSync(join(packages,item.cache_name+'.tgz'))
 if(sha(tarball)!==item.tarball_sha256||'sha512-'+createHash('sha512').update(tarball).digest('base64')!==item.integrity)throw Error('Tarball integrity mismatch')
}
const flockLink=join(packages,'node-addon-system/package/node_modules/@deepseek-ai/node-addon-system-linux-x64')
if(realpathSync(flockLink)!==realpathSync(join(packages,'node-addon-system-linux-x64/package')))throw Error('Unexpected native prebuilt route')
const verified=spawnSync(process.execPath,['conformance/verify-reference.mjs'],{cwd:repo,encoding:'utf8',timeout:30000})
if(verified.error||verified.status!==0)throw Error('Original reference integrity verification failed')
mkdirSync(output,{recursive:false});const state=join(output,'state');mkdirSync(state)
const result={passed:false,actualPublishedAgentLoop:true,actualPublishedJsonlBackend:true,productionPolicyDifferential:false,nativeRustLoopParity:false,acceptedRubrics:0,paidModelCalls:0,localRustCompilations:0,compression:process.env.FULL_LOOP_COMPRESSION??'none',bridgeSha256:sha(readFileSync(bridge)),packages:lock.packages.map(({name,version,tarball_sha256})=>({name,version,tarball_sha256})),phases:[]}
writeFileSync(join(output,'reference-integrity.log'),verified.stdout+verified.stderr)
try{
 for(const phase of ['create','resume']){
  const evidence=join(output,phase+'.json')
  const run=spawnSync(process.execPath,[join(repo,'conformance/reference/upstream/node_modules/vitest/vitest.mjs'),'run','--config','conformance/reference/full-agent-loop.vitest.mjs','--reporter=verbose'],{cwd:repo,encoding:'utf8',timeout:60000,maxBuffer:16*1024*1024,env:{...process.env,FULL_LOOP_PACKAGES:packages,COLD_LOOP_DIR:state,COLD_LOOP_PHASE:phase,FULL_LOOP_EVIDENCE:evidence,SKILL_CATALOG_BRIDGE_BIN:bridge}})
  writeFileSync(join(output,phase+'.log'),(run.stdout??'')+(run.stderr??''));writeFileSync(join(output,phase+'.exit'),String(run.status)+'\n')
  if(run.error||run.status!==0)throw Error('SDK '+phase+' phase failed; inspect retained log')
  const report=JSON.parse(readFileSync(evidence,'utf8'))
  if(!report.passed||!report.productionPolicyDifferential||report.nativeRustLoopParity||report.acceptedRubrics!==0)throw Error('Invalid phase evidence')
  result.phases.push({phase,pid:report.pid,requests:report.requests.length,policyComparisons:report.policyCases.length,events:report.events.length,evidenceSha256:sha(readFileSync(evidence))})
 }
 if(result.phases[0].pid===result.phases[1].pid)throw Error('Cold phases must be distinct processes')
 result.productionPolicyDifferential=true;result.passed=true
}finally{writeFileSync(join(output,'summary.json'),JSON.stringify(result,null,2)+'\n')}
process.stdout.write(JSON.stringify(result,null,2)+'\n')
