/** Independent original Store -> Rust Store schema-10 migration comparison.
 * No module mocks: only the second constructor is replaced by the Rust adapter.
 * Logical rows and identity-bearing KV encodings are assertions; generic JSON
 * encoding differences are separately retained as an unresolved D07 diagnostic.
 */
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { DatabaseSync } from 'node:sqlite'
import { afterAll, afterEach, expect, it, vi } from 'vitest'
import { Store as ReferenceStore } from './upstream/src/app/store.ts'
import { createStoreAdapter, RustDatabaseSync, cleanupStoreClients, saveStoreEvidence } from './store-target-adapter.ts'
const TargetStore = createStoreAdapter(RustDatabaseSync)
const fixed = Date.parse('2026-11-01T00:00:00.123Z')
const actor = { source: 'feishu', id: 'ou_alice' }
const at = '2026-10-31T00:00:00Z'
const channel = { source: 'feishu', thread: 'feishu:oc_team:om_1', address: { chatId: 'oc_team', messageId: 'om_1' } }
const basis = { chatId: 'oc_team', member: 'ou_alice', mention: true }
const mandate = { strength: 'explicit', policyVersion: 'v1', actor, scope: 'small', basis }
const ticket = { id: 'legacy', title: 'Fix', decisionId: 'd1', state: 'open', assignments: [{ id: 'a1', state: 'running', createdAt: at }], summary: 'in progress', createdAt: at, updatedAt: at, mandate,
  inputs: [{ kind: 'reopen', id: 'old', decisionId: 'd2', at }], undelivered: ['old'], channels: [channel, { source: 'github', thread: 'github:9', address: { number: '9' } }],
  meta: { workspace: '/w/fix', sourceTriggerId: 'trig', triggerIds: ['trig'], assignment: 'Fix the typo', prNumber: 9 } }
const thread = { key: channel.thread, ref: { source: 'feishu', id: 'oc_team:om_1' }, createdAt: at, subscriptions: ['legacy'], correlated: [], pending: [], meta: {} }
const trigger = { id: 'trig', source: 'feishu', threads: [thread.ref], actor, human: true, at, text: 'fix', channel, meta: {} }
const decision = { id: 'd1', threadKey: thread.key, triggerIds: ['trig'], policyVersion: 'v1', kind: 'accept', reason: 'trig: sender_not_member; ci: Assessment timed out', at, ticketId: 'legacy', mandate }
const legacyPrompts = { shared: 'old', orchestrator: 'old', worker: 'old', guides: { work: 'fewer than 350 lines (additions plus deletions)', release: 'orchestrator_create_task event_id guide/github dsh-tag-feishu' } }
const history = (revision: number, settings: any, source = 'web') => ({ revision, savedAt: '', source, policyVersion: '', policyChanged: false, notified: [], settings })
const baseKv = () => ({
  settings: { allowedChats: ['oc_team'], members: [{ name: 'Alice', feishuId: 'ou_alice', github: 'alice' }], maxWorkers: 2 },
  'relay-queue': [{ id: 'kept', text: 'do not replace' }, { id: 'new', text: 'next' }], 'subscription-results': ['github:9'], 'prompt-notification': 2, 'settings-notification': 0,
  'prompt-revision': 1, 'prompt-settings-text': 1,
  'prompt-version:1': { revision: 1, previousRevision: 0, savedAt: '', source: 'web', config: legacyPrompts },
  'feishu-chats': { oc_team: true }, 'feishu-history-threads:oc_team': ['zeta', 'alpha', '2', '1'], 'feishu-history:oc_team': at,
  'github-thread:9': '2026-10-30T00:00:00Z', 'thread-since:github:9': at, 'github-ended:9': { at, reason: 'merged' },
  'answer-ready:legacy:trig': true, 'task/legacy/input/old': 'body', 'prompt-version-read:1': true,
})
type Scenario = { name: string; kv?: any; emptyKv?: boolean; records?: boolean; generation?: boolean; mutate?: (db: DatabaseSync) => void }
const scenarios: Scenario[] = [
  { name: 'complete legacy state, preserved attempts, notices and pending reply', records: true },
  { name: 'already-versioned effect and dropped/sent/permanent receipts', records: true, generation: true, mutate: db => {
    db.prepare('INSERT INTO outbox (id,value,sent) VALUES (?,?,?)').run('sent', JSON.stringify({ id:'sent', channel, text:'sent', attempts:3, error:'old' }), 1)
    db.prepare('INSERT INTO outbox (id,value,sent) VALUES (?,?,?)').run('gone', JSON.stringify({ id:'gone', channel, text:'gone', dropped:{at,reason:'The conversation can no longer receive messages'} }), 0)
    db.prepare('INSERT INTO outbox (id,value,sent) VALUES (?,?,?)').run('ignored', JSON.stringify({ id:'ignored', channel, text:'ignored', dropped:{at,reason:'The Source no longer listens to this conversation'} }), 0)
  } },
  { name: 'domain settings, removed sources and corrupt historical prompt fallback', kv: {
    settings: { sources: { zeta:{enabled:true,config:{z:1,a:2}}, 'github-issue':{enabled:true,config:{}}, github:{enabled:true,config:{mergeCli:'gh'}} }, policy:{members:[{name:'Alice',feishuId:'ou_alice',github:'alice'}],autoMergeLineLimit:456,rules:[]}, executor:{prepare:'DSH_TAG_DIR DSH_GO_HOME',paused:true} },
    'settings-version:1': history(1,{allowedChats:['oc_team'],members:[]}),
    'settings-version:2': history(2,{sources:{'github-issue':{enabled:true,config:{}}}},'orchestrator-tool'),
    'settings-version:3': history(3,{executor:{maxWorkers:'many'}},'member-sync'),
    'prompt-version:2':{revision:2,previousRevision:1,savedAt:'',source:'web',config:{communication:1}},
  } },
  { name: 'current settings absent, history pointer lags newest revision', emptyKv: true, kv: {
    'settings-revision':1,'settings-version:1':history(1,{executor:{maxWorkers:2}}), 'settings-version:4':history(4,{executor:{maxWorkers:4}},'policy-sync'),
  } },
  { name: 'nullish queues, topic timestamps and snapshot settings', emptyKv:true, kv:{'relay-queue':null,'subscription-results':null,'prompt-notification':null,'settings-notification':null,'feishu-history-threads:oc_team':['b','a'],'feishu-history:oc_team':null,'settings-version:1':history(1,null)} },
  { name: 'removed issue retries, cursor consolidation, lark and active assessments', records:true, kv:{
    'github-issue-pending':{old:{number:8,at:'2026-10-01T00:00:00Z'},a:{number:9,at:'2026-10-30T00:00:00Z'},b:{number:9,at:'2026-10-29T00:00:00Z'},invalid:{number:11,at:'invalid'}},
    'github-retry':{9:{since:'2026-10-28T00:00:00Z',pr:true},10:{since:at,pr:false}},'github-issues-since':at,'github-issues-started-at':at,
    'github-issue-eligibility:9':true,'github-cursor:9':'2026-10-31T01:00:00Z',
  },mutate:db=>{
    const evaluation = { id:'e1',threadKey:thread.key,triggerIds:['trig'],strength:'inferred',actor,policyVersion:'v1',startedAt:'',deadline:'',basis:{source:'feishu',scope:{chatId:'oc_team'},row:'chats',admission:'listedUnaddressed',observation:true,mode:'auto'} }
    db.prepare('UPDATE thread SET value=?').run(JSON.stringify({...thread,evaluating:evaluation}))
    db.prepare('UPDATE trigger SET value=?').run(JSON.stringify({...trigger,source:'github-issue',url:'https://larksuite.com/example'}))
    db.prepare("INSERT INTO effect (kind,key,value,seq,attempts,error,due,since) VALUES ('assessment','e1','{}',5,3,'failed',7,'old')").run()
    db.prepare("INSERT INTO effect (kind,key,value,seq,due,since) VALUES ('assessment','orphan','{}',6,7,'old')").run()
  } },
  { name: 'already-current working columns with schema-10 marker', records:true, mutate:db=>{
    db.exec("ALTER TABLE ticket ADD COLUMN obsolete INTEGER AS (1); CREATE INDEX ticket_obsolete ON ticket(obsolete)")
  } },
  { name: 'legacy numeric and overflowing date timestamps use the same host timezone', records:true, mutate:db=>{
    db.prepare('UPDATE trigger SET at=?,value=?').run('4',JSON.stringify({...trigger,at:'4'}))
    db.prepare('UPDATE trigger_thread SET at=?').run('4')
    db.prepare('UPDATE thread SET value=?').run(JSON.stringify({...thread,createdAt:'1',lastAt:'2026-02-30'}))
  } },
  { name: 'ended tickets without redundant reason and reopened decision inputs', records:true, mutate:db=>{
    db.prepare('UPDATE ticket SET value=?').run(JSON.stringify({...ticket,state:'ended',ended:{at,by:'human',reason:'Not needed'},assignments:[{...ticket.assignments[0],endedAt:at,endedReason:'Not needed',ticketId:'legacy',ref:'worker-legacy'}],execution:{session:'worker-legacy',name:'fix'}}))
    db.prepare('INSERT INTO decision VALUES (?,?,?)').run('d2',thread.key,JSON.stringify({...decision,id:'d2',kind:'reopen',reason:'follow-up'}))
    db.prepare('INSERT INTO disposition VALUES (?,?)').run('trig',JSON.stringify({filtered:'Sender is not an authorized human member'}))
  } },
]
const directories: string[] = [], reports: any[] = []
function seed(path: string, scenario: Scenario) {
  new ReferenceStore(path).close()
  const db = new DatabaseSync(path)
  db.exec("DROP TABLE outbox; CREATE TABLE outbox (id TEXT PRIMARY KEY,value TEXT NOT NULL,sent INTEGER NOT NULL DEFAULT 0); CREATE TABLE inbox(id TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT INTO inbox VALUES('old','{}'); CREATE TABLE task(id TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT INTO task VALUES('old','{}')")
  if (!scenario.generation) db.exec('ALTER TABLE effect DROP COLUMN generation')
  const kv = { ...(scenario.emptyKv ? {} : baseKv()), ...scenario.kv, schema:10 }
  for (const [key,value] of Object.entries(kv)) db.prepare('INSERT INTO kv VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value').run(key,JSON.stringify(value))
  if (scenario.records) {
    db.prepare('INSERT INTO ticket(id,value) VALUES (?,?)').run(ticket.id,JSON.stringify(ticket))
    db.prepare('INSERT INTO thread(key,value) VALUES (?,?)').run(thread.key,JSON.stringify(thread))
    db.prepare('INSERT INTO trigger VALUES (?,?,?)').run(trigger.id,JSON.stringify(trigger),at)
    db.prepare('INSERT INTO trigger_thread VALUES (?,?,?,?)').run(thread.key,trigger.id,at,1)
    db.prepare('INSERT INTO decision VALUES (?,?,?)').run(decision.id,decision.threadKey,JSON.stringify(decision))
    db.prepare('INSERT INTO outbox VALUES (?,?,?)').run('pending',JSON.stringify({id:'pending',channel,text:'收到',attempts:2,error:'offline',nextAttemptAt:0}),0)
  }
  db.prepare("INSERT INTO effect (kind,key,value,seq,attempts,error,due,since) VALUES ('relay','kept','{\"text\":\"first\"}',4,4,'held',7,'old')").run()
  scenario.mutate?.(db); db.close()
}
function snapshot(path:string) {
  const db = new DatabaseSync(path), logical:any={}, raw:any={}, schema:any={}
  for (const {name} of db.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").all() as any[]) {
    schema[name] = {columns:db.prepare(`pragma table_xinfo('${name}')`).all(),indexes:db.prepare(`pragma index_list('${name}')`).all().map((index:any)=>({...index,columns:db.prepare(`pragma index_xinfo('${index.name}')`).all()}))}
    raw[name] = db.prepare(`SELECT * FROM ${name} ORDER BY rowid`).all().map(row=>({...row}))
    logical[name] = raw[name].map((row:any)=>({...row,...typeof row.value==='string'?{value:JSON.parse(row.value)}:{}}))
  }
  db.close(); return {schema,logical,raw}
}
afterEach(()=>{cleanupStoreClients();vi.useRealTimers();for(const directory of directories.splice(0)) rmSync(directory,{recursive:true,force:true})})
for (const scenario of scenarios) it('logical and identity KV parity: '+scenario.name,()=>{
  const directory=mkdtempSync(join(tmpdir(),'superagent-migration-diff-'));directories.push(directory)
  const reference=join(directory,'reference.sqlite'),target=join(directory,'target.sqlite')
  seed(reference,scenario);seed(target,scenario)
  vi.useFakeTimers({toFake:['Date'],now:fixed})
  new ReferenceStore(reference).close();new TargetStore(target).close()
  const expected=snapshot(reference),actual=snapshot(target)
  const encodings:any[]=[]
  for(const [table,rows] of Object.entries(expected.raw) as [string,any[]][]) rows.forEach((row,index)=>{
    const other=actual.raw[table]?.[index]
    if(row.value!==other?.value) encodings.push({table,key:row.key??row.id??`${row.kind}/${row.seq}`,reference:row.value,target:other?.value})
  })
  reports.push({name:scenario.name,raw_encoding_differences:encodings})
  expect(actual.logical).toEqual(expected.logical)
  expect(actual.schema).toEqual(expected.schema)
  // Identity-bearing serialized configuration/source order is observable and must
  // not be normalized by this comparison. Generic record encodings remain D07.
  for(const row of expected.raw.kv.filter((r:any)=>/^(settings-version:|prompt-version:|feishu-history-threads:|github-retry$)/.test(r.key))) {
    expect(actual.raw.kv.find((r:any)=>r.key===row.key)?.value,'exact KV '+row.key).toBe(row.value)
  }
  // Opening the successfully converted result has no further migration effects.
  new TargetStore(target).close();expect(snapshot(target)).toEqual(actual)
})
afterAll(()=>{
  const base=process.env.SUPERAGENT_STORE_TARGET_EVIDENCE
  const report={scope:'independent original TypeScript vs Rust migration; logical rows, SQLite column/index metadata and identity KV encodings asserted; generic record JSON encodings separately retained, not accepted as D07 parity',scenarios:reports,generic_encoding_difference_count:reports.reduce((n,r)=>n+r.raw_encoding_differences.length,0)}
  if(base)writeFileSync(base.replace(/\.json$/,'-migration-differential.json'),JSON.stringify(report,null,2)+'\n')
  console.log('MIGRATION_DIFFERENTIAL '+JSON.stringify({scenarios:reports.length,generic_encoding_difference_count:report.generic_encoding_difference_count}))
  saveStoreEvidence('-migration-differential-rpc',report.scope)
})
