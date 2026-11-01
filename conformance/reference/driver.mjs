import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { Engine, MemoryRepositories, UserError, runningAssignment, ticketStage } from './upstream/src/core/index.ts';
import { SettingsSchema, settingsPolicy, assessmentOutcome, policyVersion, policyEffects } from './upstream/src/policy.ts';

const copy = value => value === undefined ? { $undefined: true } : structuredClone(value);
const merge = (base, patch) => Object.fromEntries([...new Set([...Object.keys(base), ...Object.keys(patch)])].map(key => {
  const value = patch[key];
  return [key, value === undefined ? base[key] : value && typeof value === 'object' && !Array.isArray(value)
    && base[key] && typeof base[key] === 'object' && !Array.isArray(base[key]) ? merge(base[key], value) : value];
}));

/** Run the unmodified reference domain with recording ports, never a product replacement. */
export function runScenario(scenario) {
  if (scenario.schema_version !== 1) throw new Error('Unsupported scenario schema');
  const repo = new MemoryRepositories();
  let time = Date.parse(scenario.initial?.time ?? '2026-10-31T00:00:00.000Z');
  if (!Number.isFinite(time)) throw new Error('Invalid fixture clock');
  let settings = SettingsSchema.parse(merge({
    sources: { github: { enabled: true, config: {} } },
    policy: { sources: { github: { allowed: [{ id: 'alice' }] } } },
  }, scenario.initial?.settings ?? {}));
  const calls = [], events = [], requests = [], outbox = [], withdrawn = [];
  const launches = [], deliveries = new Set(), knownTriggers = new Set(), aliases = {};
  const scopeFor = configuration => ({ scope: ({ source, meta }) => {
    // SourceScope is an explicitly scripted test port, not a replacement Source implementation.
    if (meta?.blocked || configuration.sources[source]?.enabled === false) return { listened: false, reason: 'fixture_scope_filtered' };
    return { listened: true, observed: meta?.observed ?? true, row: meta?.row ?? 'comments' };
  } });
  const scope = { scope: input => scopeFor(settings).scope(input) };
  const now = () => new Date(time).toISOString();
  const executor = {
    start: (ticket, assignment) => { calls.push({ type: 'executor.start', ticket: copy(ticket), assignment: copy(assignment) }); launches.push([ticket.id, assignment.id]); },
    deliver: (ticket, assignment) => { calls.push({ type: 'executor.deliver', ticketId: ticket.id, assignmentId: assignment.id }); deliveries.add(ticket.id); },
    stop: (ticket, assignment, reason) => { calls.push({ type: 'executor.stop', ticketId: ticket.id, assignmentId: assignment.id, reason }); },
  };
  const engine = new Engine({ repo, scope, now, policy: () => settingsPolicy(settings, scope), executor,
    assessor: { request: c => { requests.push(copy(c)); calls.push({ type: 'assessor.request', candidate: copy(c) }); },
      release: evalId => calls.push({ type: 'assessor.release', evalId }) },
    outbox: { send: m => { if (!outbox.some(item => item.id === m.id)) outbox.push(copy(m)); calls.push({ type: 'outbox.send', message: copy(m) }); },
      status: id => withdrawn.includes(id) ? 'dropped' : outbox.some(m => m.id === id) ? 'pending' : undefined,
      withdraw: id => { withdrawn.push(id); calls.push({ type: 'outbox.withdraw', id }); } },
    emit: event => events.push(copy(event)),
    ...(scenario.initial?.prepare_fails ? { prepareTicket: () => { throw new Error('fixture_prepare_failure'); } } : {}),
  });
  const resolve = value => typeof value === 'string' && value.startsWith('$') ? aliases[value.slice(1)] : value;
  const ticket = value => {
    const item = repo.tickets.get(resolve(value));
    if (!item) throw new Error('Unknown fixture ticket alias');
    return item;
  };
  const assignment = value => {
    const a = runningAssignment(ticket(value));
    if (!a) throw new Error('Fixture ticket has no running assignment');
    return a;
  };
  const settle = () => {
    for (let budget = 0; budget < 1000; budget++) {
      const launch = launches.shift();
      if (launch) { engine.assignmentStarted(...launch); continue; }
      const [id] = deliveries;
      if (id === undefined) return;
      deliveries.delete(id);
      const item = repo.tickets.get(id), a = item && runningAssignment(item);
      if (item && !item.ended && a) engine.inputsDelivered(id, a.id, [...item.undelivered]);
    }
    throw new Error('Fixture effect budget exhausted');
  };
  const snapshot = () => ({
    time: now(), policyVersion: policyVersion(settings), paused: settings.paused,
    triggers: [...knownTriggers].map(id => ({ value: repo.triggers.get(id), disposition: repo.triggers.disposition(id) })),
    threads: repo.threads.list(), decisions: repo.threads.list().flatMap(t => repo.decisions.byThread(t.key)),
    tickets: repo.tickets.list().map(t => ({ value: t, stage: ticketStage(t) })),
    outbox: copy(outbox), withdrawn: copy(withdrawn), calls: copy(calls), events: copy(events), candidates: copy(requests),
  });
  const steps = [];
  for (const [index, command] of scenario.commands.entries()) {
    let result;
    try {
      switch (command.op) {
        case 'receive': {
          const supplied = command.trigger;
          const source = supplied.source ?? 'github';
          const t = { id: supplied.id, source, threads: [{ source, id: 'issue:1' }],
            actor: { source, id: 'alice' }, human: true, addressed: 'mention', at: now(), text: 'Fix typo',
            channel: { source, thread: 'issue:1', address: { message: supplied.id } }, meta: {}, ...supplied };
          // The fixture DSL uses null for an absent optional field; never pass it into the domain.
          if (t.addressed === null) delete t.addressed;
          result = engine.receive(t); knownTriggers.add(t.id); break;
        }
        case 'evaluate_due': result = engine.evaluateDue(); break;
        case 'settle': result = settle(); break;
        case 'advance': time += command.milliseconds; result = now(); break;
        case 'delivered': result = engine.candidateDelivered(engine.candidate(command.trigger).evalId); break;
        case 'decide': {
          const candidate = engine.candidate(command.trigger);
          const outcome = command.assessment ? assessmentOutcome(command.assessment, settings, candidate.strength) : {
            ...command.outcome, ...(command.outcome.ticketId ? { ticketId: resolve(command.outcome.ticketId) } : {}) };
          result = engine.decide(command.trigger, { ...outcome, ...(candidate ? { evalId: candidate.evalId } : {}) });
          if (command.as) aliases[command.as] = result.ticketId ?? result.id;
          break;
        }
        case 'set_settings': {
          const old = settings, previous = policyVersion(old);
          settings = SettingsSchema.parse(merge(settings, command.patch));
          const current = policyVersion(settings);
          if (previous !== current) engine.policyChanged(previous, current, policyEffects(repo.tickets.open(), old, settings, scopeFor));
          result = { previous, current }; break;
        }
        case 'report': result = engine.report(ticket(command.ticket).id, command.report); break;
        case 'stop': result = engine.stopTicket(ticket(command.ticket).id, command.reason); break;
        case 'retry': result = engine.retryTicket(ticket(command.ticket).id); break;
        case 'attach': result = engine.attach(ticket(command.ticket).id, command.trigger, command.note); break;
        case 'reevaluate': result = engine.reevaluate(command.trigger); break;
        case 'unsubscribe': result = engine.unsubscribe(command.thread); break;
        case 'request_status': result = engine.requestStatus(ticket(command.ticket).id); break;
        case 'note': result = engine.note(ticket(command.ticket).id, command.text); break;
        case 'unrelated': result = engine.markUnrelated(ticket(command.ticket).id, command.thread, command.reason); break;
        case 'reply': result = engine.reply({ ...command.request, ticketId: resolve(command.request.ticketId) }); break;
        case 'interrupted': result = engine.assignmentInterrupted(ticket(command.ticket).id, assignment(command.ticket).id); break;
        case 'delivery_failed': result = engine.deliveryFailed(ticket(command.ticket).id, assignment(command.ticket).id, command.error); break;
        case 'handback': result = engine.handback(ticket(command.ticket).id, command.trigger, command.reason); break;
        case 'bind': result = engine.bindThread(ticket(command.ticket).id, command.thread, command.own); break;
        case 'close': result = engine.threadClosed(command.thread, command.outcome, command.at ?? now()); break;
        case 'context': result = engine.followForContext(ticket(command.ticket).id, command.thread); break;
        case 'read': result = engine.inputsRead(ticket(command.ticket).id, ticket(command.ticket).inputs.filter(i => i.kind === 'trigger').map(i => i.id)); break;
        case 'turn_end': result = engine.turnEnded(ticket(command.ticket).id, assignment(command.ticket).id); break;
        default: throw new Error('Unknown scenario command: ' + command.op);
      }
      if (command.expect_error) throw new Error('Expected command error was not raised');
      steps.push({ index, op: command.op, result: copy(result), state: snapshot() });
    } catch (error) {
      if (error.message === 'Expected command error was not raised') throw error;
      const caught = error instanceof UserError ? { type: 'UserError', code: error.code, values: error.values, missing: error.missing, message: error.message }
        : { type: error.name, message: error.message };
      if (!command.expect_error || ![caught.code, caught.message].includes(command.expect_error)) throw error;
      steps.push({ index, op: command.op, error: caught, state: snapshot() });
    }
  }
  return { schema_version: 1, scenario_id: scenario.id, reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e',
    scope: 'original_core_and_policy_with_scripted_source_and_executor_ports', steps, final: snapshot() };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (!process.argv[2]) throw new Error('Usage: driver.mjs <scenario.json>');
  const scenario = JSON.parse(await readFile(process.argv[2], 'utf8'));
  process.stdout.write(JSON.stringify(runScenario(scenario)) + '\n');
}
