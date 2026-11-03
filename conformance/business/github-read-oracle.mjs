// The unmodified pinned readGithub AND Commands run against an explicitly
// offline gh executable. No SDK stubs, network, real account or paid provider.
import {mkdtemp,writeFile,readFile,chmod,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import assert from 'node:assert/strict';
import {readGithub} from '../reference/upstream/src/sources/github-read.ts';
import {Commands} from '../reference/upstream/src/infra/commands.ts';

const at='2026-11-02T00:00:00.000Z';
const comment=(id,patch={})=>({id,user:{login:'alice',type:'User'},body:'正文 中文 😀',created_at:at,...patch});
const issue={...comment(123),number:12,title:'Issue title',state:'open',html_url:'https://github.com/owner/repo/issues/12'};
const head='abcdef0123456789';
const checks={
  [`commits/${head}/check-runs?per_page=100`]:[{check_runs:[{name:'build',status:'completed',conclusion:'failure'},{name:'test',status:'in_progress',conclusion:null},{name:'bad',status:'completed'},{name:'same',status:'in_progress',conclusion:null},{name:'same',status:'in_progress',conclusion:'failure'}]}],
  [`commits/${head}/statuses?per_page=100`]:[[{context:'deploy',state:'pending'},{context:'deploy',state:'success'},{context:'lint',state:'success'}]],
};
const definitions=[
 ['issue','github/issues/12',{'issues/12':issue}],
 ['deleted-account','github/issues/12',{'issues/12':{...issue,user:null}}],
 ['pull','github/pulls/12',{'pulls/12':{...issue,draft:true,merged:true,head:{ref:'feature'},base:{ref:'main'},labels:[{name:'功能'},{name:'bug'}]}}],
 ['comments-page','github/issues/12/comments',{'issues/12/comments?per_page=20&page=1':Array.from({length:20},(_,i)=>i===3?{id:'invalid'}:comment(i+1,i===4?{user:{login:'ci[bot]',type:'User'}}:{}))}],
 ['comment-reply','github/pulls/12/comments/2',{'pulls/12/comments?per_page=20&page=2':[comment(9,{in_reply_to_id:4,path:'a.rs',original_line:8})]}],
 ['reviews','github/pulls/12/reviews/1',{'pulls/12/reviews?per_page=20&page=1':[comment(4,{state:'APPROVED',body:null}),comment(5,{state:'COMMENTED'}),comment(6,{state:'CHANGES_REQUESTED',created_at:null,submitted_at:at,user:{login:'app',type:'Bot'}})]}],
 ['review-thread','github/pulls/12/thread/11',{'pulls/12/comments?per_page=100':[[comment(11,{in_reply_to_id:10,created_at:'2026-11-02T00:00:01.000Z'}),comment(10,{path:'src/文件.rs',line:4,diff_hunk:'one\ntwo\nthree\nfour'}),comment(12,{in_reply_to_id:99}),{id:'invalid'}]]}],
 ['missing-review-thread','github/pulls/12/thread/999',{'pulls/12/comments?per_page=100':[[comment(10)]]}],
 ['commit-checks',`github/commits/${head}/checks`,checks],
 ['pull-checks','github/pulls/12/checks',{'pulls/12':{head:{sha:head}},...checks}],
 ...['github/issues/0','github/issues/01','github/issues/12/reviews','github/pulls/12/thread','github/pulls/12/checks/2','github/commits/abcdefg/checks','github/issues/12/comments/0','github/../../secrets'].map((resource,i)=>[`invalid-${i}`,resource,{}]),
 ['invalid-description','github/issues/12',{'issues/12':{...issue,created_at:null}}],
];
const program=`#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
root=Path(os.environ['READ_ORACLE_ROOT'])
args=sys.argv[1:]
assert args[0]=='api' and args[1].startswith('repos/owner/repo/'), args
path=args[1][len('repos/owner/repo/'):]
with (root/'trace.jsonl').open('a') as f: f.write(json.dumps({'path':path,'flags':args[2:]})+'\\n')
data=json.loads((root/'case.json').read_text())
assert path in data, path
print(json.dumps(data[path],ensure_ascii=False))
`;
const directory=await mkdtemp(join(tmpdir(),'superagent-read-oracle-'));
const cases=[];
try{
 const cli=join(directory,'gh-fixture');await writeFile(cli,program);await chmod(cli,0o700);
 for(const [name,resource,responses] of definitions){
  await writeFile(join(directory,'case.json'),JSON.stringify(responses));await writeFile(join(directory,'trace.jsonl'),'');
  const commands=new Commands(()=>({repository:'owner/repo',github:{login:'robot',cli}}),{READ_ORACLE_ROOT:directory},new AbortController().signal);
  let outcome;
  try{outcome={value:await readGithub(commands,resource)};}catch(error){outcome={error:true,message:String(error.message)};}
  const trace=(await readFile(join(directory,'trace.jsonl'),'utf8')).trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
  cases.push({name,resource,responses,outcome,trace});
 }
}finally{await rm(directory,{recursive:true,force:true});}
const output={reference_commit:'6337fc823ce20251c8de8085948165c81aaa701e',scope:'Unmodified pinned readGithub + Commands, offline gh executable; not full Native/product or real-service acceptance',cases};
const file=new URL('./github-read-oracle.json',import.meta.url);
if(process.argv.includes('--check'))assert.deepEqual(JSON.parse(await readFile(file,'utf8')),output);else await writeFile(file,JSON.stringify(output,null,2)+'\n');
console.log(`${cases.length} original GitHub read/Commands executable cases verified.`);
