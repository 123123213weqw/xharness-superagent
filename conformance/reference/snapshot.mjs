import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { SettingsSchema, policyVersion } from './upstream/src/policy.ts';
import { LIMITS } from './upstream/src/limits.ts';
import { ADMISSION_DEFAULTS, ADMISSION_OPTIONS } from './upstream/src/admission.ts';
import { USER_ERRORS, REASONS } from './upstream/src/core/index.ts';
const sha = text => createHash('sha256').update(text).digest('hex');
const source = JSON.parse(await readFile(new URL('./SOURCE.json', import.meta.url), 'utf8'));
const settings = SettingsSchema.parse({});
const prompts = Object.fromEntries(await Promise.all(Object.entries(source.file_sha256)
  .filter(([name]) => name.startsWith('prompts/') || name.endsWith('/SKILL.md'))
  .map(async ([name]) => [name, { text: await readFile(new URL('./upstream/' + name, import.meta.url), 'utf8'), sha256: source.file_sha256[name] }])));
const imports = await readFile(new URL('./upstream/src/policy.ts', import.meta.url), 'utf8');
process.stdout.write(JSON.stringify({ schema_version: 1, reference_commit: source.commit,
  node: process.version, settings, policyVersion: policyVersion(settings), limits: LIMITS,
  admissionDefaults: ADMISSION_DEFAULTS, admissionOptions: ADMISSION_OPTIONS,
  errors: USER_ERRORS, reasons: REASONS, prompts, policy_source_sha256: sha(imports),
  scope: 'resolved_original_policy_and_default_text; runtime tool schemas captured in a later snapshot' }, null, 2)+'\n');
