// Unmodified pinned context/repository/admission functions and real Commands;
// the gh executable is explicitly offline. This is a Source boundary oracle.
import {mkdtemp,writeFile,readFile,chmod,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import assert from 'node:assert/strict';
import {githubThreadContext,readRepositoryPath} from '../reference/upstream/src/sources/github-read.ts';
import {admissionContext} from '../reference/upstream/src/sources/admission-context.ts';
import {Commands} from '../reference/upstream/src/infra/commands.ts';
const at='2026-11-02T00:00:00.000Z';
const url='https://github.com/owner/repo/issues/12';
const comment=(id,patch={})=>({id,user:{login:'alice',type:'User'},body:`正文 ${id} 中文 😀`,created_at:at,html_url:`${url}#issuecomment-${id}`,...patch});
const issue={...comment(12),number:12,title:'Issue title',state:'open',html_url:url};
const trigger={id:'github:candidate',source:'github',threads:[{source:'github',id:'12'}],actor:{source:'github',id:'alice'},human:true,at,text:'修复 result.txt 😀',url:`${url}#issuecomment-75`,meta:{number:12,kind:'conversation'}};
const page=Array.from({length:100},(_,i)=>comment(i+1));
const responses={'issues/12':issue,'issues/12/comments?per_page=100&page=1':page,'issues/12/comments?per_page=100&page=2':[comment(100,{body:'duplicate ignored'}),comment(101,{user:null}),{id:'bad'}]};
const definitions=[
 {name:'conversation-thread',operation:'thread',trigger,part:'thread',responses:{'issues/12':issue,'issues/12/comments?per_page=100&page=1':[comment(75),{id:'bad'},comment(76,{user:{login:'ci[bot]',type:'User'}})]}},
 ...['root','parent'].map(part=>({name:`conversation-${part}`,operation:'thread',trigger,part,responses:{'issues/12':issue,'issues/12/comments?per_page=100&page=1':[comment(75)]}})),
 {name:'conversation-cursor',operation:'thread',trigger,part:'thread',cursor:'2',responses:{'issues/12/comments?per_page=100&page=2':[comment(101,{html_url:null})]}},
 ...[1,2].map(maxPages=>({name:`admission-${maxPages}-page-window`,operation:'load',trigger,maxPages,responses})),
 {name:'source-not-found-window',operation:'load',trigger:{...trigger,url:'absent'},maxPages:1,responses},
 {name:'branch-ci',operation:'thread',trigger:{...trigger,meta:{branch:'main',ci:true}},part:'thread',responses:{}},
 ...['thread','root','parent'].map(part=>({name:`inline-${part}`,operation:'thread',trigger:{...trigger,meta:{number:12,kind:'inline',commentId:11}},part,responses:{'pulls/12/comments?per_page=100':[[comment(11,{in_reply_to_id:10,created_at:'2026-11-02T00:00:01.000Z'}),comment(10,{path:'src/文件.rs',line:4,diff_hunk:'one\ntwo\nthree\nfour'}),comment(12,{in_reply_to_id:99}),{id:'bad'}]]}})),
 {name:'empty-inline-context',operation:'thread',trigger:{...trigger,meta:{number:12,kind:'inline',commentId:99}},part:'thread',responses:{'pulls/12/comments?per_page=100':[[comment(10)]]}},
 {name:'repository-file',operation:'repo',path:'/src/./文件 name.rs',kind:'file',responses:{'contents/src/%E6%96%87%E4%BB%B6%20name.rs':{raw:'源代码 😀\n'}}},
 {name:'repository-directory',operation:'repo',path:'.',kind:'directory',responses:{'contents/':[{name:'z.rs'},{name:'.env'},{name:'node_modules'},{name:'src'},{name:'文件'},{name:'😀'},{name:'\uE000'}]}},
 {name:'file-not-directory',operation:'repo',path:'file.rs',kind:'directory',responses:{'contents/file.rs':{name:'file.rs'}}},
 ...['../secret','src/.env','node_modules/a','src/../a','.git/config'].map(path=>({name:`blocked-${path}`,operation:'repo',path,kind:'file',responses:{}})),
];
const program=`#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
root=Path(os.environ['CONTEXT_ORACLE_ROOT']);args=sys.argv[1:]
assert args[0]=='api' and args[1].startswith('repos/owner/repo/'),args
path=args[1][len('repos/owner/repo/'):]
with (root/'trace.jsonl').open('a') as f:f.write(json.dumps({'path':path,'flags':args[2:]})+'\\n')
data=json.loads((root/'case.json').read_text());assert path in data,path
value=data[path]
print(value['raw'] if isinstance(value,dict) and 'raw' in value else json.dumps(value,ensure_ascii=False),end='')
`;
const directory=await mkdtemp(join(tmpdir(),'superagent-context-oracle-'));
const cases=[];
try {
 const cli=join(directory,'gh');await writeFile(cli,program);await chmod(cli,0o700);
 for(const row of definitions){
  await writeFile(join(directory,'case.json'),JSON.stringify(row.responses));await writeFile(join(directory,'trace.jsonl'),'');
  const commands=new Commands(()=>({repository:'owner/repo',github:{login:'robot',cli}}),{CONTEXT_ORACLE_ROOT:directory},new AbortController().signal);
  let outcome;
  try{
   let value;
   if(row.operation==='repo')value=await readRepositoryPath(commands,row.path,row.kind);
   else if(row.operation==='thread')value=await githubThreadContext.read(commands,row.trigger,row.part,row.cursor);
   else value=await admissionContext({messageId:githubThreadContext.messageId(row.trigger),read:(commands,part,cursor)=>githubThreadContext.read(commands,row.trigger,part,cursor)},commands,row.maxPages);
   outcome={value:JSON.parse(JSON.stringify(value))};
  }catch(error){outcome={error:true,message:String(error.message)};}
  const trace=(await readFile(join(directory,'trace.jsonl'),'utf8')).trim().split('\n').filter(Boolean).map(s=>JSON.parse(s));
  cases.push({...row,outcome,trace});
 }
}finally{await rm(directory,{recursive:true,force:true});}
const output={reference_commit:'6337fc823ce20251c8de8085948165c81aaa701e',scope:'Unmodified githubThreadContext/readRepositoryPath/admissionContext/Commands with offline gh, not full product acceptance',cases};
const file=new URL('./github-context-oracle.json',import.meta.url);
if(process.argv.includes('--check'))assert.deepEqual(JSON.parse(await readFile(file,'utf8')),output);else await writeFile(file,JSON.stringify(output,null,2)+'\n');
console.log(`${cases.length} original GitHub context/repository/admission executable cases verified.`);
