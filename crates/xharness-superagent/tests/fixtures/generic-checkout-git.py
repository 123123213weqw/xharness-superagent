#!/usr/bin/env python3
"""Only remote URL transport is replaced. All Git operations execute real Git."""
import json, os, sys, time
from pathlib import Path
root=Path(__file__).resolve().parent.parent
args=sys.argv[1:];env=dict(os.environ);real=(root/'real-git').read_text()
if 'fetch' in args:
    rows=json.loads((root/'checkout-map.json').read_text());found=False
    for repo,row in rows.items():
        url='https://github.com/'+repo+'.git'
        if url in args:
            assert env['GIT_ALLOW_PROTOCOL']=='https'
            assert env['GIT_CONFIG_GLOBAL']=='/dev/null'
            assert not any(k in env for k in ['SSH_AUTH_SOCK','DEEPSEEK_API_KEY','GIT_CONFIG_COUNT'])
            with (root/'fetch-calls.jsonl').open('a') as f:f.write(json.dumps(args)+'\n')
            if (root/'fail-fetch').exists(): sys.exit(1)
            if (root/'block-fetch').exists():
                ready=root/'fetch-ready.tmp';ready.write_text(str(os.getpid()));ready.replace(root/'fetch-ready');time.sleep(30)
            args[args.index(url)]=row['path'];env['GIT_ALLOW_PROTOCOL']='file';found=True;break
    assert found,args
os.execve(real,[real,*args],env)
