#!/usr/bin/env python3
"""Read-only exploration fixture; ordinary Source behavior stays in base-gh."""
import json
import os
from pathlib import Path
import sys

root = Path(os.environ['GH_FIXTURE_STATE'])
args = sys.argv[1:]
path = args[1] if len(args) > 1 else ''
headers = ''
if path == 'search/issues':
    assert args[2:5] == ['--method', 'GET', '--include'], args
    fields = dict(s.split('=', 1) for s in args[6::2])
    assert fields['q'] == 'repo:owner/repo is:issue is:open "中文😀"', fields
    page = int(fields['page'])
    value = {'total_count': 2, 'incomplete_results': False, 'items': [
        {'number': 77 if page == 1 else 78, 'title': 'Selected 中文😀',
         'repository_url': 'https://api.github.com/repos/owner/repo'}]}
    if page == 1:
        headers = 'Link: <https://api.github.com/repositories/42/issues?page=2&per_page=20>; rel="next"\r\n'
elif path == 'repos/owner/repo/issues/77':
    assert args[2:] == ['--method', 'GET', '--include'], args
    value = {'number': 77, 'title': 'Chosen issue', 'body': 'Context from API 中文😀'}
elif path == 'repos/owner/repo/pulls?state=open&sort=updated&direction=desc&page=1&per_page=20':
    assert args[2:] == ['--method', 'GET', '--include'], args
    value = [{'number': 88, 'title': 'Relevant PR'}]
elif path == 'repos/owner/repo/pulls/88':
    assert args[2:] == ['--method', 'GET', '--include'], args
    value = {'number': 88, 'title': 'Relevant PR', 'body': 'Worker selected this PR from list'}
else:
    os.execv(sys.executable, [sys.executable, str(root / 'base-gh.py'), *args])
with (root / 'gh-calls.jsonl').open('a') as log:
    log.write(json.dumps(args) + '\n')
print('HTTP/2 200 OK\r\n' + headers + '\r\n' + json.dumps(value))
