import { spawnSync } from 'node:child_process';
import { readdir, readFile, writeFile, mkdir } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { gzipSync } from 'node:zlib';
import { resolve } from 'node:path';
import { runScenario } from './reference/driver.mjs';
import { firstDifference } from './compare.mjs';

const binary = process.argv[2];
if (!binary) throw new Error('Pass the already-built remote superagent-engine binary');
const resultFile = process.argv[3], traceDirectory = resultFile ? resolve(resultFile.replace(/\.json$/, '') + '-traces') : null;
if (traceDirectory) await mkdir(traceDirectory, { recursive: true });
const clean = value => JSON.parse(JSON.stringify(value));
const sha = value => createHash('sha256').update(value).digest('hex');
const canonical = value => Array.isArray(value) ? value.map(canonical) : value !== null && typeof value === 'object'
  ? Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])])) : value;
// Only root implementation-description metadata differs. No domain scope, ID,
// timestamp, authorization, event, callback or state field is removed.
const body = ({ scope, ...trace }) => trace;
const files = (await readdir(new URL('./scenarios/', import.meta.url))).filter(file => file.endsWith('.json')).sort();
const results = [], differences = [];
for (const file of files) {
  const input = JSON.parse(await readFile(new URL('./scenarios/' + file, import.meta.url), 'utf8'));
  const reference = clean(runScenario(input));
  const run = spawnSync(binary, [], { input: JSON.stringify(input) + '\n', encoding: 'utf8', maxBuffer: 128 * 1024 * 1024 });
  if (run.error) throw run.error;
  if (run.status !== 0) throw new Error(`Rust engine ${file} failed (${run.status}): ${run.stderr}`);
  const target = JSON.parse(run.stdout);
  const difference = firstDifference(body(reference), body(target));
  if (difference) differences.push({ id: input.id, difference });
  const refBytes = JSON.stringify(reference), targetBytes = JSON.stringify(target);
  const row = { id: input.id, fixture: file, commands: input.commands.length, equal: difference === null,
    reference_body_sha256: sha(JSON.stringify(canonical(body(reference)))), target_body_sha256: sha(JSON.stringify(canonical(body(target)))) };
  if (traceDirectory) {
    const referenceName = input.id + '.reference.json.gz', targetName = input.id + '.target.json.gz';
    await writeFile(resolve(traceDirectory, referenceName), gzipSync(refBytes)); await writeFile(resolve(traceDirectory, targetName), gzipSync(targetBytes));
    row.traces = { reference: referenceName, target: targetName, reference_json_sha256: sha(refBytes), target_json_sha256: sha(targetBytes) };
  }
  results.push(row);
}
const report = { schema_version: 1, reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e',
  scope: 'transactional Rust domain commands versus original Engine/Policy with scripted ports; no SQL, Runtime, real Sources or provider parity claim',
  metadata_exception: 'root scope description only', cases: results.length, commands: results.reduce((sum, row) => sum + row.commands, 0),
  passed: results.filter(row => row.equal).length, results, differences, trace_directory: traceDirectory };
if (resultFile) await writeFile(resultFile, JSON.stringify(report, null, 2) + '\n');
process.stdout.write(JSON.stringify({ ...report, results: undefined, differences: differences.slice(0, 5) }) + '\n');
if (differences.length) process.exitCode = 1;
