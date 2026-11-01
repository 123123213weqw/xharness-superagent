import {fileURLToPath} from 'node:url'
import {resolve} from 'node:path'
import {readdirSync} from 'node:fs'
import {createRequire} from 'node:module'
const root=fileURLToPath(new URL('.',import.meta.url))
const sdk=fileURLToPath(new URL('./upstream/node_modules/',import.meta.url))
if(!process.env.FULL_LOOP_PACKAGES)throw Error('FULL_LOOP_PACKAGES is required')
const packages=resolve(process.env.FULL_LOOP_PACKAGES)+'/'
const supplemental=['dsh-agent-loop','dsh-session-projection','dsh-brand','dsh-util-values','dsh-session-persistence-jsonl']
const formats=['dsh-session-format-catalog','dsh-session-format','dsh-session-format-v3-to-v4'].map(name=>{
 const dirs=readdirSync(sdk+'.pnpm/').filter(d=>d.startsWith('@deepseek-ai+'+name+'@0.2.1-alpha.2_'))
 if(dirs.length!==1)throw Error('Ambiguous pinned package: '+name)
 const req=createRequire(sdk+'.pnpm/'+dirs[0]+'/node_modules/@deepseek-ai/'+name+'/package.json')
 return {name,req}
})
export default {root,resolve:{alias:[
 {find:/^vitest$/,replacement:sdk+'vitest/dist/index.js'},
 {find:/^@deepseek-ai\/node-addon-system\/flock$/,replacement:packages+'node-addon-system/package/lib/flock.js'},
 {find:/^@deepseek-ai\/dsh-session-format-catalog\/message-projections$/,replacement:formats[0].req.resolve('@deepseek-ai/dsh-session-format-catalog/message-projections')},
 ...formats.map(({name,req})=>({find:new RegExp('^@deepseek-ai/'+name+'$'),replacement:req.resolve('@deepseek-ai/'+name)})),
 ...supplemental.map(name=>({find:new RegExp('^@deepseek-ai/'+name+'$'),replacement:packages+''+name+'/package/lib/index.js'})),
 {find:/^@deepseek-ai\/(.+)$/,replacement:sdk+'@deepseek-ai/$1'},
 {find:/^zod$/,replacement:sdk+'zod/index.js'},
]},test:{include:['full-agent-loop.spec.ts'],maxWorkers:1,fileParallelism:false,testTimeout:30000}}
