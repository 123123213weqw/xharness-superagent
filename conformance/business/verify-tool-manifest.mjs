// Product subset metadata check, not a product-acceptance or SDK execution test.
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';

const selected = {
  main: ['orchestrator_read', 'orchestrator_route', 'orchestrator_ticket_retry', 'orchestrator_ticket_request_status', 'orchestrator_trigger_requeue', 'orchestrator_reply', 'orchestrator_thread_unsubscribe'],
  worker: ['orchestrator_read', 'orchestrator_ack', 'orchestrator_thread_unrelated', 'orchestrator_thread_follow', 'orchestrator_trigger_handback', 'orchestrator_report'],
};
const reference = JSON.parse(await readFile(new URL('../reference/runtime-contracts.json', import.meta.url), 'utf8'));
const target = new URL('../../crates/xharness-superagent/assets/business-tools.json', import.meta.url);
let unions = 0;
let inputDefaults = 0;
function portable(value) {
  if (Array.isArray(value)) return value.map(portable);
  if (!value || typeof value !== 'object') return value;
  const result = Object.fromEntries(Object.entries(value).map(([k, v]) => [k, portable(v)]));
  if (!result.anyOf) return result;
  // ONLY the pinned disjoint primitive/null unions are flattened. Constraints
  // remain type-specific under JSON Schema, so null does not acquire a string
  // length or integer range restriction. No general anyOf rewrite or dropping.
  const branches = result.anyOf;
  assert.deepEqual(branches.map(b => b.type).slice(1), ['null']);
  const kind = branches[0].type;
  const allowed = kind === 'string' ? ['type', 'minLength', 'maxLength']
    : kind === 'integer' ? ['type', 'exclusiveMinimum', 'maximum'] : [];
  assert.ok(allowed.length > 0);
  assert.deepEqual(Object.keys(branches[1]), ['type']);
  for (const [key, field] of Object.entries(branches[0])) {
    assert.ok(allowed.includes(key), `Unsupported union constraint: ${key}`);
    if (key !== 'type') {
      assert.equal(Object.hasOwn(result, key), false);
      result[key] = field;
    }
  }
  assert.equal(Object.hasOwn(result, 'type'), false);
  result.type = [kind, 'null'];
  delete result.anyOf;
  unions += 1;
  return result;
}
const expected = {};
for (const [role, names] of Object.entries(selected)) {
  expected[role] = names.map(name => {
    const matches = reference.roles[role].filter(tool => tool.name === name);
    assert.equal(matches.length, 1, name);
    const definition = portable(matches[0]);
    if (name === 'orchestrator_read') {
      // Pinned sessions.ts calls schema.parse(args): Zod supplies offset=0.
      // Its default toJSONSchema output describes parsed OUTPUT as required.
      // XHarness validates INPUT before the handler, so advertise the actual
      // accepted input here; Request::parse applies this one explicit default.
      // No generic kernel defaulting, other optionalization or constraint loss.
      assert.deepEqual(definition.parameters.required, ['resource', 'offset']);
      assert.equal(definition.parameters.properties.offset.default, 0);
      assert.equal(definition.parameters.properties.offset.type, 'integer');
      assert.equal(definition.parameters.properties.offset.minimum, 0);
      assert.equal(definition.parameters.properties.offset.maximum, Number.MAX_SAFE_INTEGER);
      definition.parameters.required = ['resource'];
      inputDefaults += 1;
    }
    return definition;
  });
}
if (process.argv.includes('--write')) await writeFile(target, JSON.stringify(expected, null, 2) + '\n');
assert.deepEqual(JSON.parse(await readFile(target, 'utf8')), expected);
assert.equal(unions, 8);
assert.equal(inputDefaults, 2);
console.log('13 product-subset definitions verified: 8 primitive/null unions and 2 explicit Zod offset-input defaults projected; all other pinned metadata retained.');
