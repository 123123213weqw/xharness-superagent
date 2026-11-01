import { spawnSync } from 'node:child_process';
import { writeFile } from 'node:fs/promises';
import { SettingsSchema, settingsPolicy, policyVersion, policyEffects, assessmentOutcome, directionAllowed, allowedSender, anyoneAllowed } from './reference/upstream/src/policy.ts';
import { firstDifference } from './compare.mjs';

const binary = process.argv[2];
if (!binary) throw new Error('Pass a remote-built superagent-policy binary; this runner does not compile Rust.');
const clean = value => JSON.parse(JSON.stringify(value));
const cases = [];
const groups = {};
function add(group, input) { cases.push({ id: group + '/' + (groups[group] ?? 0), input }); groups[group] = (groups[group] ?? 0) + 1; }
const listened = { listened: true, observed: true, row: 'comments' };
const filtered = { listened: false, reason: 'fixture_scope_filtered' };
const scopePort = (spec, label, calls) => ({ scope: ({ source, meta }) => { calls.push({ label, source, meta }); return spec ?? listened; } });
function reference(input) {
  if (input.op === 'json') return { encoded: JSON.stringify(input.raw === undefined ? input.value : JSON.parse(input.raw)) };
  if (input.op === 'direction') return { allowed: directionAllowed(input.value) };
  if (input.op === 'effects') {
    const old = SettingsSchema.parse(input.old), now = SettingsSchema.parse(input.new), calls = [];
    const oldScope = scopePort(input.oldScope, 'old', calls), newScope = scopePort(input.newScope, 'new', calls);
    return { effects: [...policyEffects(input.tickets, old, now, settings => settings === old ? oldScope : newScope)], calls };
  }
  const parsed = SettingsSchema.safeParse(input.settings);
  if (!parsed.success) return { ok: false, issues: parsed.error.issues.map(({ code, path, message }) => ({ code, path, message })) };
  const settings = parsed.data;
  if (input.op === 'settings') return { ok: true, settings, encoded: JSON.stringify(settings), version: policyVersion(settings) };
  if (input.op === 'assessment') {
    try { return { outcome: assessmentOutcome(input.assessment, settings, input.strength) }; }
    catch (error) { return { error: error.message }; }
  }
  const calls = [], policy = settingsPolicy(settings, scopePort(input.scope, 'scope', calls));
  switch (input.op) {
    case 'gate': return { gate: policy.gate(input.trigger, input.thread, input.tickets ?? []), version: policy.version, calls };
    case 'revalidate': return { valid: policy.revalidate(input.mandate), version: policy.version, calls };
    case 'admit': return { admitted: policy.admitsInferred(input.rules), version: policy.version };
    case 'allowed': return { allowed: allowedSender(settings, input.actor) ?? null, anyone: anyoneAllowed(settings) };
    default: throw new Error('Unknown policy operation');
  }
}

// Identity-sensitive JSON: insertion order, numeric property names, duplicate names,
// binary64 parsing/rounding, scientific-notation thresholds, escaping and negative zero.
for (const raw of ['{}', '[]', '-0', '1e-7', '1e-6', '1e20', '1e21', '1e400', '-1e400', '1e-400',
  '9007199254740993', '18446744073709551615', '123456789012345678901234567890',
  '{"b":1,"a":2,"2":3,"10":4,"0":5,"01":6,"4294967294":7,"4294967295":8}',
  '{"a":1,"b":2,"a":3}', '{"__proto__":{"x":1},"constructor":2}',
  '{"中文":"😀","escape":"\\n\\r\\t\\b\\f\\u0000\\\"\\\\","line":"\u2028\u2029"}']) add('json-edge', { op: 'json', raw });
let seed = 0x4d595df4d0f33173n;
const bytes = new ArrayBuffer(8), view = new DataView(bytes);
for (let index = 0; index < 10000; index++) {
  seed ^= seed << 13n; seed ^= seed >> 7n; seed ^= seed << 17n; seed = BigInt.asUintN(64, seed);
  view.setBigUint64(0, seed); const number = view.getFloat64(0);
  if (Number.isFinite(number)) add('json-number', { op: 'json', raw: JSON.stringify(number) });
}

const defaultRules = SettingsSchema.parse({}).policy.rules;
const merge = (base, patch) => Object.fromEntries([...new Set([...Object.keys(base), ...Object.keys(patch)])].map(key => {
  const value = patch[key];
  return [key, value === undefined ? base[key] : value && typeof value === 'object' && !Array.isArray(value)
    && base[key] && typeof base[key] === 'object' && !Array.isArray(base[key]) ? merge(base[key], value) : value];
}));
const settingsInputs = [{}, null, [], true, 1, 'settings', { paused: false }, { paused: 'yes' }, { extra: true },
  { executor: { maxWorkers: 1.2 } }, { executor: { maxWorkers: 1e20 } },
  { policy: { rules: [...defaultRules, defaultRules[0]] } },
  { policy: { sources: { github: { bots: [{ id: 'helper' }], extra: { b: 1, a: 2, 2: 'two', 1: 'one' } } } } },
  JSON.parse('{"policy":{"sources":{"__proto__":{"extra":1},"github":{"__proto__":2,"constructor":3,"extra":{"__proto__":4}}}}}'),
  { executor: { model: { model: ' m ', provider: '\ufeff p\ufeff', reasoningEffort: ' high ' }, directory: ' /tmp/work ' } },
  { policy: { rules: [{ id: 'copy', name: '😀'.repeat(51), enabled: true, instruction: ' x ' }] } },
  { policy: { rules: [{ id: 'copy', name: '\u0085x\u0085', enabled: true, instruction: '\ufeff x\ufeff' }] } },
  { paused: 1, sources: { 'Bad_key': { enabled: 'yes' } }, policy: { rules: null }, eval: null, executor: null, 'extra\n"': 1 },
];
for (const [section, key, values] of [
  ['eval', 'mode', ['off', 'observe', 'auto', '', null, 1, 'AUTO']],
  ['eval', 'timeoutSeconds', [29, 30, 600, 601, null, '120', 120.5, -1e20]],
  ['executor', 'maxWorkers', [0, 1, 16, 17, null, '3', 3.1]],
  ['executor', 'directory', ['', '/tmp', 'relative', ' /tmp ', '\ufeff/tmp\ufeff', null]],
  ['executor', 'prepare', ['', 'x'.repeat(4000), 'x'.repeat(4001), '😀'.repeat(2001), null]],
  ['executor', 'blockedCommands', [[], ['curl', 'curl'], ['gh', 'git', 'lark-cli'], ['..', '.', 'a b', '', 'a/b'], null, [42]]],
  ['policy', 'rules', [[], [{ id: 'bad space', name: '', enabled: 'yes', instruction: '' }], [{ id: 'x', name: 'x', enabled: true, instruction: 'x', unexpected: 1 }]]],
]) for (const value of values) settingsInputs.push({ [section]: { [key]: value } });
for (const value of [null, [], 1, 'yes', { github: null }, { github: { enabled: 'yes', config: null, extra: 1 } },
  { github: { config: { arbitrary: [1, 'x', null, { nested: true }] } } }]) settingsInputs.push({ sources: value });
for (const id of ['', 'Upper', 'a_b', 'github', 'a-1', 'constructor', 'toString', '__proto__']) {
  settingsInputs.push({ policy: { sources: JSON.parse(JSON.stringify({ [id]: { allowed: [{ id: ' Alice ', name: ' Alice ' }], admission: { comments: { unlistedAddressed: 'explicit' } }, bots: [] } })) } });
}
for (const value of [null, [], 1, { allowed: null }, { allowed: [{ id: '', name: 1, extra: 1 }] },
  { admission: { comments: { listedUnaddressed: 'explicit', unlistedAddressed: 'bad', unlistedUnaddressed: true, extra: 1 } } },
  { admission: { comments: null } }, { admission: [] }, { extra: 2, allowed: [], admission: {}, bots: [] }]) settingsInputs.push({ policy: { sources: { github: value } } });
for (const settings of settingsInputs) add('settings', { op: 'settings', settings });

const trigger = (actor = 'alice', human = true, addressed) => ({ id: 't', source: 'github', threads: [{ source: 'github', id: 'issue:1' }],
  actor: { source: 'github', id: actor }, human, ...(addressed ? { addressed } : {}), at: '2026-10-31T00:00:00.000Z', text: 'Fix typo', meta: { row: 'comments' } });
const thread = { key: 'github:issue:1', ref: { source: 'github', id: 'issue:1' }, createdAt: '', subscriptions: [], correlated: [], pending: [], meta: {} };
const mandate = { strength: 'explicit', policyVersion: 'old-version', basis: { via: 'addressed', source: 'github', scope: { row: 'comments' }, addressed: 'mention' }, actor: { source: 'github', id: 'alice' }, scope: 'One typo' };
const ticket = { id: 'ticket', title: 'Fix', decisionId: 'd', mandate, channels: [], assignments: [], inputs: [], undelivered: [], createdAt: '', updatedAt: '', origin: { triggerId: 't', triggerIds: ['t'] } };
const contexts = [{ thread, tickets: [] }, { thread: { ...thread, subscriptions: ['ticket'] }, tickets: [] },
  { thread: { ...thread, subscriptions: ['ticket'], context: ['ticket'] }, tickets: [ticket] },
  { thread, tickets: [ticket] }, { thread, tickets: [{ ...ticket, ended: { at: '', by: 'executor', outcome: 'completed' } }] }];
const admissions = [{}];
for (const listedUnaddressed of ['inferred', 'ignore']) for (const unlistedAddressed of ['explicit', 'inferred', 'ignore'])
  for (const unlistedUnaddressed of ['inferred', 'ignore']) admissions.push({ listedUnaddressed, unlistedAddressed, unlistedUnaddressed });
for (const mode of ['off', 'observe', 'auto']) for (const human of [true, false]) for (const actor of ['alice', 'ALICE', 'guest', 'helper', ''])
  for (const addressed of [undefined, 'mention', 'direct']) for (const scope of [filtered, listened, { listened: true, observed: false }, { listened: true, observed: false, row: 'comments' }])
    for (const context of contexts) for (const row of admissions) {
      const settings = { eval: { mode }, policy: { sources: { github: { allowed: [{ id: 'Alice', name: 'Operator' }], bots: [{ id: 'helper', name: 'Helper' }], admission: { comments: row } } } } };
      add('gate', { op: 'gate', settings, trigger: trigger(actor, human, addressed), scope, ...context });
    }
for (const bots of [[], null, [{ id: ' helper ', name: ' H ' }], [{ id: 'helper' }, 42], [{ id: 'helper', extra: 1 }], 'helper'])
  add('bot-list', { op: 'gate', settings: { policy: { sources: { github: { bots } } } }, scope: listened, thread, trigger: trigger('helper', false, 'mention') });

for (const mode of ['off', 'observe', 'auto']) for (const via of ['addressed', 'listed', 'followup', 'admission', 'bot'])
  for (const strength of ['explicit', 'inferred']) for (const listed of [true, false]) for (const observation of [undefined, {}])
    for (const rules of [undefined, [], ['copy'], ['visual'], ['unknown']]) for (const cell of ['listedUnaddressed', 'unlistedAddressed', 'unlistedUnaddressed'])
      for (const scope of [filtered, listened, { listened: true, observed: false }]) {
        const settings = { eval: { mode }, policy: { rules: defaultRules.map(rule => ({ ...rule, enabled: rule.id === 'copy' })), sources: { github: { allowed: listed ? [{ id: 'ALICE' }] : [], bots: listed ? [{ id: 'alice' }] : [], admission: { comments: { unlistedAddressed: 'explicit', unlistedUnaddressed: 'inferred' } } } } } };
        const basis = { via, source: 'github', scope: { row: 'comments' }, cell, ...(observation ? { observation } : {}), ...(rules ? { rules } : {}) };
        add('revalidate', { op: 'revalidate', settings, scope, mandate: { ...mandate, strength, basis } });
      }
for (const mode of ['off', 'observe', 'auto']) for (const rules of [[], ['copy'], ['copy', 'visual'], ['copy', 'unknown'], ['copy', 'copy']]) add('admit', { op: 'admit', settings: { eval: { mode } }, rules });
for (const actor of [{ source: 'github', id: 'alice' }, { source: 'github', id: 'ALICE' }, { source: 'feishu', id: 'alice' }, { source: 'github', id: 'guest' }])
  for (const settings of [{}, { policy: { sources: { github: { allowed: [{ id: ' Alice ', name: ' ' }] } } } }]) add('allowed', { op: 'allowed', settings, actor });

const direction = { implementation_requested: true, target_basis: 'specified', requires_design_decision: false, evidence: 'Correct teh to the' };
const directions = [direction, null, {}, { ...direction, implementation_requested: false }, { ...direction, target_basis: 'undetermined' },
  { ...direction, target_basis: 'existing-standard' }, { ...direction, requires_design_decision: true }, { ...direction, extra: true },
  { ...direction, evidence: ' ' }, { ...direction, evidence: '😀'.repeat(351) }, { ...direction, evidence: '\ufeff okay \ufeff' }, { ...direction, implementation_requested: 'true' }];
for (const value of directions) add('direction', { op: 'direction', value });
for (const mode of ['off', 'observe', 'auto']) for (const strength of ['explicit', 'inferred']) for (const decision of ['accept', 'ignore', 'unavailable'])
  for (const direction of directions) for (const matches of [[true, false], [false, true], [true, true], [false, false]]) for (const evidence of [undefined, '', 'Please correct this typo']) {
    const assessment = { decision, direction, rule_assessments: ['copy', 'visual'].map((rule_id, index) => ({ rule_id, matched: matches[index], reason: 'fixture judgement' })),
      title: 'Fix typo', scope: 'One typo', reason: 'settled correction', acknowledgement: 'I will fix it.', worktree_name: 'fix-typo', ...(evidence === undefined ? {} : { intent_evidence: evidence }) };
    add('assessment', { op: 'assessment', settings: { eval: { mode } }, strength, assessment });
  }
for (const ids of [[], ['copy'], ['copy', 'copy'], ['copy', 'unknown'], ['copy', 'visual', 'extra']]) add('assessment-invalid-rules', {
  op: 'assessment', settings: {}, strength: 'inferred', assessment: { decision: 'accept', direction, title: 'Fix', scope: 'Typo', reason: 'Fix', intent_evidence: 'Fix', rule_assessments: ids.map(rule_id => ({ rule_id, reason: 'r', matched: true })) },
});

const old = { eval: { mode: 'auto' }, policy: { sources: { github: { allowed: [{ id: 'alice' }], bots: [{ id: 'helper' }], admission: { comments: { unlistedAddressed: 'explicit', unlistedUnaddressed: 'inferred' } } } } } };
const changes = [{}, { eval: { mode: 'observe' } }, { policy: { sources: { github: { allowed: [] } } } },
  { policy: { rules: defaultRules.map(rule => ({ ...rule, enabled: false })) } }, { policy: { rules: defaultRules.map(rule => ({ ...rule, instruction: rule.instruction + ' Changed.' })) } },
  { eval: { mode: 'off' }, policy: { sources: { github: { allowed: [] } } } }, { executor: { maxWorkers: 8 } }, { paused: true }];
for (const change of changes) for (const via of ['addressed', 'admission', 'listed', 'followup', 'bot']) for (const strength of ['explicit', 'inferred'])
  for (const newScope of [listened, filtered]) {
    const tickets = [ticket, { ...ticket, id: 'second', mandate: { ...mandate, strength, basis: { via, source: 'github', scope: {}, cell: 'listedUnaddressed', observation: { mode: 'auto' }, rules: ['copy'] } } }];
    add('effects', { op: 'effects', old, new: merge(old, change), oldScope: listened, newScope, tickets });
  }

// Resolve expected values before invoking the target; a broken reference fixture is not a passing target case.
for (const item of cases) item.expected = clean(reference(clean(item.input)));
const run = spawnSync(binary, [], { input: cases.map(item => JSON.stringify(item.input)).join('\n') + '\n', encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 });
if (run.error) throw run.error;
if (run.status !== 0) throw new Error(`Rust policy adapter failed (${run.status}): ${run.stderr}`);
const lines = run.stdout.trim().split('\n');
if (lines.length !== cases.length) throw new Error(`Response count differs: ${lines.length} vs ${cases.length}`);
const differences = [], resultGroups = Object.fromEntries(Object.entries(groups).map(([group, total]) => [group, { total, passed: 0, failed: 0 }]));
for (const [index, item] of cases.entries()) {
  const difference = firstDifference(item.expected, JSON.parse(lines[index]));
  const group = resultGroups[item.id.split('/')[0]];
  if (difference) { group.failed++; differences.push({ id: item.id, difference, input: item.input, expected: item.expected, actual: JSON.parse(lines[index]) }); }
  else group.passed++;
}
const report = { schema_version: 1, reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e',
  scope: 'original settings validation/normalization, JSON identity, policy admission/revalidation/effects and assessment conversion with recording SourceScope ports; not the full Engine or real Source',
  cases: cases.length, passed: cases.length - differences.length, groups: resultGroups, differences };
if (process.argv[3]) await writeFile(process.argv[3], JSON.stringify(report, null, 2) + '\n');
process.stdout.write(JSON.stringify({ ...report, differences: differences.slice(0, 5) }) + '\n');
if (differences.length) process.exitCode = 1;
