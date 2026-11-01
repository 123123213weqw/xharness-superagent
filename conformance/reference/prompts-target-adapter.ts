/** Transport only: Rust performs schema/semantic validation, text composition,
 * versioning/CAS/history/default diffs and SQLite work. JS supplies callbacks,
 * file-path resolution and the original error constructors, not the algorithms. */
import { RustDatabaseSync, createStoreAdapter } from './store-target-adapter.ts'
import { UserError } from './upstream/src/core/index.ts'
import { ZodError } from './upstream/node_modules/zod/index.js'
export class PromptConflict extends Error {}
const nativeErrors = new WeakMap<Error, string>()
export const nativeErrorMessage = (error: Error) => nativeErrors.get(error)
const RustStore = createStoreAdapter(RustDatabaseSync)
let pure: any
function decode(error: any): never {
  if (error.type === 'UserError') { const result:any = new UserError(error.user.code, error.user.values); nativeErrors.set(result, error.message); throw result }
  if (error.type === 'ZodError') throw new ZodError(JSON.parse(error.issuesEncoded))
  if (error.type === 'PromptConflict') throw new PromptConflict(error.message)
  throw new Error(error.message)
}
function client() {
  if (!pure || pure.db.client.closed) { pure = new RustStore(':memory:'); pure.db.client.promptError = decode }
  return pure.db.client
}
function run(method: string, args: any[]) { return JSON.parse(client().call('prompts.' + method, [0, ...args]).encoded) }
const encoded = (value: any) => JSON.stringify(value)
export const defaultPrompts = () => run('defaultPrompts', [])
export const builtInParts = () => run('builtInParts', [])
export const builtInBase = (text: string) => run('builtInBase', [text])
export const composePrompt = (config: any, role: string, environment?: string, policy?: any) => run('composePrompt', [encoded(config), role, environment ?? null, policy ?? null])
export const promptParts = (config: any, role: string, environment?: string, policy?: any) => run('promptParts', [encoded(config), role, environment ?? null, policy ?? null])
export const composeSkill = (config: any, name: string) => run('composeSkill', [encoded(config), name])
export const policySection = (settings: any, config?: any) => run('policySection', [encoded(settings), config === undefined ? null : encoded(config)])
export const environmentSection = (config: any, variables?: any[]) => run('environmentSection', [encoded(config), variables ?? null])
export const readPromptFile = (absolutePath: string) => run('promptFile', [absolutePath])
export const normalizeText = (text: string) => run('normalizeText', [text])
export class RustPrompts {
  store: any; id: number
  constructor(store: any, maxChars: () => number, environment: () => any, feishu: () => boolean) {
    this.store = store; const client = store.db.client
    this.id = ++client.next; client.promptError = decode
    client.promptInputs.set(this.id, { maxChars, environment, feishu, now: () => new Date().toISOString() })
  }
  call(method: string, args: any[] = []) { return JSON.parse(this.store.db.client.call('prompts.' + method, [this.id, ...args]).encoded) }
  current() { return this.call('current') }
  version(revision: number) { return this.call('version', [revision]) }
  validate(value: unknown) { return this.call('validate', [encoded(value)]) }
  preview(value: unknown) { return this.call('preview', [encoded(value)]) }
  save(value: unknown, revision: number, origin: string) { return this.call('save', [encoded(value), revision, origin]) }
  defaultChanged(config: any) { return this.call('defaultChanged', [encoded(config)]) }
  history(offset = 0, limit = 20) { return this.call('history', [offset, limit]) }
  compose(config: any, role: string, variables?: any[]) { return this.call('compose', [encoded(config), role, variables ?? null]) }
  skills(config: any) { return this.call('skills', [encoded(config)]) }
}
