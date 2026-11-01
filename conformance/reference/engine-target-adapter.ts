/** Test transport only. Each command executes Rust; fixtures retain their real JS
 * Policy/Scope/Clock callbacks, vi.fn observation ports and clone-on-read Maps.
 * Neither expected values nor the original Engine are used to compute a result.
 */
import { spawn, execFileSync } from 'node:child_process'
import { constants, openSync, closeSync, readSync, writeSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { gzipSync } from 'node:zlib'

const sleep = new Int32Array(new SharedArrayBuffer(4))
const instances = new Set<any>()
const counts: Record<string, number> = {}
const callbacks: Record<string, number> = {}
const traces: any[] = []
const maps = [['triggers', 'triggerRows', 'id'], ['threads', 'threadRows', 'key'], ['tickets', 'ticketRows', 'id'], ['decisions', 'decisionRows', 'id']] as const
const clean = (value: any) => value === undefined ? { $undefined: true } : JSON.parse(JSON.stringify(value))

export function createEngineAdapter(original: any) {
  class RustEngine {
    options: any
    directory: string
    child: any
    input: number
    output: number
    pending = Buffer.alloc(0)
    policies: any[] = []
    constructor(options: any) {
      this.options = options
      const binary = process.env.SUPERAGENT_ENGINE_BRIDGE_BIN
      if (!binary) throw new Error('SUPERAGENT_ENGINE_BRIDGE_BIN must point to the prebuilt remote executable')
      this.directory = mkdtempSync(join(tmpdir(), 'superagent-engine-'))
      const input = join(this.directory, 'input'), output = join(this.directory, 'output')
      // mkdtemp's private 0700 directory limits the local test-only FIFO endpoints.
      execFileSync('mkfifo', ['-m', '600', input, output])
      this.input = openSync(input, constants.O_RDWR | constants.O_NONBLOCK)
      this.output = openSync(output, constants.O_RDWR | constants.O_NONBLOCK)
      this.child = spawn(binary, [input, output], { stdio: ['ignore', 'ignore', 'inherit'] })
      instances.add(this)
    }
    write(value: any) {
      const bytes = Buffer.from(JSON.stringify(value) + '\n')
      let offset = 0
      const deadline = Date.now() + 20000
      while (offset < bytes.length) {
        try { offset += writeSync(this.input, bytes, offset) }
        catch (error: any) { if (error.code !== 'EAGAIN') throw error; if (Date.now() > deadline) throw new Error('Rust bridge write timeout'); Atomics.wait(sleep, 0, 0, 1) }
      }
    }
    read() {
      const deadline = Date.now() + 20000
      for (;;) {
        const newline = this.pending.indexOf(10)
        if (newline >= 0) {
          const frame = this.pending.subarray(0, newline)
          this.pending = this.pending.subarray(newline + 1)
          return JSON.parse(frame.toString('utf8'))
        }
        const chunk = Buffer.allocUnsafe(65536)
        try {
          const length = readSync(this.output, chunk)
          if (length) this.pending = Buffer.concat([this.pending, chunk.subarray(0, length)])
          else Atomics.wait(sleep, 0, 0, 1)
        } catch (error: any) { if (error.code !== 'EAGAIN') throw error; Atomics.wait(sleep, 0, 0, 1) }
        if (Date.now() > deadline) throw new Error(`Rust bridge response timeout, child ${this.child.pid}`)
        if (this.pending.length > 64 * 1024 * 1024) throw new Error('Rust bridge exceeded frame limit')
      }
    }
    callback(method: string, args: any[]) {
      callbacks[method] = (callbacks[method] ?? 0) + 1
      const options = this.options
      if (method === 'policy') {
        const policy = options.policy()
        const token = this.policies.push(policy) - 1
        return { token, version: policy.version, paused: policy.paused, timeout: policy.evalTimeoutMs }
      }
      if (method.startsWith('policy.')) {
        const [token, ...rest] = args
        return this.policies[token][method.slice(7)](...rest)
      }
      if (method === 'scope') return options.scope.scope({ source: args[0], meta: args[1] })
      if (method === 'now') return options.now?.() ?? new Date().toISOString()
      if (method === 'emit') { options.emit?.(...args); return null }
      if (method === 'prepareTicket') return options.prepareTicket?.(...args) ?? args[0]
      const [port, action] = method.split('.')
      return options[port][action](...args)
    }
    command(method: string, args: any[]) {
      counts[method] = (counts[method] ?? 0) + 1
      const repo = this.options.repo
      const records: any = { dispositions: [...repo.dispositions.entries()] }
      for (const [name, field] of maps) records[name] = [...repo[field].values()]
      if (method === 'policyChanged' && args[2] instanceof Map) args[2] = [...args[2].entries()]
      const request = { kind: 'command', method, args, records, emitEnabled: !!this.options.emit }
      const trace = { request: clean(request), callbacks: [] as any[], response: null as any }
      traces.push(trace)
      this.write(request)
      for (;;) {
        const frame = this.read()
        if (frame.kind === 'callback') {
          let response
          try { response = { value: this.callback(frame.method, frame.args) ?? null } }
          catch (error: any) {
            response = { error: error instanceof original.UserError ? { type: 'UserError', code: error.code, values: error.values, missing: error.missing, message: error.message } : { type: 'Error', message: error.message } }
          }
          trace.callbacks.push({ request: frame, response: clean(response) })
          this.write(response)
          continue
        }
        if (frame.kind !== 'result') throw new Error(`Unexpected Rust frame ${JSON.stringify(frame)}`)
        trace.response = frame
        for (const [name, field, key] of maps) {
          repo[field].clear()
          for (const value of frame.records[name]) repo[field].set(value[key], structuredClone(value))
        }
        repo.dispositions.clear()
        for (const [id, value] of frame.records.dispositions) repo.dispositions.set(id, structuredClone(value))
        this.policies.length = 0
        if (frame.error) {
          if (frame.error.type === 'UserError') throw new original.UserError(frame.error.code, frame.error.values)
          throw new Error(frame.error.message)
        }
        return frame.value?.$undefined === true ? undefined : frame.value
      }
    }
    close() {
      try { this.write({ kind: 'close' }) } catch { /* process is terminated below on failed transport too */ }
      this.child.kill('SIGTERM')
      closeSync(this.input); closeSync(this.output)
      rmSync(this.directory, { recursive: true, force: true })
      instances.delete(this)
    }
  }
  for (const method of ['receive', 'bindThread', 'threadClosed', 'evaluateDue', 'candidate', 'candidateDelivered', 'decide', 'reevaluate', 'dismiss', 'unsubscribe', 'stopTicket', 'retryTicket', 'requestStatus', 'reply', 'note', 'followForContext', 'handback', 'attach', 'markUnrelated', 'assignmentStarted', 'inputsDelivered', 'inputsRead', 'turnEnded', 'deliveryFailed', 'assignmentInterrupted', 'report', 'policyChanged']) {
    Object.defineProperty(RustEngine.prototype, method, { value: function(...args: any[]) { return this.command(method, args) } })
  }
  return RustEngine
}
export function cleanupEngines() { for (const instance of instances) instance.close() }
export function saveEvidence(suffix = '', scope = 'unchanged upstream core-engine.spec.ts assertions against Rust Engine; original Policy, Scope, Clock, port callbacks, repository projection and model helper controls') {
  const basePath = process.env.SUPERAGENT_ENGINE_TARGET_EVIDENCE
  const path = basePath ? basePath.replace(/\.json$/, suffix + '.json') : undefined
  const report = { schema_version: 1, repository_backend: process.env.SUPERAGENT_ENGINE_REPOSITORY ?? 'memory', reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e', scope, command_counts: counts, callback_counts: callbacks, commands: traces.length }
  if (path) { writeFileSync(path, JSON.stringify(report, null, 2) + '\n'); writeFileSync(path + '.traces.json.gz', gzipSync(JSON.stringify(traces))) }
  console.log('RUST_ENGINE_TARGET_EVIDENCE ' + JSON.stringify(report))
}
