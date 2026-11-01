import { afterAll, afterEach, vi } from 'vitest'
import { cleanupStoreClients, saveStoreEvidence } from './store-target-adapter.ts'
vi.mock('node:sqlite', async () => {
  const { RustDatabaseSync } = await import('./store-target-adapter.ts')
  return { DatabaseSync: RustDatabaseSync }
})
vi.mock('./upstream/src/app/store.ts', async importOriginal => {
  const original = await importOriginal<any>()
  const { DatabaseSync } = await import('node:sqlite')
  const { createStoreAdapter } = await import('./store-target-adapter.ts')
  return { ...original, Store: createStoreAdapter(DatabaseSync) }
})
vi.mock('./upstream/src/app/effects.ts', async importOriginal => {
  const original = await importOriginal<any>()
  const { RustEffects, Later } = await import('./store-target-adapter.ts')
  return { ...original, Effects: RustEffects, Later }
})
afterEach(cleanupStoreClients)
afterAll(() => saveStoreEvidence('-effects'))
await import('./upstream/tests/effects.spec.ts')
