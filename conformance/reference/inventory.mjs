import ts from './upstream/node_modules/typescript/lib/typescript.js';
import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
const rubric = JSON.parse(await readFile(new URL('../rubric.json', import.meta.url), 'utf8'));
const files = [];
for (const path of rubric.reference_test_suite_inventory) {
  const source = await readFile(new URL('./upstream/' + path, import.meta.url), 'utf8');
  const ast = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true,
    path.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
  const declarations = [], assertions = [];
  const location = node => ast.getLineAndCharacterOfPosition(node.getStart(ast)).line + 1;
  const visit = node => {
    if (ts.isCallExpression(node)) {
      const expression = node.expression.getText(ast), title = node.arguments[0];
      if (/^(?:it|test)(?:[.(]|$)/.test(expression) && title && ts.isStringLiteralLike(title)) {
        declarations.push({ line: location(node), title: title.text, registration: expression,
          parameterized: expression.includes('.each(') });
      }
      if (ts.isPropertyAccessExpression(node.expression) && /^to[A-Z]/.test(node.expression.name.text)
        && /^expect(?:\.(?:soft|poll))?\(/.test(expression)) {
        assertions.push({ line: location(node), matcher: node.expression.name.text, text: node.getText(ast) });
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(ast);
  files.push({ path, sha256: createHash('sha256').update(source).digest('hex'),
    groups: Object.entries(rubric.reference_suite_group_mapping).filter(([, paths]) => paths.includes(path)).map(([group]) => group),
    declarations, assertions, assertion_to_rubric_review: 'pending' });
}
const result = { schema_version: 1, reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e',
  scope: 'TypeScript AST inventory; parameterized/loop tests expand at runtime; assertion-level requirement review is still pending', files };
await writeFile(new URL('./assertion-inventory.json', import.meta.url), JSON.stringify(result, null, 2) + '\n');
process.stdout.write(JSON.stringify({ files: files.length, declarations: files.reduce((sum, file) => sum + file.declarations.length, 0),
  assertion_calls: files.reduce((sum, file) => sum + file.assertions.length, 0) }) + '\n');
