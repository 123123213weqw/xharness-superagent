// Offline execution of the unmodified pinned Source functions. Test-only Zod
// comes from conformance/package-lock.json, never the production binary.
import { writeFile, readFile } from 'node:fs/promises';
import assert from 'node:assert/strict';
import { CommentSchema, IssueSchema, commentTrigger, issueTrigger, pullTrigger, mentions, githubAddress, ciTrigger, branchCiTrigger } from '../reference/upstream/src/sources/github-trigger.ts';
import { sendGithub } from '../reference/upstream/src/sources/github-channel.ts';
import { CliError } from '../reference/upstream/src/infra/commands.ts';

const fixed = '2026-11-02T00:00:00.000Z';
const OriginalDate = Date;
globalThis.Date = class extends OriginalDate {
  constructor(...args) { super(...(args.length ? args : [fixed])); }
  static now() { return OriginalDate.parse(fixed); }
};
const base = { id: 101, body: '@robot 请修复 😀', user: { login: 'alice', type: 'User' }, html_url: 'https://github.com/owner/repo/issues/12#issuecomment-101', created_at: fixed, updated_at: fixed };
const reference = { number: 12, isPr: false, url: 'https://github.com/owner/repo/issues/12' };
const comments = [
  ['conversation', {}], ['conversation', { user: null }], ['conversation', { user: { login: 'robot', type: 'User' } }],
  ['conversation', { user: { login: 'ci[bot]', type: 'User' } }], ['conversation', { user: { login: 'app', type: 'Bot' } }],
  ['conversation', { updated_at: '2026-11-02T00:01:00.000Z' }],
  ['inline', { path: 'src/文件.rs', line: null, original_line: 8, diff_hunk: 'first\nsecond\nthird\nfourth', in_reply_to_id: 88, html_url: 'https://github.com/owner/repo/pull/12#discussion_r101' }],
  ['inline', { path: 'a.rs', body: null, diff_hunk: null }],
  ['review', { state: 'APPROVED', commit_id: 'abcdef0', body: null }],
  ['review', { state: 'COMMENTED', body: null }],
  ['review', { updated_at: null, submitted_at: fixed, created_at: null, state: 'CHANGES_REQUESTED', commit_id: null }],
  ['conversation', { html_url: null, updated_at: null, created_at: null }],
].map(([kind, patch]) => {
  const input = { ...base, ...patch };
  const ref = kind === 'conversation' ? reference : { ...reference, isPr: true, prAuthor: 'bob' };
  return { input, reference: ref, kind, now: fixed, expected: commentTrigger(CommentSchema.parse(input), ref, kind, 'robot') };
});
const issues = [false, true].flatMap(pr => [false, true].map(titleOnly => {
  const input = { ...base, number: 12, title: titleOnly ? '@robot title' : 'Fix copy', body: titleOnly ? 'body' : base.body, state: 'open', ...(pr ? { pull_request: { url: 'https://api.github.com/repos/owner/repo/pulls/12' } } : {}) };
  return { input, expected: pr ? pullTrigger(IssueSchema.parse(input), 'robot') : issueTrigger(IssueSchema.parse(input), 'robot') };
}));
const mentionTexts = ['@robot', 'hello @ROBOT!', '中文@robot。', 'x@robot', '@@robot', '@robot-foo', '@robot_name', '@robot1', '@rob', '`@robot`', '😀@robot', '@robot/yes'];
const mentionCases = mentionTexts.map(text => ({ text, expected: mentions(text, 'robot') }));
const validity = [{ user: { login: 'alice', type: null } }, { user: { login: 'alice' } }, { user: null }, { user: {} }, { user: [] }, { user: { login: null } }, { user: { login: 'app', type: 'Bot' } }].map(patch => { const input = { ...base, ...patch }; return { input, expected: CommentSchema.safeParse(input).success }; });
const addresses = ['https://github.com/owner/repo/pull/12#discussion_r101', 'https://api.github.com/repos/OWNER/REPO/pulls/12/', 'https://github.com/owner/repo/issues/12#', 'https://github.com/owner/repo/pull/12#DiScUsSiOn_r101', 'https://github.com/owner/repo/pull/12#discussion_rabc', 'https://github.com/other/repo/pull/12', 'https://github.com/owner/repo/issues/12?q=1', 'http://github.com/owner/repo/issues/12', 'https://github.com/owner/repo/issues/12/extra'].map(url => ({ url, expected: githubAddress(url, 'owner/repo') ?? null }));
const deployment = { repository: 'owner/repo', github: { login: 'robot' } };
const receipt = { id: 'receipt-1', channel: { source: 'github', thread: 'github:12', address: { number: '12' } }, text: '已完成 😀' };
const channels = [];
for (const kind of ['top', 'duplicate', 'other-account', 'inline', 'inline-other-root', 'deleted-inline', 'inline-gone', 'not-found', 'gone', 'locked', 'forbidden']) {
  const trace = [];
  const inline = kind.startsWith('inline') || kind === 'deleted-inline';
  const message = structuredClone(receipt);
  if (inline) message.channel.address.url = 'https://github.com/owner/repo/pull/12#discussion_r101';
  const commands = { deployment,
    api: async (_schema, path, ...flags) => {
      trace.push({ op: 'api', path, flags });
      if (path === 'pulls/comments/101') {
        if (kind === 'deleted-inline' || kind === 'inline-gone') throw new CliError('comment missing', kind === 'inline-gone' ? 410 : 404);
        return { id: 101, in_reply_to_id: 88 };
      }
      return {};
    },
    pages: async (_schema, path) => {
      trace.push({ op: 'pages', path });
      const errors = { 'not-found': [404, 'Not Found'], gone: [410, 'Gone'], locked: [403, 'Issue locked'], forbidden: [403, 'Permission denied'] };
      if (errors[kind]) throw new CliError(errors[kind][1], errors[kind][0]);
      if (kind === 'duplicate' || kind === 'other-account' || kind === 'inline-other-root') return [{ id: 11, user: { login: kind === 'other-account' ? 'alice' : 'ROBOT' }, body: '<!-- dsh-superagent:receipt-1 -->', ...(kind === 'inline-other-root' ? { in_reply_to_id: 9 } : {}) }];
      return [];
    },
  };
  let expected;
  try { expected = { state: await sendGithub({ commands }, message) }; }
  catch (error) { expected = { error: error.message, code: error.code }; }
  channels.push({ kind, message, expected, trace });
}
const ci = [
  ciTrigger({ number: 12, html_url: 'https://github.com/owner/repo/pull/12', user: { login: 'bob' } }, 3, 'CI failed'),
  branchCiTrigger('main', 'abcdef', 4, 'CI recovered', 'https://github.com/owner/repo/commit/abcdef'),
];
const result = { reference_commit: '6337fc823ce20251c8de8085948165c81aaa701e', scope: 'offline unmodified trigger/channel functions; not full Source/CLI/product acceptance', deployment: { repository: 'owner/repo', login: 'robot', noreply: '1+robot@users.noreply.github.com' }, comments, issues, mentions: mentionCases, addresses, channels, ci, validity };
const target = new URL('./github-source-oracle.json', import.meta.url);
if (process.argv.includes('--check')) assert.deepEqual(JSON.parse(await readFile(target, 'utf8')), result);
else await writeFile(target, JSON.stringify(result, null, 2) + '\n');
console.log(`${comments.length} comment, ${issues.length} body, ${mentionCases.length} mention, ${addresses.length} address, ${channels.length} delivery, ${ci.length} CI, ${validity.length} account-metadata Source oracle fixtures verified.`);
