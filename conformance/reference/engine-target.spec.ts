/** Run the upstream file unchanged. Only the Engine class is replaced, not assertions. */
import { afterAll, afterEach, vi } from 'vitest'
import { cleanupEngines, saveEvidence } from './engine-target-adapter.ts'
vi.mock('./upstream/src/core/index.ts', async importOriginal => {
  const original = await importOriginal<any>()
  const { createEngineAdapter } = await import('./engine-target-adapter.ts')
  return { ...original, Engine: createEngineAdapter(original) }
})
afterEach(cleanupEngines)
afterAll(() => saveEvidence())
await import('./upstream/tests/core-engine.spec.ts')
