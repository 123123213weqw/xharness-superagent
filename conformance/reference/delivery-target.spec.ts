/** Original assertions unchanged; only AgentDelivery class is replaced. */
import { afterAll, afterEach, vi } from 'vitest'
import { cleanupDelivery, saveDeliveryEvidence } from './delivery-target-adapter.ts'
vi.mock('./upstream/src/app/delivery.ts',async()=>({AgentDelivery:(await import('./delivery-target-adapter.ts')).RustAgentDelivery}))
afterEach(cleanupDelivery)
afterAll(saveDeliveryEvidence)
await import('./upstream/tests/delivery.spec.ts')
