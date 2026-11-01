/** Additional exact differential checks with a clock that advances on EVERY read.
 * This exposes clock evaluation order hidden by the upstream fixed-time fixture.
 */
import { afterAll, afterEach, expect, it } from 'vitest'
import { Engine as ReferenceEngine } from './upstream/src/core/engine.ts'
import * as core from './upstream/src/core/index.ts'
import { createEngineAdapter, cleanupEngines, saveEvidence } from './engine-target-adapter.ts'
const TargetEngine = createEngineAdapter(core)
const clean = (value: any) => value === undefined ? { $undefined: true } : JSON.parse(JSON.stringify(value))
function fixture(Constructor: any, events: boolean) {
  const repo: any = new core.MemoryRepositories()
  const state = { clock: 0, paused: false, version: 'v1', failPrepare: false }
  const calls: any[] = [], emitted: any[] = [], messages: any[] = [], withdrawn: string[] = [], requests: any[] = []
  const policy = { get version() { return state.version }, get paused() { return state.paused }, evalTimeoutMs: 60000,
    gate: (trigger: any, thread: any) => !trigger.human ? { admissible: false, reason: 'not human' } : trigger.actor.id === 'alice'
      ? { admissible: true, strength: 'explicit', basis: { via: 'listed' } } : thread.subscriptions.length
        ? { admissible: true, strength: 'inferred', basis: { via: 'followup' } } : { admissible: false, reason: 'not a member' },
    revalidate: () => state.version !== 'revoked', admitsInferred: (rules: string[]) => rules.includes('enabled') }
  const observe = (type: string, args: any[]) => calls.push({ type, args: clean(args) })
  const engine = new Constructor({ repo, policy: () => policy,
    scope: { scope: (input: any) => input.meta.blocked ? { listened: false, reason: 'filtered' } : { listened: true, observed: false } },
    now: () => new Date(Date.UTC(2026, 8, 23) + state.clock++ * 1000).toISOString(),
    assessor: { request: (candidate: any) => { requests.push(clean(candidate)); observe('assessor.request', [candidate]) }, release: (...args: any[]) => observe('assessor.release', args) },
    executor: Object.fromEntries(['start', 'deliver', 'stop'].map(name => [name, (...args: any[]) => observe('executor.' + name, args)])),
    outbox: { send: (message: any) => { if (!messages.some(m => m.id === message.id)) messages.push(clean(message)); observe('outbox.send', [message]) },
      status: (id: string) => withdrawn.includes(id) ? 'dropped' : messages.some(m => m.id === id) ? 'pending' : undefined,
      withdraw: (id: string) => { withdrawn.push(id); observe('outbox.withdraw', [id]) } },
    prepareTicket: (ticket: any) => { if (state.failPrepare) throw new Error('fixture prepare failed'); return ticket },
    ...events ? { emit: (event: any) => emitted.push(clean(event)) } : {},
  })
  const snapshot = () => clean({ state, triggers: [...repo.triggerRows.values()], dispositions: [...repo.dispositions], threads: [...repo.threadRows.values()], tickets: [...repo.ticketRows.values()], decisions: [...repo.decisionRows.values()], calls, emitted, messages, withdrawn, requests })
  return { engine, repo, state, snapshot }
}
function trigger(id: string, meta = {}) { return { id, source: 'chat', threads: [{ source: 'chat', id: 't1' }], actor: { source: 'chat', id: 'alice' }, human: true, at: '2026-09-23T00:00:01Z', text: id, channel: { source: 'chat', thread: 't1', address: { message: id } }, meta } }
for (const events of [false, true]) {
  it(`matches advancing-clock command, callback and state order; event sink ${events}`, () => {
    const reference = fixture(ReferenceEngine, events), target = fixture(TargetEngine, events)
    const command = (method: string, ...args: any[]) => {
      const invoke = (engine: any) => { try { return { ok: clean(engine[method](...structuredClone(args))) } } catch (error: any) { return { error: { message: error.message, code: error.code } } } }
      const expected = invoke(reference.engine), actual = invoke(target.engine)
      expect(actual, `result of ${method}`).toEqual(expected)
      expect(target.snapshot(), `snapshot after ${method}`).toEqual(reference.snapshot())
      return (expected as any).ok
    }
    command('receive', trigger('filtered', { blocked: true }))
    command('receive', trigger('1')); command('receive', trigger('1')); command('evaluateDue')
    const candidate = command('candidate', '1')
    command('candidateDelivered', candidate.evalId)
    command('decide', '1', { kind: 'accept', title: 'Fix', scope: 'small', reason: 'requested', acknowledgement: 'ok', evalId: candidate.evalId })
    const ticket = reference.repo.tickets.list()[0], id = ticket.id, assignment = ticket.assignments[0].id
    command('assignmentStarted', id, assignment)
    command('followForContext', id, { source: 'chat', id: 'context' })
    command('receive', trigger('2'))
    const input = reference.repo.tickets.get(id).inputs[0].id
    command('inputsDelivered', id, assignment, [input]); command('inputsRead', id, [input])
    command('report', id, { state: 'waiting', summary: 'waiting', question: 'which?', askedBy: '2' })
    command('reply', { purpose: 'attention', ticketId: id, text: 'which?', question: 'which?' })
    command('report', id, { state: 'progress', summary: 'working', question: null })
    command('deliveryFailed', id, assignment, 'failure'); command('retryTicket', id)
    const replacement = reference.repo.tickets.get(id).assignments.at(-1).id
    command('assignmentStarted', id, assignment); command('assignmentStarted', id, replacement)
    command('turnEnded', id, replacement); command('note', id, 'review carefully')
    command('bindThread', id, { source: 'github', id: 'pr-1' }, true)
    command('report', id, { state: 'ready', summary: 'done' })
    command('threadClosed', { source: 'github', id: 'pr-1' }, 'merged', '2026-09-23T01:00:00Z')
    command('reply', { purpose: 'result', ticketId: id, text: 'done' })
    command('receive', trigger('3')); command('assignmentInterrupted', id, replacement)
    command('stopTicket', id, ''); command('retryTicket', id)
    reference.state.version = target.state.version = 'v2'
    command('policyChanged', 'v1', 'v2', new Map())
    command('evaluateDue')
  })
}
afterEach(cleanupEngines)
afterAll(() => saveEvidence('-clock', 'exact reference-versus-Rust mixed command walks and advancing-clock order, with original scripted fixture ports; not real Runtime/source coverage'))

// Seeded, bounded command walks include successful actions, stale callbacks and
// rejected commands. Rejections are compared, never silently filtered or skipped.
for (let seed = 1; seed <= 12; seed++) {
  it(`matches seeded mixed command walk ${seed}`, () => {
    const reference = fixture(ReferenceEngine, seed % 2 === 0), target = fixture(TargetEngine, seed % 2 === 0)
    let random = seed
    const next = (limit: number) => { random = (Math.imul(random, 1664525) + 1013904223) >>> 0; return random % limit }
    const pick = (items: any[]) => items.length ? items[next(items.length)] : undefined
    const command = (method: string, ...args: any[]) => {
      const invoke = (engine: any) => { try { return { ok: clean(engine[method](...structuredClone(args))) } } catch (error: any) { return { error: { message: error.message, code: error.code } } } }
      const expected = invoke(reference.engine), actual = invoke(target.engine)
      expect(actual, `seed ${seed}, ${method}, args ${JSON.stringify(args)}`).toEqual(expected)
      expect(target.snapshot(), `seed ${seed}, snapshot after ${method}, args ${JSON.stringify(args)}`).toEqual(reference.snapshot())
      return (expected as any).ok
    }
    command('receive', trigger('origin')); command('evaluateDue')
    const ticket = command('decide', 'origin', { kind: 'accept', title: 'Fix', scope: 'small', reason: 'requested', acknowledgement: 'ok' })
    command('assignmentStarted', ticket.ticketId, reference.repo.tickets.get(ticket.ticketId).assignments[0].id)
    for (let step = 0; step < 100; step++) {
      const tickets = reference.repo.tickets.list(), row = pick(tickets), id = row?.id ?? 'unknown-ticket'
      const received = pick([...reference.repo.triggerRows.values()]), triggerId = received?.id ?? 'unknown-trigger'
      const assignment = pick(row?.assignments ?? [])?.id ?? 'unknown-assignment'
      const thread = { source: 'chat', id: pick(['t1', 'side', 'extra']) }
      const inputIds = row?.inputs.map((input: any) => input.id) ?? ['unknown-input']
      const active = pick([...reference.repo.threadRows.values()].filter((thread: any) => thread.evaluating))?.evaluating
      switch (next(27)) {
        case 0: case 1: case 2: {
          const t = trigger('m' + step, next(9) === 0 ? { blocked: true } : {})
          t.human = next(4) !== 0; t.actor.id = next(3) ? 'alice' : 'guest'
          if (next(3) === 0) t.threads = [thread]
          command('receive', t); break
        }
        case 3: command('evaluateDue'); break
        case 4: command('candidateDelivered', active?.id ?? 'unknown-eval'); break
        case 5: {
          const kind = pick(['accept', 'ignore', 'unavailable', 'stop'])
          command('decide', triggerId, { kind, reason: 'because', title: 'Fix', scope: 'small', ticketId: id, rules: ['enabled'], ...next(3) === 0 && active ? { evalId: active.id } : {} }); break
        }
        case 6: command('assignmentStarted', id, assignment); break
        case 7: command('inputsDelivered', id, assignment, inputIds); break
        case 8: command('inputsRead', id, inputIds); break
        case 9: {
          const state = pick(['progress', 'waiting', 'blocked', 'failed', 'ready', 'dropped'])
          const variant = next(4)
          command('report', id, { state, summary: 'state ' + step, ...variant === 0 ? { question: 'Which?', askedBy: triggerId } : variant === 1 ? { question: null } : variant === 2 ? { answers: triggerId } : {} }); break
        }
        case 10: command('deliveryFailed', id, assignment, 'delivery error'); break
        case 11: command('retryTicket', id); break
        case 12: command('stopTicket', id, pick(['', undefined, 'stop'])); break
        case 13: command('requestStatus', id); break
        case 14: command('reevaluate', triggerId); break
        case 15: command('handback', id, triggerId, 'asks more'); break
        case 16: command('attach', id, triggerId, next(2) ? 'context' : undefined); break
        case 17: command('followForContext', id, thread); break
        case 18: command('bindThread', id, thread, next(2) === 0); break
        case 19: command('unsubscribe', core.threadKey(thread)); break
        case 20: command('markUnrelated', id, core.threadKey(thread), 'unrelated'); break
        case 21: command('threadClosed', thread, next(2) ? 'merged' : 'closed', '2026-09-23T02:00:00Z'); break
        case 22: command('note', id, 'note ' + step); break
        case 23: command('reply', { purpose: pick(['acknowledgement', 'answer', 'attention', 'result']), ticketId: id, triggerId, text: 'reply ' + step, question: 'Which?' }); break
        case 24: command('assignmentInterrupted', id, assignment); break
        case 25: command('turnEnded', id, assignment); break
        case 26: {
          const from = reference.state.version, to = next(4) === 0 ? 'revoked' : 'v' + step
          reference.state.version = target.state.version = to
          reference.state.paused = target.state.paused = next(4) === 0
          reference.state.failPrepare = target.state.failPrepare = next(5) === 0
          const effects = new Map(tickets.filter(() => next(2)).map((ticket: any) => [ticket.id, { requesterAllowed: next(2) === 0 }]))
          command('policyChanged', from, to, effects); break
        }
      }
    }
  })
}
