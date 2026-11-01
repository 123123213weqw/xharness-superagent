/** Original migration assertions run unchanged with Rust migration and Store.
 * Original Prompts compose/history helpers remain JS consumers of migrated data.
 */
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
vi.mock('./upstream/src/app/migration.ts', async importOriginal => {
  const original = await importOriginal<any>()
  const { RustDatabaseSync } = await import('./store-target-adapter.ts')
  // Helper-only instances need no initialized schema; they use the same actual
  // Rust helpers as the constructor migration, not reference-derived answers.
  const rpc = (method: string, args: any[]) => {
    const db = new RustDatabaseSync(':memory:')
    try { return db.client.call(method, args) } finally { db.close() }
  }
  return { ...original,
    basisOf: (value: any) => rpc('migration.basis', [value]),
    reasonsOf: (value: any) => rpc('migration.reasons', [value]),
    withoutCopies: (value: any) => rpc('migration.withoutCopies', [value]),
    migrateSettings: (value: any) => JSON.parse(rpc('migration.settings', [JSON.stringify(value)]).encoded),
  }
})
afterEach(cleanupStoreClients)
afterAll(() => saveStoreEvidence('-migration', 'unchanged migration.spec.ts assertions through actual Rust schema-10 conversion and basis/reason/copy helpers; original JS Prompts composition/history and model controls are consumers, not native Prompt/Runtime/source coverage'))
await import('./upstream/tests/migration.spec.ts')
