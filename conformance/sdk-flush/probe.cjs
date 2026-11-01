// Executes byte-exact published tracker and locked closed-error class bodies.
// Test-owned handles are ports, not a replacement implementation of flushAll.
const fs = require('node:fs'), vm = require('node:vm'), assert = require('node:assert/strict'), crypto = require('node:crypto');
const source = fs.readFileSync(__dirname + '/backend.js', 'utf8');
const abstract = fs.readFileSync(__dirname + '/locked-abstract.js', 'utf8');
const sha = x => crypto.createHash('sha256').update(x).digest('hex');
assert.equal(sha(source), '027803dde72aac7038253f0a8ff9fdcdbf0eb8cad53ed26efa3ec610f01587e7');
assert.equal(sha(abstract), 'cc0b6d3a224133af611b428d5a49020e300f86c3b4ba28037aeb219029bde3eb');
const extract = (s, start, end) => { const a = s.indexOf(start); assert(a >= 0); const b = s.indexOf(end, a); assert(b > a); return s.slice(a, b + 3); };
const tracker = extract(source, 'var JsonlBackendTracker = class {', '\n};\n//#endregion');
const closed = extract(abstract, 'var SessionHandleClosedError = class extends Error {', '\n};');
const ctx = vm.createContext({AggregateError, Error, SessionPersistenceRevision: x => x,
  SessionAlreadyExistsError: class extends Error {}, SessionAlreadyOwnedError: class extends Error {}});
vm.runInContext(closed + '\n' + tracker + '\nthis.Tracker=JsonlBackendTracker;this.Closed=SessionHandleClosedError;', ctx);
const deferred = () => { let resolve; return {promise: new Promise(r => {resolve=r}), release: () => resolve()}; };
const tick = async () => { for(let i=0;i<10;i++) await Promise.resolve(); };
let cases = [];
(async () => {
  // A reservation precedes adoption; overwriting its Map value never reorders.
  let trace=[], t=new ctx.Tracker('oracle'), hold=deferred();
  const handle = (id, opts={}) => ({id, access:'write', async drainLive(){trace.push('drain:'+id); if(opts.hold) await opts.hold.promise; if(opts.error) throw opts.error;}, async flush(){trace.push('flush:'+id);}});
  t.claimWrite('z'); t.claimWrite('a'); t.adopt(handle('a', {hold})); t.adopt(handle('z'));
  let done=false; let f=t.flushAll().then(()=>{done=true});
  assert.deepEqual(trace, ['drain:z']); // eager prefix before awaiting caller
  t.adopt(handle('late'));
  await tick(); assert.deepEqual(trace,['drain:z','flush:z','drain:a']); assert.equal(done,false);
  hold.release(); await f; assert.deepEqual(trace,['drain:z','flush:z','drain:a','flush:a']);
  cases.push('reservation order, eager snapshot, sequential blocked writer, late registration excluded');
  trace=[]; t=new ctx.Tracker('oracle');
  const e1={identity:1}, e2={identity:2};
  t.adopt(handle('z',{error:e1})); t.adopt(handle('a',{error:e2})); t.adopt(handle('healthy'));
  let aggregate; try { await t.flushAll(); assert.fail('expected AggregateError'); } catch(e) { aggregate=e; }
  assert(aggregate instanceof AggregateError); assert.equal(aggregate.message,'oracle flush failed');
  assert.equal(aggregate.errors[0],e1); assert.equal(aggregate.errors[1],e2);
  assert.deepEqual(trace,['drain:z','drain:a','drain:healthy','flush:healthy']);
  cases.push('ordered original errors aggregate, later healthy writer still flushed');
  trace=[]; t=new ctx.Tracker('oracle'); t.adopt(handle('only',{error:e1}));
  await assert.rejects(t.flushAll(), e=>e instanceof AggregateError && e.errors.length===1 && e.errors[0]===e1);
  cases.push('one error still AggregateError, not unwrapped');
  trace=[]; t=new ctx.Tracker('oracle'); t.claimWrite('placeholder');
  t.adopt(handle('closed',{error:new ctx.Closed('closed','flush')})); t.adopt(handle('healthy'));
  await t.flushAll(); assert.deepEqual(trace,['drain:closed','drain:healthy','flush:healthy']);
  cases.push('null claim skipped and actual locked SessionHandleClosedError ignored');
  trace=[]; t=new ctx.Tracker('oracle'); const z=handle('z'); const a=handle('a');
  t.claimWrite('z'); t.adopt(z); t.adopt(a); t.release(z,true); t.claimWrite('z'); t.adopt(z);
  await t.flushAll(); assert.deepEqual(trace,['drain:a','flush:a','drain:z','flush:z']);
  cases.push('closed/reopened registration moves to end');
  trace=[]; t=new ctx.Tracker('oracle'); hold=deferred();
  t.adopt(handle('first',{hold})); const old=handle('replace'); t.adopt(old);
  f=t.flushAll(); t.release(old,true); t.adopt(handle('replacement'));
  hold.release(); await f; assert.deepEqual(trace,['drain:first','flush:first','drain:replace','flush:replace']);
  cases.push('in-flight snapshot retains original handles after registry replacement');
  console.log(JSON.stringify({cases, passed:cases.length, failed:0, backendVersion:'0.2.1-alpha.2',
    backendSourceSha256:sha(source), lockedAbstractSha256:sha(abstract), extractedTrackerSha256:sha(tracker), extractedClosedErrorSha256:sha(closed),
    scope:'actual published tracker plus test-owned handle ports; not full filesystem/Agent integration; supplemental matching release, pinned prototype unchanged'},null,2));
})().catch(e=>{console.error(e);process.exitCode=1});
