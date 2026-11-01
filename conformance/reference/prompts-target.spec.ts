/** Original assertions unchanged; real Rust Prompts + real Rust Store replace
 * the implementations. The file loader resolves a path then reads it in Rust. */
import { afterAll, afterEach, vi } from 'vitest'
import { cleanupStoreClients, saveStoreEvidence } from './store-target-adapter.ts'
vi.mock('node:sqlite', async () => ({DatabaseSync: (await import('./store-target-adapter.ts')).RustDatabaseSync}))
vi.mock('./upstream/src/app/store.ts', async importOriginal => {
  const original = await importOriginal<any>()
  const { RustDatabaseSync, createStoreAdapter } = await import('./store-target-adapter.ts')
  return { ...original, Store: createStoreAdapter(RustDatabaseSync) }
})
vi.mock('./upstream/src/app/skill-files.ts', async importOriginal => {
  const original = await importOriginal<any>()
  const { packagePath } = await import('./upstream/src/infra/paths.ts')
  const { readPromptFile } = await import('./prompts-target-adapter.ts')
  return { ...original, promptFile: (name: string) => readPromptFile(packagePath('prompts', name + '.md')) }
})
vi.mock('./upstream/src/app/prompts.ts', async importOriginal => {
  const original = await importOriginal<any>()
  const target = await import('./prompts-target-adapter.ts')
  return { ...original, ...target, Prompts: target.RustPrompts }
})
afterEach(cleanupStoreClients)
afterAll(() => saveStoreEvidence('', 'unchanged original prompts.spec assertions through production Rust Prompts and SQLite Store, including raw prompt-file reads; JS only supplies callback bodies/path resolution/error constructors; no native request/skills/Root installer claim'))
await import('./upstream/tests/prompts.spec.ts')
