#!/usr/bin/env python3
"""GitHub boundary fixture; never performs network or pretends to be live GitHub."""
import hashlib,json,os,sys,urllib.parse
from pathlib import Path
root=Path(os.environ['GH_FIXTURE_STATE']);args=sys.argv[1:];path=args[1]
if not (path.endswith('/pulls') or any(x in path for x in ['/git/','/pulls?state=all','/pulls/31/','/commits/'])):
    exec(compile((root/'base-gh.py').read_text(),str(root/'base-gh.py'),'exec'))
    sys.exit(0)
with (root/'gh-calls.jsonl').open('a') as f:f.write(json.dumps(args)+'\n')
prefix='repos/owner/repo/'
assert path.startswith(prefix),args
path=path[len(prefix):]
state_path=root/'delivery-state.json'
s=json.loads(state_path.read_text()) if state_path.exists() else {'base':'1'*40,'base_tree':'2'*40,'blobs':{},'commits':{},'branches':{},'trees':{},'pulls':[],'writes':[]}
method='POST' if '--method' in args else 'GET'
fields=[]
for i in range(2,len(args),2):
    flag,value=args[i:i+2]
    if flag=='--method':assert value=='POST'
    else:
        assert flag in ['--raw-field','--field'],args
        fields.append((flag,*value.split('=',1)))
d={k:v for _,k,v in fields};value=None
if method=='POST':s['writes'].append({'path':path,'fields':fields})
if path.startswith('git/ref/heads/'):
    b=urllib.parse.unquote(path[len('git/ref/heads/'):])
    head=s['base'] if b=='main' else s['branches'].get(b)
    if not head:
        print('Not Found (HTTP 404)',file=sys.stderr);sys.exit(1)
    value={'object':{'sha':head}}
elif path.startswith('git/commits/'):
    oid=path.split('/')[-1]
    value={'sha':oid,'tree':{'sha':s['base_tree']},'parents':[]} if oid==s['base'] else s['commits'][oid]
elif path.startswith('git/trees/'):
    value={'sha':s['base_tree'],'truncated':False,'tree':[{'path':'README.md','mode':'100644','type':'blob','sha':'3'*40},{'path':'run.sh','mode':'100755','type':'blob','sha':'4'*40}]}
elif path=='git/blobs':
    assert d['encoding']=='utf-8'
    b=d['content'].encode();oid=hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest()
    s['blobs'][oid]=d['content'];value={'sha':oid}
elif path=='git/trees':
    assert d['base_tree']==s['base_tree'];items=[];keys=['tree[][path]','tree[][mode]','tree[][type]','tree[][sha]']
    remaining=fields[1:]
    for at in range(0,len(remaining),4):
        part=remaining[at:at+4];assert [p[1] for p in part]==keys
        items.append({p[1]:p[2] for p in part})
    oid=hashlib.sha1(json.dumps(items,sort_keys=True).encode()).hexdigest()
    s['trees'][oid]=items;value={'sha':oid}
elif path=='git/commits':
    assert d['parents[]']==s['base'];oid=hashlib.sha1(json.dumps(d,sort_keys=True).encode()).hexdigest()
    value={'sha':oid,'tree':{'sha':d['tree']},'message':d['message'],'parents':[{'sha':d['parents[]']}]};s['commits'][oid]=value
elif path=='git/refs':
    b=d['ref'].removeprefix('refs/heads/');assert b.startswith('xharness/ticket-');assert b not in s['branches']
    s['branches'][b]=d['sha'];value={'ref':d['ref'],'object':{'sha':d['sha']}}
elif path.startswith('pulls?state=all'):
    q=urllib.parse.parse_qs(urllib.parse.urlsplit(path).query)
    assert q['base']==['main'] and q['head'][0].startswith('owner:xharness/ticket-')
    value=s['pulls']
elif path=='pulls':
    assert ('--field','draft','true') in fields
    assert not s['pulls']
    value={'number':31,'html_url':'https://github.com/owner/repo/pull/31','state':'open','draft':True,'body':d['body'],'head':{'ref':d['head'],'sha':s['branches'][d['head']],'repo':{'full_name':'owner/repo'}},'base':{'ref':d['base'],'repo':{'full_name':'owner/repo'}}};s['pulls'].append(value)
elif path.startswith('pulls/31/reviews?'):value=[{'id':71,'state':'COMMENTED','body':'Fixture review only'}]
elif path.startswith('pulls/31/comments?'):value=[]
elif path.startswith('commits/') and '/check-runs?' in path:value={'total_count':1,'check_runs':[{'name':'fixture check','status':'completed','conclusion':'success'}]}
else:raise AssertionError(args)
state_path.write_text(json.dumps(s))
if path=='git/blobs' and os.environ.get('GH_FIXTURE_DELIVERY_GATE'):
    import time
    (root/'delivery-gate-ready').write_text('ready');deadline=time.monotonic()+20
    while not (root/'delivery-gate-release').exists():
        assert time.monotonic()<deadline,'Delivery gate was not released'
        time.sleep(.01)
print(json.dumps(value))
