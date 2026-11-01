// Product subset metadata check, not a product-acceptance or SDK execution test.
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';

const selected = {
  main: ['orchestrator_read', 'orchestrator_route', 'orchestrator_ticket_retry', 'orchestrator_ticket_note', 'orchestrator_ticket_request_status', 'orchestrator_trigger_requeue', 'orchestrator_reply', 'orchestrator_thread_unsubscribe', 'orchestrator_settings_save', 'orchestrator_prompts_save', 'orchestrator_eval', 'orchestrator_eval_read', 'orchestrator_eval_decide'],
  worker: ['orchestrator_read', 'orchestrator_ack', 'orchestrator_thread_unrelated', 'orchestrator_thread_follow', 'orchestrator_trigger_handback', 'orchestrator_report'],
};
const reference = JSON.parse(await readFile(new URL('../reference/runtime-contracts.json', import.meta.url), 'utf8'));
const target = new URL('../../crates/xharness-superagent/assets/business-tools.json', import.meta.url);
let unions = 0;
let inputDefaults = 0;
let jsonDomains = 0;
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
    if (name === 'orchestrator_settings_save') {
      // z.unknown exports an unconstrained JSON object with no type. The
      // existing Host requires an explicit type: enumerate exactly the JSON
      // domain, not just object, while the block handler validates its value.
      const value = definition.parameters.properties.value;
      assert.deepEqual(Object.keys(value), ['description']);
      value.type = ['object', 'array', 'string', 'number', 'boolean', 'null'];
      jsonDomains += 1;
    }
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
    if (name === 'orchestrator_eval_read') {
      assert.deepEqual(definition.parameters.required, ['trigger_id', 'resource', 'offset']);
      assert.equal(definition.parameters.properties.offset.default, 0);
      assert.equal(definition.parameters.properties.offset.type, 'integer');
      assert.equal(definition.parameters.properties.offset.minimum, 0);
      assert.equal(definition.parameters.properties.offset.maximum, Number.MAX_SAFE_INTEGER);
      definition.parameters.required = ['trigger_id', 'resource'];
      inputDefaults += 1;
    }
    if (name === 'orchestrator_prompts_save') {
      // Same explicit Zod input/output distinction as read.offset: missing
      // preview is parsed as false, while baseRevision remains handler-checked.
      assert.deepEqual(definition.parameters.required, ['config', 'preview']);
      assert.equal(definition.parameters.properties.preview.type, 'boolean');
      assert.equal(definition.parameters.properties.preview.default, false);
      definition.parameters.required = ['config'];
      inputDefaults += 1;
    }
    return definition;
  });
}
if (process.argv.includes('--write')) await writeFile(target, JSON.stringify(expected, null, 2) + '\n');
assert.deepEqual(JSON.parse(await readFile(target, 'utf8')), expected);
assert.equal(unions, 10);
assert.equal(inputDefaults, 4);
assert.equal(jsonDomains, 1);
console.log('19 product-subset definitions verified: 10 primitive/null unions, 4 explicit Zod input defaults, and 1 unconstrained JSON domain projected; all other pinned metadata retained.');
