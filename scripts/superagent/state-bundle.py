#!/usr/bin/env python3
"""Operator-only quiesced state/config/workspace bundles; no service side effects.

Stop and JOIN the service and its restart supervisor first. This utility refuses
live WAL sidecars, special files, overlapping paths, existing bundles and silent
content drift. It does not pretend to stop a process or reconcile external APIs.
"""
import argparse, hashlib, json, os, shutil, stat, sys, time
from pathlib import Path

def fingerprint(root):
    result={}
    paths=[root]
    if root.is_dir() and not root.is_symlink():
        paths.extend(p for base,dirs,files in os.walk(root,followlinks=False)
                     for p in [*(Path(base)/n for n in dirs),*(Path(base)/n for n in files)])
    for path in sorted(set(paths)):
        st=path.lstat();key='.' if path==root else path.relative_to(root).as_posix()
        row={'mode':stat.S_IMODE(st.st_mode)}
        if stat.S_ISLNK(st.st_mode):row.update(type='symlink',target=os.readlink(path))
        elif stat.S_ISDIR(st.st_mode):row['type']='directory'
        elif stat.S_ISREG(st.st_mode):
            if path.name.endswith(('.sqlite-wal','.sqlite-shm')):raise ValueError('WAL sidecar present; use a fully joined/checkpointed state')
            h=hashlib.sha256()
            with path.open('rb') as f:
                for block in iter(lambda:f.read(1024*1024),b''):h.update(block)
            row.update(type='file',bytes=st.st_size,sha256=h.hexdigest())
        else:raise ValueError('Special file cannot be snapshotted: '+str(path))
        result[key]=row
    return result

def safe_absolute(path):
    path=Path(path)
    if not path.is_absolute():raise ValueError('Explicit absolute path required')
    path=Path(os.path.abspath(path));path=path.parent.resolve()/path.name
    if path==Path('/') or path==Path.home().resolve():raise ValueError('Non-home/non-root path required')
    return path

def copy(src,dst):
    if src.is_symlink():os.symlink(os.readlink(src),dst);shutil.copystat(src,dst,follow_symlinks=False)
    elif src.is_dir():shutil.copytree(src,dst,symlinks=True,copy_function=shutil.copy2)
    else:shutil.copy2(src,dst,follow_symlinks=False)

def remove(path):
    if path.is_dir() and not path.is_symlink():shutil.rmtree(path)
    else:path.unlink()

def snapshot(paths,bundle):
    bundle=safe_absolute(bundle);paths=[safe_absolute(p) for p in paths]
    if len(set(paths))!=len(paths):raise ValueError('Duplicate paths')
    for p in paths:
        if not p.exists() and not p.is_symlink():raise ValueError('Missing snapshot path: '+str(p))
        if p.is_symlink():raise ValueError('Declare the resolved top-level path, not a symlink')
        for q in [*paths,bundle]:
            if q!=p and (p in q.parents or q in p.parents):raise ValueError('Overlapping snapshot paths/bundle')
    bundle.mkdir(mode=0o700,parents=False,exist_ok=False)
    entries=[]
    try:
        for i,p in enumerate(paths):
            expected=fingerprint(p);target=bundle/('object-'+str(i));copy(p,target)
            if fingerprint(p)!=expected or fingerprint(target)!=expected:raise ValueError('Source changed while copying; no consistent bundle')
            entries.append({'source':str(p),'object':target.name,'fingerprint':expected})
        manifest={'schema':1,'quiesced_asserted_by_operator':True,'entries':entries}
        raw=json.dumps(manifest,sort_keys=True,separators=(',',':')).encode()
        (bundle/'manifest.json').write_bytes(raw);(bundle/'manifest.sha256').write_text(hashlib.sha256(raw).hexdigest()+'\n')
        verify(bundle)
        return {'objects':len(entries),'files':sum(sum(v['type']=='file' for v in e['fingerprint'].values()) for e in entries),'verified':True}
    except BaseException:
        remove(bundle)
        raise

def verify(bundle):
    bundle=safe_absolute(bundle);raw=(bundle/'manifest.json').read_bytes()
    if hashlib.sha256(raw).hexdigest()!=(bundle/'manifest.sha256').read_text().strip():raise ValueError('Manifest checksum mismatch')
    m=json.loads(raw)
    if m.get('schema')!=1 or not m.get('quiesced_asserted_by_operator') or not isinstance(m.get('entries'),list):raise ValueError('Unsupported manifest')
    objects=set()
    for e in m['entries']:
        if e['object'] in objects or e['object']!='object-'+str(len(objects)):raise ValueError('Invalid bundle object')
        objects.add(e['object']);safe_absolute(e['source'])
        if fingerprint(bundle/e['object'])!=e['fingerprint']:raise ValueError('Bundle content differs from manifest')
    if set(p.name for p in bundle.iterdir())!=objects|{'manifest.json','manifest.sha256'}:raise ValueError('Unexpected bundle files')
    return m

def restore(bundle,into=None,replace_originals=False):
    bundle=safe_absolute(bundle);m=verify(bundle)
    if (into is None)==(not replace_originals):raise ValueError('Choose --into or --replace-originals')
    if into is not None:
        into=safe_absolute(into)
        if into.exists():raise ValueError('Relocated restore directory must be new')
        into.mkdir(mode=0o700)
    staged=[];installed=[];quarantine=[]
    try:
        for i,e in enumerate(m['entries']):
            target=(into/e['object']) if into else safe_absolute(e['source'])
            if target==bundle or bundle in target.parents or target in bundle.parents:raise ValueError('Restore overlaps the bundle')
            for old,_,_ in staged:
                if target==old or target in old.parents or old in target.parents:raise ValueError('Overlapping restore targets')
            target.parent.mkdir(parents=True,exist_ok=True)
            stage=target.parent/('.'+target.name+'-restore-'+str(os.getpid())+'-'+str(time.time_ns()))
            staged.append((target,stage,e))
            copy(bundle/e['object'],stage)
            if fingerprint(stage)!=e['fingerprint']:raise ValueError('Staged restore verification failed')
        for target,stage,e in staged:
            old=None
            if target.exists() or target.is_symlink():
                old=target.parent/('.'+target.name+'-before-restore-'+str(os.getpid())+'-'+str(time.time_ns()))
                os.replace(target,old);quarantine.append((target,old))
            os.replace(stage,target);installed.append(target)
            if fingerprint(target)!=e['fingerprint']:raise ValueError('Installed restore verification failed')
        return {'restored':len(installed),'verified':True,'prior_trees_retained':[str(q) for _,q in quarantine]}
    except BaseException:
        for target in reversed(installed):remove(target)
        for target,old in reversed(quarantine):os.replace(old,target)
        for _,stage,_ in staged:
            if stage.exists() or stage.is_symlink():remove(stage)
        if into and into.exists() and not any(into.iterdir()):into.rmdir()
        raise

def main():
    os.umask(0o077)
    p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='command',required=True)
    s=sub.add_parser('snapshot');s.add_argument('--quiesced',action='store_true',required=True);s.add_argument('--path',action='append',required=True);s.add_argument('--bundle',required=True)
    v=sub.add_parser('verify');v.add_argument('--bundle',required=True)
    r=sub.add_parser('restore');r.add_argument('--quiesced',action='store_true',required=True);r.add_argument('--bundle',required=True);g=r.add_mutually_exclusive_group(required=True);g.add_argument('--into');g.add_argument('--replace-originals',action='store_true')
    a=p.parse_args()
    if a.command=='snapshot':result=snapshot(a.path,Path(a.bundle))
    elif a.command=='verify':m=verify(Path(a.bundle));result={'verified':True,'objects':len(m['entries'])}
    else:result=restore(Path(a.bundle),Path(a.into) if a.into else None,a.replace_originals)
    print(json.dumps(result))
if __name__=='__main__':
    try:main()
    except Exception as e:print(str(e),file=sys.stderr);sys.exit(1)
