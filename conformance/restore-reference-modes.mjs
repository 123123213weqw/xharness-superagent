// Git retains executable bits, not archive group-write bits. Restore only the
// pinned vendored reference's original modes, after verifying ALL source bytes.
// Never repairs content, changes manifests, or chmods symlinks/hard links.
import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { readFile, realpath, lstat, open } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { resolve, sep } from 'node:path';
const digest = data => createHash('sha256').update(data).digest('hex');
const sourceBytes = await readFile(new URL('./reference/SOURCE.json', import.meta.url));
const modeBytes = await readFile(new URL('./reference/file-modes.json', import.meta.url));
if (digest(sourceBytes) !== '68f0e828cb736d7253453da3cf7e85ed41af20f710919d5bfce18ab67b9c7120' || digest(modeBytes) !== '9608716daff6c00542091e3a5f2bb055eecc550ad75d771c8e1685f152a29b10') {
  throw new Error('Pinned reference manifest bytes differ; refusing mode restoration');
}
if (process.platform === 'win32') throw new Error('Archive mode restoration requires POSIX file modes');
const source = JSON.parse(sourceBytes), modes = JSON.parse(modeBytes);
if (source.commit !== '6337fc823ce20251c8de8085948165c81aaa701e' || Object.keys(source.file_sha256).length !== 216
    || Object.keys(modes).length !== 216 || Object.keys(modes).some(path => !(path in source.file_sha256))) {
  throw new Error('Pinned reference inventory differs');
}
const root = fileURLToPath(new URL('./reference/upstream/', import.meta.url));
if (await realpath(root) !== resolve(root)) throw new Error('Reference root must not be a symlink');
const handles = [];
let changed = 0;
try {
  // Hold the verified regular-file descriptors until the complete preflight
  // passes. A corrupt file or unsafe path must cause ZERO permission writes.
  for (const [name, expected] of Object.entries(source.file_sha256)) {
    const path = resolve(root, name), mode = Number(modes[name]);
    if (!path.startsWith(resolve(root) + sep) || ![0o664, 0o775].includes(mode)
        || await realpath(path) !== path || !(await lstat(path)).isFile()) {
      throw new Error(`Unsafe pinned reference path or mode: ${name}`);
    }
    const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    handles.push({ handle, mode });
    const info = await handle.stat();
    if (!info.isFile() || info.nlink !== 1 || digest(await handle.readFile()) !== expected) {
      throw new Error(`Pinned reference bytes/type/links differ: ${name}`);
    }
  }
  for (const { handle, mode } of handles) {
    if (((await handle.stat()).mode & 0o777) !== mode) { await handle.chmod(mode); changed++; }
  }
} finally {
  await Promise.all(handles.map(({ handle }) => handle.close()));
}
process.stdout.write(JSON.stringify({ commit: source.commit, checked: handles.length, modes_restored: changed, content_modified: false }) + '\n');
