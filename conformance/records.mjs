import { spawnSync } from 'node:child_process';
import { writeFile } from 'node:fs/promises';
import { followupOwners, reached, liveTicket, ticketOutcome, settled, UserError, USER_ERRORS, reasonText, REASONS } from './reference/upstream/src/core/index.ts';
import { automatic } from './reference/upstream/src/core/tickets.ts';
import { firstDifference } from './compare.mjs';
const binary = process.argv[2];
if (!binary) throw new Error('Pass an already-built remote superagent-primitives path');
const clean = value => JSON.parse(JSON.stringify(value));
const cases = [];
const add = (id, request, expected) => cases.push({ id, request: clean(request), expected: clean(expected) });
const baseTicket = { id: 'ticket', title: 'Fix', decisionId: 'd', mandate: { strength: 'explicit', policyVersion: 'v', basis: { via: 'listed' }, actor: { source: 'github', id: 'alice' }, scope: 'Typo' },
  channels: [], assignments: [], inputs: [], undelivered: [], createdAt: '', updatedAt: '', origin: { triggerId: 'start', triggerIds: ['start'] } };
for (const human of [true, false]) for (const end of [undefined, 'executor', 'human']) for (const inputKind of ['none', 'pending', 'delivered', 'context'])
  for (const contextDisposition of [false, true]) for (const handback of [false, true]) for (const evaluation of [false, true])
    for (const requeued of [false, true]) for (const contextThread of [false, true]) for (const subscription of [false, true]) for (const origin of [false, true]) {
      const trigger = { id: origin ? 'start' : 'followup', source: 'github', threads: [{ source: 'github', id: 'issue:1' }], actor: { source: 'github', id: 'alice' }, human, at: '', text: 'Follow-up', meta: {} };
      const ticket = { ...baseTicket, ...(end ? { ended: { at: '', by: end, outcome: end === 'human' ? 'stopped' : 'completed' } } : {}),
        inputs: inputKind === 'none' ? [] : [{ id: 'i', kind: 'trigger', triggerId: trigger.id, at: '', ...(human ? { human: true } : {}), ...(inputKind === 'context' ? { context: true } : {}) }],
        undelivered: ['pending', 'context'].includes(inputKind) ? ['i'] : [] };
      const threads = [{ key: 'github:issue:1', ref: { source: 'github', id: 'issue:1' }, createdAt: '', subscriptions: subscription ? ['ticket'] : [], context: contextThread ? ['ticket'] : [], correlated: [], pending: [], meta: {},
        ...(evaluation ? { evaluating: { id: 'eval', threadKey: 'github:issue:1', triggerIds: [trigger.id], primary: trigger.id, strength: 'explicit', actor: trigger.actor, basis: { via: 'followup' }, policyVersion: 'v', startedAt: '', deadline: '' } } : {}) }];
      const disposition = { fanout: ['ticket'], at: '', ...(contextDisposition ? { context: ['ticket'] } : {}), ...(handback ? { handbacks: [{ ticketId: 'ticket', reason: 'Beyond scope', at: '' }] } : {}), ...(requeued ? { requeued: true } : {}) };
      const tickets = [ticket], records = { tickets: { get: id => tickets.find(ticket => ticket.id === id) }, threads: { get: key => threads.find(thread => thread.key === key) }, triggers: { disposition: () => disposition } };
      const thread = key => threads.find(thread => thread.key === key);
      add('ownership/' + cases.length, { op: 'records', data: { ticket, trigger, threads, tickets, disposition } }, {
        reached: reached(ticket, trigger, thread), reachedWithoutContext: reached(ticket, trigger, thread, false),
        owners: followupOwners(trigger, records), live: liveTicket(ticket), outcome: ticketOutcome(ticket) ?? null,
        automatic: ticket.inputs.map(automatic), settled: null, threads, decision: null, disposition,
      });
    }
const trigger = { id: 'followup', source: 'github', threads: [{ source: 'github', id: 'issue:1' }], actor: { source: 'github', id: 'alice' }, human: true, at: '', text: 'x', meta: {} };
for (const kind of ['ignore', 'accept', 'reopen', 'stop', 'unavailable']) for (const code of ['said', 'stopNotExplicit', 'stopUnknown', 'routed', 'unknown']) {
  const decision = { id: 'd', evalId: 'e', threadKey: 'github:issue:1', triggerIds: ['followup'], policyVersion: 'v', at: '', kind, reason: [{ code, values: { text: 'fixture' } }], strength: 'explicit' };
  const ticket = { ...baseTicket, inputs: [{ id: 'note', kind: 'note', text: 'Context', at: '' }, { id: 'text', kind: 'text', text: 'Request', at: '' }, { id: 'policy', kind: 'policy-changed', from: 'v', to: 'v2', at: '' }] };
  add('settled/' + cases.length, { op: 'records', data: { ticket, trigger, threads: [], tickets: [ticket], decision } }, {
    reached: false, reachedWithoutContext: false, owners: [], live: true, outcome: null, automatic: ticket.inputs.map(automatic), settled: settled(decision), threads: [], decision, disposition: null,
  });
}
for (const code of Object.keys(USER_ERRORS)) for (const values of [{}, { ticket: 't', tickets: 't, other', eval: 'e', block: 'policy', revision: '42', part: 'runtime/shared', source: 'github', error: 'fixture', limit: '100', placeholders: '{ticket}' }, { part: '中文😀', error: '$& {literal}' }]) {
  const error = new UserError(code, values);
  add('error/' + cases.length, { op: 'error', code, values }, { code, values, missing: error.missing, message: error.message });
}
for (const code of [...Object.keys(REASONS), 'unknown', '{a{b}}', '{中文}{word}', '{{text}', 'unknown {missing}']) for (const values of [undefined, { text: '中文😀 $& {literal}', ticket: 't', trigger: 'g', b: 'inner', word: 'word' }]) {
  const reasons = [{ code, ...(values ? { values } : {}) }, { code: 'said', triggerId: 'trigger', values: { text: 'Second' } }];
  add('reason/' + cases.length, { op: 'reason_text', reasons }, { text: reasonText(reasons) });
}
const run = spawnSync(binary, [], { input: cases.map(item => JSON.stringify(item.request)).join('\n') + '\n', encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
if (run.error) throw run.error;
if (run.status !== 0) throw new Error(`Target records adapter failed (${run.status}): ${run.stderr}`);
const lines = run.stdout.trim().split('\n');
if (lines.length !== cases.length) throw new Error('Missing or duplicated target records responses');
const differences = [];
for (const [index, item] of cases.entries()) { const actual = JSON.parse(lines[index]), difference = firstDifference(item.expected, actual); if (difference) differences.push({ id: item.id, difference, request: item.request, expected: item.expected, actual }); }
const result = { schema_version: 1, reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e',
  scope: 'model ownership/reachability/settlement, automatic-input classification and public errors/reasons; no Engine or runtime parity claim', cases: cases.length, passed: cases.length - differences.length, differences };
if (process.argv[3]) await writeFile(process.argv[3], JSON.stringify(result, null, 2) + '\n');
process.stdout.write(JSON.stringify({ ...result, differences: differences.slice(0, 3) }) + '\n');
if (differences.length) process.exitCode = 1;
