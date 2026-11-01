import { afterAll, afterEach, vi, it, expect } from 'vitest'
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
afterAll(() => saveStoreEvidence())
await import('./upstream/tests/store.spec.ts')

it('retains original Policy identity and Settings encoding across save history and reopen', async () => {
  const { Store } = await import('./upstream/src/app/store.ts')
  const { SettingsSchema, policyVersion } = await import('./upstream/src/policy.ts')
  const { mkdtempSync, rmSync } = await import('node:fs')
  const { tmpdir } = await import('node:os')
  const { join } = await import('node:path')
  const directory = mkdtempSync(join(tmpdir(), 'store-identity-'))
  try {
    for (const names of [['zeta','alpha'],['alpha','zeta'],['source-9','source-2','zeta','alpha']]) {
      const path = join(directory, names.join('-') + '.sqlite')
      let store = new Store(path)
      const input = { policy: { sources: Object.fromEntries(names.map(id => [id, { allowed: [{ id }] }])) } }
      const expected = SettingsSchema.parse(input), identity = policyVersion(expected), encoded = JSON.stringify(expected)
      try {
        expect(JSON.stringify(store.saveSettings(input))).toBe(encoded)
        expect(policyVersion(store.settings())).toBe(identity)
        expect(JSON.stringify(store.settingsHistory()[0]!.settings)).toBe(encoded)
      } finally { store.close() }
      store = new Store(path)
      try { expect(policyVersion(store.settings())).toBe(identity); expect(JSON.stringify(store.settingsHistory()[0]!.settings)).toBe(encoded) }
      finally { store.close() }
    }
  } finally { rmSync(directory, { recursive:true, force:true }) }
})
