/** Test-only transport, not a Store/queue reimplementation: all persistence,
 * query/working-set selection, generation fencing, retry/backoff and health state
 * are executed by Rust. JS only runs supplied test callbacks and Promise scheduling.
 */
import { spawn, execFileSync } from 'node:child_process'
import { constants, openSync, closeSync, readSync, writeSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { gzipSync } from 'node:zlib'
const sleep = new Int32Array(new SharedArrayBuffer(4))
const clients = new Set<any>(), traces: any[] = []
const counts: Record<string, number> = {}
const encode = (value: any) => value === undefined ? { $undefined: true } : value
const decode = (value: any) => value?.$undefined === true ? undefined : value
class Client {
  directory: string; child: any; input: number; output: number; pending = Buffer.alloc(0)
  closed = false; actions = new Map<number, () => unknown>(); next = 0; limits = new Map<number, any>()
  report: (error: unknown) => unknown = () => {}
  source: any; validateIm: any; promptInputs = new Map<number, any>(); promptError?: (error: any) => never
  constructor() {
    const binary = process.env.SUPERAGENT_STORE_BRIDGE_BIN
    if (!binary) throw new Error('SUPERAGENT_STORE_BRIDGE_BIN must point to a prebuilt remote executable')
    this.directory = mkdtempSync(join(tmpdir(), 'superagent-store-'))
    const input = join(this.directory, 'input'), output = join(this.directory, 'output')
    execFileSync('mkfifo', ['-m', '600', input, output])
    this.input = openSync(input, constants.O_RDWR | constants.O_NONBLOCK)
    this.output = openSync(output, constants.O_RDWR | constants.O_NONBLOCK)
    this.child = spawn(binary, [input, output], { stdio: ['ignore', 'ignore', 'inherit'] })
    clients.add(this)
  }
  write(value: any) {
    const bytes = Buffer.from(JSON.stringify(value) + '\n'); let offset = 0
    const deadline = performance.now() + 20000
    while (offset < bytes.length) {
      try { offset += writeSync(this.input, bytes, offset) }
      catch (error: any) { if (error.code !== 'EAGAIN') throw error; if (performance.now() > deadline) throw new Error('Store bridge write timeout'); Atomics.wait(sleep, 0, 0, 1) }
    }
  }
  read() {
    // Fake Date in upstream tests must not turn a transport deadline into infinity.
    const deadline = performance.now() + 20000
    for (;;) {
      const newline = this.pending.indexOf(10)
      if (newline >= 0) { const line = this.pending.subarray(0, newline); this.pending = this.pending.subarray(newline + 1); return JSON.parse(line.toString('utf8')) }
      const bytes = Buffer.allocUnsafe(65536)
      try { const length = readSync(this.output, bytes); if (length) this.pending = Buffer.concat([this.pending, bytes.subarray(0, length)]); else Atomics.wait(sleep, 0, 0, 1) }
      catch (error: any) { if (error.code !== 'EAGAIN') throw error; Atomics.wait(sleep, 0, 0, 1) }
      if (performance.now() > deadline) throw new Error('Store bridge response timeout')
      if (this.pending.length > 64 * 1024 * 1024) throw new Error('Store bridge frame limit exceeded')
    }
  }
  call(method: string, args: any[] = [], now?: number): any {
    if (this.closed) throw new Error('Database is closed')
    counts[method] = (counts[method] ?? 0) + 1
    const request = { kind: 'command', method, args: args.map(encode), now }
    const trace = { request, callbacks: [] as any[], response: null as any }; traces.push(trace)
    this.write(request)
    for (;;) {
      const response = this.read()
      if (response.kind === 'callback') {
        let result
        try {
          let value
          if (response.method === 'promptInput') { const input = this.promptInputs.get(response.args[0]); if (!input) throw new Error('Missing fixture prompt inputs'); value = input[response.args[1]]() }
          else if (response.method === 'limit') value = this.limits.get(response.args[0])(response.args[1])
          else if (response.method === 'validateIm') { this.validateIm(response.args[0]); value = null }
          else if (response.method === 'sourceEnabled') value = !!this.source.instance(response.args[0])?.enabled
          else if (response.method === 'channelAllowed') value = this.source.channelAllowed(response.args[0])
          else throw new Error('Unknown callback')
          result = { value }
        } catch (error: any) { result = { error: { message: response.method === 'limit' ? String(error) : error?.message ?? String(error) } } }
        trace.callbacks.push({ request: response, response: result }); this.write(result); continue
      }
      trace.response = response
      if (response.error) { if (method.startsWith('prompts.') && this.promptError) this.promptError(response.error); throw new Error(response.error.message) }
      for (const action of response.fired ?? []) {
        const callback = this.actions.get(action.id); this.actions.delete(action.id)
        if (!callback) throw new Error('Missing fixture afterCommit callback')
        try { callback() } catch (error) { if (action.committed) this.report(error); else throw error }
      }
      return decode(response.value)
    }
  }
  shutdown() {
    if (this.closed) return
    this.closed = true
    try { this.write({ kind: 'close' }) } catch { /* cleanup after failed IPC too */ }
    this.child.kill('SIGTERM'); closeSync(this.input); closeSync(this.output)
    rmSync(this.directory, { recursive: true, force: true }); clients.delete(this)
  }
}
export class RustDatabaseSync {
  client: Client
  constructor(path: string, options: any = {}) { this.client = new Client(); this.client.call('open', [path, !!options.createParent]) }
  exec(sql: string) { return this.client.call('sql.exec', [sql]) }
  prepare(sql: string) { return { run: (...args: any[]) => this.client.call('sql.run', [sql, args]), get: (...args: any[]) => this.client.call('sql.get', [sql, args]), all: (...args: any[]) => this.client.call('sql.all', [sql, args]) } }
  close() { try { this.client.call('close') } finally { this.client.shutdown() } }
}
export function createStoreAdapter(DatabaseSync: any) {
  return class RustStore {
    db: any; receipts: any; triggers: any; threads: any; tickets: any; decisions: any; read: any
    constructor(path: string, report: (error: unknown) => unknown = () => {}) {
      this.db = new DatabaseSync(path, { createParent: true }); this.db.client.report = report
      try { this.call('initialize', [Date.now()]) } catch (error) { this.db.close(); throw error }
      this.triggers = { get: (id: string) => this.call('aggregate.get', ['trigger', id]), save: (value: any) => this.call('aggregate.save', ['trigger', value]), disposition: (id: string) => this.call('disposition.get', [id]), saveDisposition: (...args: any[]) => this.call('disposition.save', args) }
      const methods = (table: string) => ({ get: (id: string) => this.call('aggregate.get', [table, id]), save: (value: any) => this.call('aggregate.save', [table, value]), list: () => this.call('aggregate.list', [table]), active: () => this.call('aggregate.list', [table, 'active']) })
      this.threads = { ...methods('thread'), describe: (...args: any[]) => this.call('thread.describe', args), patchMeta: (id: string, patch: any) => this.call('thread.patchMeta', [id, Object.entries(patch).map(([key, value]) => [key, encode(value)])]) }
      this.tickets = { ...methods('ticket'), open: () => this.call('aggregate.list', ['ticket', 'open']) }
      this.decisions = { get: (id: string) => this.call('aggregate.get', ['decision', id]), save: (value: any) => this.call('aggregate.save', ['decision', value]), byThread: (id: string) => this.call('decisions.byThread', [id]) }
      this.read = Object.fromEntries(['recentTriggers', 'triggerStats', 'flow', 'triggers', 'threadTriggers', 'threadPage', 'neighbours', 'firstTrigger', 'search', 'recentDecisions', 'decisions', 'decisionsOfKinds', 'failingReplies', 'droppedReplies', 'droppedSince', 'subscribedThreads'].map(method => [method, (...args: any[]) => this.call('read', [method, args])]))
      this.receipts = Object.fromEntries(['get', 'waiting', 'behind', 'add', 'withdraw'].map(method => [method, (...args: any[]) => this.call('receipts.' + method, args)]))
      this.receipts.settle = (message: any, outcome: string) => this.call('receipts.settle', [message, outcome, new Date().toISOString()])
    }
    call(method: string, args: any[] = []) { return this.db.client.call(method, args) }
    close() { return this.db.close() }
    atomic(operation: () => unknown): any { this.call('begin'); let value; try { value = operation() } catch (error) { this.call('rollback'); throw error } this.call('commit'); return value }
    afterCommit(action: () => unknown) { const id = ++this.db.client.next; this.db.client.actions.set(id, action); this.call('afterCommit', [id]) }
    get(name: string) { const row = this.call('get', [name]); return row.exists ? JSON.parse(row.encoded) : undefined }
    set(name: string, value: any) { return this.call('set', [name, value]) }
    delete(name: string) { return this.call('delete', [name]) }
    keys(family: any) { return this.call('keys', [typeof family === 'string' ? family : family.prefix]) }
    settings() { return JSON.parse(this.call('settings').encoded) }
    blockRevision(block: string) { return this.call('blockRevision', [block]) }
    saveSettings(input: any, record?: any) { return JSON.parse(this.call('saveSettings', [JSON.stringify(input), record ?? null, new Date().toISOString()]).encoded) }
    settingsHistory(limit = 20) { return JSON.parse(this.call('settingsHistory', [limit]).encoded) }
    replyStatus(id: string) { return this.receipts.get(id)?.status }
  }
}
export class Later extends Error {}
export class RustEffects {
  store: any; id: number; board: any; now: () => number; handlers = new Map<string, any>(); health = new Map<string, any>()
  tasks = new Set<any>(); scheduled = false
  constructor(store: any, board: any, now = () => Date.now()) { this.store = store; this.board = board; this.now = now; this.id = store.db.client.call('queue.new', [now()]); store.db.client.limits.set(this.id, (kind: string) => this.handlers.get(kind).limit()) }
  call(method: string, args: any[] = []) { return this.store.db.client.call('queue.' + method, [this.id, ...args], this.now()) }
  sync() { const health = this.call('health'); for (const [kind, value] of Object.entries(health)) this.health.get(kind)?.set(value) }
  handle(kind: string, run: any, options: any = {}) {
    // Rust rejects duplicate registrations before the test fixture claims health again.
    this.call('register', [kind, { retry: options.retry ?? [2000, 60000], ordered: options.ordered ?? false, quiet: options.quiet ?? false }])
    this.health.set(kind, this.board.claim(kind)); this.handlers.set(kind, { run, limit: options.limit ?? (() => 1) }); this.sync(); this.kick()
  }
  enqueue(kind: string, key: string, payload: any) { this.call('enqueue', [kind, key, payload]); this.kick() }
  pending(kind: string, key?: string) { return this.call('pending', key === undefined ? [kind] : [kind, key]) }
  cancel(kind: string, key: string) { this.call('cancel', [kind, key]); this.store.afterCommit(() => { this.call('refresh', [kind]); this.sync() }) }
  start() { this.call('start'); this.kick() }
  kick() { if (this.scheduled) return; this.scheduled = true; queueMicrotask(() => { this.scheduled = false; this.dispatch() }) }
  dispatch() {
    if (this.store.db.client.closed) return
    const attempts = this.call('dispatch'); this.sync()
    for (const attempt of attempts) {
      const task: any = { kind: attempt.row.kind, done: null }
      const execute = async () => {
        let outcome = 'success', error = ''
        try { await this.handlers.get(attempt.row.kind).run(JSON.parse(attempt.row.value), attempt.row.key) }
        catch (failure) { outcome = failure instanceof Later ? 'later' : 'failure'; error = String(failure) }
        this.call('complete', [attempt.id, outcome, error]); this.sync()
      }
      task.done = execute().catch(error => { if (!this.store.db.client.closed) { this.call('failed', [attempt.row.kind, String(error)]); this.sync() } }).finally(() => { this.tasks.delete(task); this.kick() })
      this.tasks.add(task)
    }
  }
  async idle(kind?: string) { for (;;) { this.dispatch(); const running = [...this.tasks].filter(task => kind === undefined || task.kind === kind).map(task => task.done); if (!running.length) return; await Promise.allSettled(running) } }
  async drain() { this.call('drain'); await Promise.allSettled([...this.tasks].map(task => task.done)) }
}
export function cleanupStoreClients() { for (const client of clients) client.shutdown() }
export function saveStoreEvidence(suffix = '', scope = 'unchanged original Store/Effects/Outbox assertions through actual Rust persistence, query, generation/backoff/health protocol and receipt delivery planning; JS callback bodies, Promise scheduling, HealthBoard, Intl text segmentation, Runtime, Sources and helper controls remain original fixtures; not production Runtime/source coverage') {
  const base = process.env.SUPERAGENT_STORE_TARGET_EVIDENCE
  const path = base ? base.replace(/\.json$/, suffix + '.json') : undefined
  const report = { schema_version: 1, reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e', scope, calls: counts, total_calls: Object.values(counts).reduce((a, b) => a + b, 0) }
  if (path) { writeFileSync(path, JSON.stringify(report, null, 2) + '\n'); writeFileSync(path + '.traces.json.gz', gzipSync(JSON.stringify(traces))) }
  console.log('RUST_STORE_TARGET_EVIDENCE ' + JSON.stringify(report))
}

export function createOutboxAdapter(checkedImText: any) {
  return class RustOutbox {
    host: any
    constructor(host: any) {
      this.host = host; host.store.db.client.source = host.sources; host.store.db.client.validateIm = checkedImText
      host.effects.handle('reply', (_payload: any, id: string) => this.deliver(id), { limit: () => host.paused() ? 0 : 1, retry: [5000, 300000] })
    }
    send(message: any) { this.host.store.db.client.call('outbox.send', [this.host.effects.id, message], this.host.effects.now()); this.host.effects.kick() }
    status(id: string) { return this.host.store.receipts.get(id)?.status }
    withdraw(id: string) {
      const removed = this.host.store.db.client.call('outbox.withdraw', [this.host.effects.id, id])
      if (removed) this.host.store.afterCommit(() => { this.host.effects.call('refresh', ['reply']); this.host.effects.sync() })
    }
    sent() { return this.host.effects.idle('reply') }
    async deliver(id: string) {
      const plan = this.host.store.db.client.call('outbox.prepare', [id, new Date().toISOString()])
      if (plan.state === 'later') throw new Later(plan.reason)
      if (plan.state !== 'send') return
      const result = await this.host.sources.send(plan.receipt, this.host.commands)
      const settled = this.host.store.db.client.call('outbox.settle', [plan.receipt, result, new Date().toISOString()])
      if (settled.state === 'later') throw new Later(settled.reason)
    }
  }
}
