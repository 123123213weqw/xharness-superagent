import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import { runScenario } from '../reference/driver.mjs';
import { firstDifference } from '../compare.mjs';

const folder = new URL('../scenarios/', import.meta.url);
const files = (await readdir(folder)).filter(f => f.endsWith('.json')).sort();
for (const file of files) {
  test('reference self-test: ' + file, async () => {
    const input = JSON.parse(await readFile(new URL(file, folder), 'utf8'));
    const a = runScenario(input), b = runScenario(input);
    assert.equal(firstDifference(a, b), null, 'original reference must be deterministic');
    const summary = {
      triggers: a.final.triggers.length, decisions: a.final.decisions.length,
      tickets: a.final.tickets.length, outbox: a.final.outbox.length,
      ended: a.final.tickets.filter(t => t.value.ended).length,
      decisionKinds: a.final.decisions.map(d => d.kind),
    };
    for (const [key, value] of Object.entries(input.expected_summary)) assert.deepEqual(summary[key], value, key);
    // These are reference-only assertions. No Rust implementation is present or marked passing.
    assert.ok(a.scope.includes('scripted_source_and_executor_ports'));
  });
}
test('comparator detects a removed authorization field', () => {
  assert.equal(firstDifference({ mandate: { scope: 'small' } }, { mandate: {} }).path, '$.mandate.$keys');
});
test('comparator detects duplicate sends and value drift', () => {
  assert.ok(firstDifference({ sends: [1] }, { sends: [1, 1] }));
  assert.ok(firstDifference({ permission: 'read-only' }, { permission: 'full-access' }));
  assert.ok(firstDifference({ at: 100 }, { at: 101 }));
  assert.ok(firstDifference({ optional: null }, {}));
});
test('comparator reports the earliest divergent event, not lexicographic index order', () => {
  const expected = Array.from({ length: 20 }, () => ({ value: 'same' }));
  const actual = structuredClone(expected);
  actual[2].value = 'first';
  actual[10].value = 'later';
  assert.equal(firstDifference(expected, actual).path, '$.2.value');
});
