import { createHash } from 'node:crypto';
import { readFile, stat } from 'node:fs/promises';
const source = JSON.parse(await readFile(new URL('./reference/SOURCE.json', import.meta.url), 'utf8'));
const modes = source.file_modes ? JSON.parse(await readFile(new URL('./reference/' + source.file_modes, import.meta.url), 'utf8')) : {};
const failures = [];
for (const [path, expected] of Object.entries(source.file_sha256)) {
  try {
    const data = await readFile(new URL('./reference/upstream/' + path, import.meta.url));
    const actual = createHash('sha256').update(data).digest('hex');
    if (actual !== expected) failures.push({ path, expected, actual });
    if (modes[path] && process.platform !== 'win32') {
      const actualMode = (await stat(new URL('./reference/upstream/' + path, import.meta.url))).mode & 0o777;
      if (actualMode !== Number(modes[path])) failures.push({ path, expected_mode: modes[path], actual_mode: actualMode.toString(8) });
    }
  } catch (error) { failures.push({ path, error: error.code }); }
}
process.stdout.write(JSON.stringify({ commit: source.commit, checked: Object.keys(source.file_sha256).length, failures }) + '\n');
if (failures.length) process.exitCode = 1;
