/** Original assertions unchanged; only Passes class is replaced by native Rust. */
import { afterAll, afterEach, vi } from 'vitest'
import { cleanupPasses, savePassesEvidence, advancePassClocks } from './passes-target-adapter.ts'
vi.mock('./upstream/src/app/passes.ts',async()=>({Passes:(await import('./passes-target-adapter.ts')).RustPasses}))
const advance=vi.advanceTimersByTimeAsync.bind(vi)
vi.spyOn(vi,'advanceTimersByTimeAsync').mockImplementation(ms=>advancePassClocks(ms,advance))
afterEach(cleanupPasses)
afterAll(()=>{savePassesEvidence();vi.restoreAllMocks()})
await import('./upstream/tests/passes.spec.ts')
