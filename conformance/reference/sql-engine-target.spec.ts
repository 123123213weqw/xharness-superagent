/** Unchanged core assertions; only the Engine is replaced with the SQL-backed
 * native Engine. No expected values or reference implementation compute results. */
import {afterAll,afterEach,vi} from 'vitest'
import {cleanupEngines,saveEvidence} from './engine-target-adapter.ts'
vi.mock('./upstream/src/core/index.ts',async importOriginal=>{
 const original=await importOriginal<any>();const {createEngineAdapter}=await import('./engine-target-adapter.ts')
 return {...original,Engine:createEngineAdapter(original)}
})
afterEach(cleanupEngines)
afterAll(()=>saveEvidence('','unchanged core-engine.spec.ts against actual Rust Engine and native SqlRepository; test-only full fixture import/export, Policy/Scope/Clock/observational ports and model helper controls remain JS; no production async Runtime or Source claim'))
await import('./upstream/tests/core-engine.spec.ts')
