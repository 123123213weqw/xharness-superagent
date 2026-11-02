#!/usr/bin/env python3
"""Offline API boundary. Local git is real; production never reads this map."""
import json, os, subprocess, sys
from pathlib import Path
from urllib.parse import unquote
root = Path(os.environ['GH_FIXTURE_STATE'])
args = sys.argv[1:]
assert args[0] == 'api', args
path = unquote(args[1])
rows = json.loads((root/'checkout-map.json').read_text())
real = (root/'real-git').read_text()
def git(row, *args):
    return subprocess.check_output([real, *args], cwd=row['path'], text=True).strip()
for repo, row in rows.items():
    base = 'repos/'+repo
    if path == base:
        if (root/'base-gh.py').exists(): break
        value = {'full_name':repo,'permissions':{'push':True}}
        if not (root/'missing-default').exists(): value['default_branch']=row['branch']
    elif path.startswith(base+'/git/ref/heads/'):
        name=path[len(base+'/git/ref/heads/'):]
        value={'object':{'sha':'f'*40 if (root/'move-ref').exists() else git(row,'rev-parse','refs/heads/'+name)}}
    elif path.startswith(base+'/git/commits/'):
        value={'sha':git(row,'rev-parse',path.rsplit('/',1)[1]+'^{commit}')}
    elif path.startswith(base+'/pulls/'):
        value={'base':{'repo':{'full_name':'foreign/repo' if (root/'foreign-base').exists() else repo}},'head':{'sha':git(row,'rev-parse','refs/pull/7/head'),'ref':'feature','repo':{'full_name':repo}}}
    else: continue
    print(json.dumps(value)); sys.exit(0)
if (root/'base-gh.py').exists():
    exec(compile((root/'base-gh.py').read_text(),str(root/'base-gh.py'),'exec')); sys.exit(0)
raise AssertionError(args)
