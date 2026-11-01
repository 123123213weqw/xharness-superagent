import { test, expect } from './upstream/node_modules/vitest/dist/index.js'
import { readFileSync, writeFileSync } from 'node:fs'
import ts from './upstream/node_modules/typescript/lib/typescript.js'
import { startRuntime, feishu } from './upstream/tests/fixtures/host.ts'
import { builtInParts, defaultPrompts } from './upstream/src/app/prompts.ts'
import { RESOURCES } from './upstream/src/app/resources.ts'
import { TICKET_STATES } from './upstream/src/app/cards.ts'
import { THREAD_FILTERS, TICKET_FILTERS } from './upstream/src/app/api.ts'

test('capture actual registered role contracts from the original Runtime', async () => {
  const host = await startRuntime()
  try {
    const { sessionId } = await host.acceptedTicket(feishu('1', '@bot 修复拼写错误'))
    const tools = (id: string) => [...host.tools.get(id)!.values()].map(tool => {
      const actual = tool as unknown as { name: string; description: string; parameters: unknown }
      expect(actual.parameters).toBeDefined()
      return { name: actual.name, description: actual.description, parameters: actual.parameters }
    })
    const main = tools(host.runtime.masterId), worker = tools(sessionId)
    expect(main).toHaveLength(14)
    expect(worker).toHaveLength(6)
    const apiSource = readFileSync(new URL('./upstream/src/app/api.ts', import.meta.url), 'utf8')
    const ast = ts.createSourceFile('api.ts', apiSource, ts.ScriptTarget.Latest, true)
    const declaration = ast.statements.find(node => ts.isInterfaceDeclaration(node) && node.name.text === 'Api')
    expect(declaration).toBeDefined()
    const routes = (declaration as ts.InterfaceDeclaration).members.map(member => ({
      route: member.name!.getText(ast), declaration: member.getText(ast),
      line: ast.getLineAndCharacterOfPosition(member.getStart(ast)).line + 1,
    }))
    writeFileSync(new URL('./runtime-contracts.json', import.meta.url), JSON.stringify({
      schema_version: 1, reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e',
      scope: 'original Runtime product tools with official in-memory host fixture; not real DSH/provider envelope',
      roles: { main, worker }, resources: RESOURCES, ticket_states: TICKET_STATES,
      api: { routes, thread_filters: THREAD_FILTERS, ticket_filters: TICKET_FILTERS },
      prompts: { defaults: defaultPrompts(), parts: builtInParts() },
    }, null, 2) + '\n')
  } finally { await host.stop() }
})
