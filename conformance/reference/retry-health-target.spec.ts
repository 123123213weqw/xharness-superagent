/** Original source and all assertions unchanged; replace only RetryHealth. */
import {afterAll,afterEach,vi} from 'vitest'
import {cleanupRetryHealth,saveRetryHealthEvidence} from './retry-health-target-adapter.ts'
vi.mock('./upstream/src/app/retry-health.ts',async()=>({RetryHealth:(await import('./retry-health-target-adapter.ts')).RustRetryHealth}))
afterEach(cleanupRetryHealth)
afterAll(saveRetryHealthEvidence)
await import('./upstream/tests/retry-health.spec.ts')
