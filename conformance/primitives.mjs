import { spawnSync } from 'node:child_process';
import { writeFile } from 'node:fs/promises';
import { canRetry, key, latestAssignment, latestReport, readUntil, runningAssignment, ticketStage, waitsForPerson } from './reference/upstream/src/core/index.ts';
import { firstDifference } from './compare.mjs';

const binary = process.argv[2];
if (!binary) throw new Error('Pass the already-built remote target/debug/superagent-primitives path; this runner never builds Rust.');
const cases = [];
const add = (id, request, expected) => cases.push({ id, request, expected });
for (const values of [[], [''], ['a'], ['a', 'b'], ['a,b'], ['["a"]'], ['中文', '😀'], ['\n', '\0', '\t', '"', '\\'],
  ['é'], ['e\u0301'], ['\u2028', '\u2029'], ['source:thread', '2026-10-31T00:00:00.000Z']]) {
  add('key/' + cases.length, { op: 'key', values }, { key: key(...values) });
}
const input = (overrides = {}) => ({ id: 'input', kind: 'trigger', triggerId: 't2', at: '2026-10-31T00:00:01.000Z', human: true, ...overrides });
const inputs = [[], [input()], [input({ context: true })], [input({ human: undefined })],
  [input({ at: '2026-10-31T00:00:00.000Z' })], [input({ at: '2026-10-30T23:59:59.000Z' })],
  [input({ readAt: null })], [input({ readAt: '2026-10-31T00:00:02.000Z', deliveredTo: 'a', deliveredAt: '2026-10-31T00:00:01.000Z' })],
  [{ id: 'policy', kind: 'policy-changed', from: 'v1', to: 'v2', at: '', effect: { proactive: false, requesterAllowed: false } }],
  [{ id: 'n', kind: 'note', text: 'Context only', at: '' }, { id: 'text', kind: 'text', text: 'text', at: '' },
    { id: 'r', kind: 'resumed', triggerId: 't2', at: '' }, { id: 'old', kind: 'reopen', decisionId: 'd', at: '' }]];
for (const state of ['none', 'pending', 'running', 'ended']) {
  for (const report of [null, 'progress', 'waiting', 'ready', 'blocked', 'failed', 'dropped']) {
    for (const flags of [0, 1, 2, 3, 4, 5, 6, 7]) {
      for (const messages of inputs) {
        const ticket = {
          id: 'ticket', title: 'Fix', decisionId: 'd',
          mandate: { strength: 'explicit', policyVersion: 'v', basis: { via: 'listed' }, actor: { source: 'github', id: 'alice' }, scope: 'One typo' },
          channels: [], assignments: state === 'none' ? [] : [{ id: 'a', state, createdAt: '',
            ...(report ? { report: { state: report, summary: '', at: '2026-10-31T00:00:00.000Z' } } : {}) }],
          inputs: messages, undelivered: [], createdAt: '', updatedAt: '', origin: { triggerId: 't1', triggerIds: ['t1'] },
          ...(flags & 1 ? { delivery: { error: 'offline', at: '' } } : {}),
          ...(flags & 2 ? { attention: { id: 'q', question: '?' } } : {}),
          ...(flags & 4 ? { ended: { at: '', by: 'executor', outcome: 'completed' } } : {}),
        };
        add('ticket/' + cases.length, { op: 'ticket', ticket }, {
          ticket, stage: ticketStage(ticket), canRetry: canRetry(ticket), waitsForPerson: waitsForPerson(ticket),
          runningAssignment: runningAssignment(ticket) ?? null, latestAssignment: latestAssignment(ticket) ?? null,
          latestReport: latestReport(ticket) ?? null, readUntil: readUntil(ticket), readUntilNonProgress: readUntil(ticket, false),
        });
      }
    }
  }
}
// Add histories in which the last report and the running/latest Assignment differ.
for (const item of cases.filter(item => item.request.op === 'ticket' && item.request.ticket.assignments.length).slice(0, 50)) {
  const ticket = structuredClone(item.request.ticket);
  ticket.assignments.unshift({ id: 'previous', state: 'ended', createdAt: '', idleAt: '2026-10-31T00:00:03.000Z',
    report: { state: 'blocked', summary: 'old', at: '2026-10-31T00:00:02.000Z' } });
  add('history/' + cases.length, { op: 'ticket', ticket }, {
    ticket, stage: ticketStage(ticket), canRetry: canRetry(ticket), waitsForPerson: waitsForPerson(ticket),
    runningAssignment: runningAssignment(ticket) ?? null, latestAssignment: latestAssignment(ticket) ?? null,
    latestReport: latestReport(ticket) ?? null, readUntil: readUntil(ticket), readUntilNonProgress: readUntil(ticket, false),
  });
}
const clean = value => JSON.parse(JSON.stringify(value));
const run = spawnSync(binary, [], { input: cases.map(item => JSON.stringify(item.request)).join('\n') + '\n', encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
if (run.error) throw run.error;
if (run.status !== 0) throw new Error(`Rust primitive adapter failed (${run.status}): ${run.stderr}`);
const lines = run.stdout.trim().split('\n');
if (lines.length !== cases.length) throw new Error('Target omitted or duplicated a response');
const differences = [];
for (const [index, item] of cases.entries()) {
  const difference = firstDifference(clean(item.expected), JSON.parse(lines[index]));
  if (difference) differences.push({ id: item.id, difference, input: clean(item.request) });
}
const report = { schema_version: 1, reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e',
  scope: 'typed model round-trip, key, stage, retry and read helpers only; no Engine or policy parity claim',
  cases: cases.length, passed: cases.length - differences.length, differences };
if (process.argv[3]) await writeFile(process.argv[3], JSON.stringify(report, null, 2) + '\n');
process.stdout.write(JSON.stringify({ ...report, differences: differences.slice(0, 3) }) + '\n');
if (differences.length) process.exitCode = 1;
