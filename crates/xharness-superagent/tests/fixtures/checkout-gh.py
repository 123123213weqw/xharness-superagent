#!/usr/bin/env python3
"""Offline gh transport only; the checkout itself is a real Git repository."""
import json,os,subprocess,sys,time
from pathlib import Path
root=Path(os.environ['GH_FIXTURE_STATE']);args=sys.argv[1:]
if args[:2]==['repo','clone']:
    assert args[2]=='https://github.com/owner/repo.git' and args[4]=='--',args
    assert '--no-recurse-submodules' in args
    assert os.environ['GIT_CONFIG_GLOBAL']=='/dev/null' and os.environ['GIT_ALLOW_PROTOCOL']=='https'
    assert 'SSH_AUTH_SOCK' not in os.environ and 'GIT_CONFIG_COUNT' not in os.environ
    with (root/'clone-calls.jsonl').open('a') as f:f.write(json.dumps(args)+'\n')
    dest=Path(args[3])
    if (root/'block-clone').exists():
        (root/'clone-ready').write_text(str(os.getpid()))
        time.sleep(30)
    env=dict(os.environ,GIT_ALLOW_PROTOCOL='file') # fixture-local boundary only
    subprocess.run(['git','clone','--no-local',str(root/'template'),str(dest),*args[5:]],env=env,check=True)
    # Simulate an auth helper in the clone config; product must replace it.
    with (dest/'.git/config').open('a') as f:f.write('\n[credential]\nhelper = fixture-private-helper\n')
    sys.exit(0)
if len(args)>1 and args[1]=='repos/owner/repo/git/ref/heads/main':
    head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root/'template',text=True).strip()
    if (root/'move-base').exists():head='f'*40
    print(json.dumps({'object':{'sha':head}}));sys.exit(0)
if len(args)>1 and args[1].startswith('repos/owner/repo/git/commits/'):
    head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root/'template',text=True).strip()
    assert args[1].endswith('/'+head),args
    tree=subprocess.check_output(['git','rev-parse','HEAD^{tree}'],cwd=root/'template',text=True).strip()
    print(json.dumps({'sha':head,'tree':{'sha':tree}}));sys.exit(0)
if len(args)>1 and args[1].startswith('repos/owner/repo/git/trees/'):
    tree=subprocess.check_output(['git','rev-parse','HEAD^{tree}'],cwd=root/'template',text=True).strip()
    assert args[1].endswith('/'+tree),args
    entries=[]
    for line in subprocess.check_output(['git','ls-tree',tree],cwd=root/'template',text=True).splitlines():
        metadata,name=line.split('\t',1);mode,kind,oid=metadata.split(' ')
        entries.append({'path':name,'mode':mode,'type':kind,'sha':oid})
    print(json.dumps({'sha':tree,'truncated':False,'tree':entries}));sys.exit(0)
exec(compile((root/'base-gh.py').read_text(),str(root/'base-gh.py'),'exec'))
