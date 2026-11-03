#!/usr/bin/env python3
"""Offline reader endpoints for both unchanged TypeScript and Rust CLI adapters."""
import json
import os
from pathlib import Path
import sys
root=Path(os.environ['FEISHU_READER_ROOT']);args=sys.argv[1:]
with (root/'trace.jsonl').open('a') as f:f.write(json.dumps(args,ensure_ascii=False)+'\n')
if args==['auth','status']:
    print(json.dumps({'appId':'cli_app','identities':{'bot':{'status':'ready'}}}));sys.exit(0)
if args==['config','strict-mode']:
    print('strict-mode: bot');sys.exit(0)
if args==['api','GET','/open-apis/bot/v3/info','--as','bot','--format','ndjson']:
    print(json.dumps({'bot':{'open_id':'ou_bot','app_name':'Reader'}}));sys.exit(0)
assert args[-4:]==['--as','bot','--format','json'],args
args=args[:-4];responses=json.loads((root/'responses.json').read_text())
if args[:3]==['api','GET','/open-apis/im/v1/messages/mget']:
    assert args[3]=='--params' and len(args)==5
    key='source'
elif args[:2]==['im','+messages-mget']:
    assert args[2]=='--message-ids' and args[4:]==['--no-reactions']
    key='message'
elif args[:2]==['im','+threads-messages-list']:
    assert args[2]=='--thread' and args[4]=='--order' and args[6]=='--page-size' and args[8]=='--no-reactions'
    key='thread'
elif args[:3]==['api','GET','/open-apis/im/v1/messages']:
    assert args[3]=='--params' and len(args)==5
    params=json.loads(args[4]);key='earlier' if 'end_time' in params else 'later'
else:
    raise RuntimeError('unexpected reader args '+repr(args))
assert key in responses,(key,args)
print(json.dumps(responses[key],ensure_ascii=False))
