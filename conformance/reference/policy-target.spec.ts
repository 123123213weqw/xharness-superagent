/** Replay the unchanged original policy assertions through a Rust boundary.
 * SourceRegistry and migration controls remain original TS, not Rust coverage.
 * The original unit tests use cast partial Ticket fixtures; hydrate only fields
 * those policy functions do not inspect. Full wire/model tests run separately.
 */
import { afterAll, vi } from 'vitest'
import { writeFileSync } from 'node:fs'

vi.mock('./upstream/src/policy.ts', async importOriginal => {
  const original = await importOriginal<any>()
  const { spawnSync } = await import('node:child_process')
  const { isDeepStrictEqual } = await import('node:util')
  const counts: Record<string, number> = {}
  ;(globalThis as any).__superagentRustPolicyCalls = counts
  const rpc = (request: any) => {
    const binary = process.env.SUPERAGENT_POLICY_BIN
    if (!binary) throw new Error('SUPERAGENT_POLICY_BIN must point to the already-built remote binary')
    const result = spawnSync(binary, [], { input: JSON.stringify(request) + '\n', encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 })
    if (result.error) throw result.error
    if (result.status !== 0) throw new Error(`Rust boundary ${request.op}: ${result.stderr}`)
    counts[request.op] = (counts[request.op] ?? 0) + 1
    return JSON.parse(result.stdout)
  }
  const clean = (value: unknown) => JSON.parse(JSON.stringify(value))
  const equal = (expected: unknown, actual: unknown) => {
    if (!isDeepStrictEqual(clean(expected), clean(actual))) throw new Error(`Rust/reference boundary divergence\nexpected ${JSON.stringify(expected)}\nactual ${JSON.stringify(actual)}`)
  }
  const schema = new Proxy(original.SettingsSchema, { get(target, key, receiver) {
    if (key === 'parse' || key === 'safeParse') return (input: any) => {
      const reference = original.SettingsSchema.safeParse(input)
      const target = rpc({ op: 'settings', settings: input })
      if (reference.success) equal({ ok: true, settings: reference.data, encoded: JSON.stringify(reference.data), version: original.policyVersion(reference.data) }, target)
      else equal({ ok: false, issues: reference.error.issues.map(({ code, path, message }: any) => ({ code, path, message })) }, target)
      if (key === 'safeParse') return reference
      if (!reference.success) throw reference.error
      // Internal Settings retain the schema's property order. The JSON envelope's
      // object presentation is not an authority identity; use the verified encoding.
      return JSON.parse(target.encoded)
    }
    return Reflect.get(target, key, receiver)
  } })
  const hydrateMandate = (partial: any) => ({ policyVersion: '', scope: '', ...partial })
  const ticket = (partial: any) => ({
    id: partial.id, title: '', decisionId: '', channels: [], assignments: [], inputs: [], undelivered: [], createdAt: '', updatedAt: '',
    origin: { triggerId: '', triggerIds: [] },
    ...partial,
    mandate: hydrateMandate(partial.mandate ?? { strength: 'explicit', basis: { via: 'listed' }, actor: { source: 'github', id: '' } }),
  })
  const scopeFor = (sources: any, mandate: any) => mandate.basis.source && mandate.basis.scope
    ? sources.scope({ source: mandate.basis.source, meta: mandate.basis.scope }) : undefined
  return { ...original, SettingsSchema: schema,
    policyVersion: (settings: any) => {
      const result = rpc({ op: 'settings', settings })
      if (!result.ok) throw new Error('Invalid typed settings fixture')
      equal(original.policyVersion(settings), result.version)
      return result.version
    },
    settingsPolicy: (settings: any, sources: any) => {
      const reference = original.settingsPolicy(settings, sources)
      const version = rpc({ op: 'settings', settings }).version
      equal(reference.version, version)
      return { ...reference, version,
        gate: (trigger: any, thread: any, tickets: any[]) => {
          const result = rpc({ op: 'gate', settings, trigger, thread, tickets: tickets.map(ticket), scope: sources.scope(trigger) }).gate
          equal(reference.gate(trigger, thread, tickets), result)
          return result
        },
        revalidate: (mandate: any) => {
          const result = rpc({ op: 'revalidate', settings, mandate: hydrateMandate(mandate), scope: scopeFor(sources, mandate) }).valid
          equal(reference.revalidate(mandate), result)
          return result
        },
        admitsInferred: (rules: string[]) => {
          const result = rpc({ op: 'admit', settings, rules }).admitted
          equal(reference.admitsInferred(rules), result)
          return result
        },
      }
    },
    assessmentOutcome: (assessment: any, settings: any, strength: string) => {
      const result = rpc({ op: 'assessment', assessment, settings, strength })
      if (result.error) {
        let caught: any
        try { original.assessmentOutcome(assessment, settings, strength) } catch (error) { caught = error }
        equal(caught?.message ?? null, result.error)
        throw new Error(result.error)
      }
      equal(original.assessmentOutcome(assessment, settings, strength), result.outcome)
      return result.outcome
    },
    policyEffects: (tickets: any[], old: any, now: any, sourceScope: any) => {
      const effects = new Map<string, unknown>()
      for (const current of tickets) {
        const result = rpc({ op: 'effects', old, new: now, tickets: [ticket(current)],
          oldScope: scopeFor(sourceScope(old), current.mandate), newScope: scopeFor(sourceScope(now), current.mandate) })
        for (const [id, effect] of result.effects) effects.set(id, effect)
      }
      equal([...original.policyEffects(tickets, old, now, sourceScope)], [...effects])
      return effects
    },
  }
})

// The original test file remains byte-for-byte unchanged.
await import('./upstream/tests/policy.spec.ts')
afterAll(() => {
  writeFileSync(new URL('./policy-target-results.json', import.meta.url), JSON.stringify({
    reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e',
    scope: 'unchanged policy.spec.ts assertions; Rust policy/settings boundaries checked; SourceRegistry/migration controls remain TS',
    rust_calls: (globalThis as any).__superagentRustPolicyCalls,
  }, null, 2) + '\n')
})
