/** Exact structural comparison. Deliberately does not erase identity, timing or authorization differences. */
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
export function firstDifference(a, b, path = '$') {
  if (Object.is(a, b)) return null;
  if (a === null || b === null || typeof a !== 'object' || typeof b !== 'object') return { path, expected: a, actual: b };
  if (Array.isArray(a) !== Array.isArray(b)) return { path, expected: a, actual: b };
  if (Array.isArray(a)) {
    if (a.length !== b.length) return { path: path + '.$length', expected: a.length, actual: b.length };
    for (let index = 0; index < a.length; index++) {
      const difference = firstDifference(a[index], b[index], path + '.' + index);
      if (difference) return difference;
    }
    return null;
  }
  const ak = Object.keys(a).sort(), bk = Object.keys(b).sort();
  if (JSON.stringify(ak) !== JSON.stringify(bk)) return { path: path + '.$keys', expected: ak, actual: bk };
  for (const key of ak) {
    const difference = firstDifference(a[key], b[key], path + '.' + key);
    if (difference) return difference;
  }
  return null;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (!process.argv[2] || !process.argv[3]) throw new Error('Usage: compare.mjs <reference.json> <target.json>');
  const [expected, actual] = await Promise.all(process.argv.slice(2, 4).map(async path => JSON.parse(await readFile(path, 'utf8'))));
  const difference = firstDifference(expected, actual);
  process.stdout.write(JSON.stringify({ equal: difference === null, first_difference: difference }) + '\n');
  if (difference) process.exitCode = 1;
}
