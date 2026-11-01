#!/usr/bin/env python3
"""Offline gh executable protocol fixture; never connects to GitHub."""
import datetime
import json
import os
from pathlib import Path
import sys

root = Path(os.environ['GH_FIXTURE_STATE'])
args = sys.argv[1:]
with (root / 'gh-calls.jsonl').open('a') as log:
    log.write(json.dumps(args) + '\n')
assert args[0] == 'api', args
path = args[1]
posts_path = root / 'posts.json'
posts = json.loads(posts_path.read_text()) if posts_path.exists() else []
if path == 'user':
    if os.environ.get('GH_FIXTURE_SETUP_ERROR'):
        print('fixture account unavailable (HTTP 401)', file=sys.stderr)
        sys.exit(1)
    value = {'id': 1, 'login': 'robot'}
elif path == 'repos/owner/repo':
    value = {'full_name': 'owner/repo', 'permissions': {'push': True}, 'default_branch': 'main'}
elif path.startswith('repos/owner/repo/notifications?'):
    print('HTTP/2 200 OK\r\nX-Poll-Interval: 120\r\n\r\n' + json.dumps([
        {'repository': {'full_name': 'owner/repo'}, 'subject': {'url': 'https://api.github.com/repos/owner/repo/issues/12'}}
    ]))
    sys.exit(0)
elif path == 'repos/owner/repo/issues/12':
    # The first request is the Source poll; the second is a real read tool.
    if os.environ.get('GH_FIXTURE_BLOCK_READ') and (root / 'timestamp').exists():
        import time
        (root / 'blocked-read-pid').write_text(str(os.getpid()))
        time.sleep(120)
    value = {'id': 12, 'number': 12, 'user': {'login': 'bob', 'type': 'User'},
             'title': 'Original issue', 'state': 'open', 'body': 'Old issue',
             'created_at': '2026-10-30T00:00:00.000Z', 'html_url': 'https://github.com/owner/repo/issues/12'}
elif path.startswith('repos/owner/repo/issues/12/comments?'):
    comment_id = int(os.environ.get('GH_FIXTURE_COMMENT_ID', '101'))
    timestamp = root / ('timestamp' if comment_id == 101 else f'timestamp-{comment_id}')
    if not timestamp.exists():
        timestamp.write_text((datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=1)).isoformat(timespec='milliseconds').replace('+00:00', 'Z'))
    at = timestamp.read_text()
    actor = os.environ.get('GH_FIXTURE_ACTOR', 'alice')
    value = [[{'id': comment_id, 'user': {'login': actor, 'type': 'Bot' if actor == 'other[bot]' else 'User'},
               'body': os.environ.get('GH_FIXTURE_TEXT', '@robot Create result.txt in the workspace 😀'), 'created_at': at, 'updated_at': at,
               'html_url': f'https://github.com/owner/repo/issues/12#issuecomment-{comment_id}',
               'issue_url': 'https://api.github.com/repos/owner/repo/issues/12'}]]
elif path == 'repos/owner/repo/issues/12/comments':
    if 'POST' in args:
        assert '-f' in args, args
        body = next(a[5:] for a in args if a.startswith('body='))
        value = {'id': 200 + len(posts), 'body': body, 'user': {'login': 'robot'}}
        posts.append(value)
        posts_path.write_text(json.dumps(posts))
    else:
        assert '--paginate' in args and '--slurp' in args, args
        value = [posts]
else:
    raise AssertionError(f'Unexpected offline gh call: {args}')
print(json.dumps(value))
