#!/usr/bin/env python3
"""Operator-only immutable releases and serialized, state-preserving upgrades.

No remote shell, dependency install, legacy migration, automatic rollout or DB
rollback. The registry contains file references, never credential values.
"""
import argparse, fcntl, hashlib, importlib.util, json, os, re, shlex, shutil, subprocess, sys, time, urllib.error, urllib.request, uuid
from pathlib import Path


def sha(path):
    h=hashlib.sha256()
    with Path(path).open('rb') as f:
        for block in iter(lambda:f.read(1048576),b''): h.update(block)
    return h.hexdigest()


def private(path):
    path=Path(path)
    if path.is_symlink() or path.stat().st_uid!=os.getuid() or path.stat().st_mode&0o077:
        raise ValueError('Operator file/directory must be owned, private and not a symlink: '+str(path))
    return path


def atomic(path,value):
    path=Path(path);tmp=path.with_name('.'+path.name+'.'+uuid.uuid4().hex)
    with tmp.open('x') as f:
        os.chmod(tmp,0o600);json.dump(value,f,ensure_ascii=False,indent=2);f.write('\n');f.flush();os.fsync(f.fileno())
    os.replace(tmp,path)


def api(profile,path,body=None):
    req=urllib.request.Request(f'http://127.0.0.1:{profile["port"]}/orchestrator/api/'+path,
        headers={'x-orchestrator-request':'1','Content-Type':'application/json'},
        data=None if body is None else json.dumps(body).encode())
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req,timeout=15) as response:return json.load(response)
        except urllib.error.HTTPError as error:
            if error.code not in [408,429] and error.code<500:raise
            if attempt==2:raise
        except (urllib.error.URLError,TimeoutError):
            if attempt==2:raise
        time.sleep(.5*(2**attempt))


def command(args,timeout=75,env=None):
    result=subprocess.run([str(a) for a in args],capture_output=True,timeout=timeout,env=env)
    if result.returncode:raise RuntimeError('Command failed: '+str(args[0])+', exit='+str(result.returncode))
    return result.stdout


class Fleet:
    def __init__(self,registry):
        self.path=private(registry);self.root=private(self.path.parent)
        self.data=json.loads(self.path.read_text())
        if self.data.get('schema')!=1:raise ValueError('Unsupported fleet registry')
        if (self.root/'.superagent-fleet-owner').read_text().strip()!=self.data['owner']:raise ValueError('Fleet ownership mismatch')
        instances=self.data['instances'];ports=set();states=set();units=set()
        for name,p in instances.items():
            if not re.fullmatch(r'[a-zA-Z0-9_-]{1,64}',name):raise ValueError('Invalid instance name')
            if p['kind'] not in ['rust','legacy']:raise ValueError('Unknown implementation')
            if not 1024<=p['port']<=65535 or p['port'] in ports:raise ValueError('Duplicate/invalid management port')
            ports.add(p['port'])
            if p['kind']=='legacy':continue
            for key in ['root','binary','config','state','environment','unitFile','unitShaFile']:
                if not Path(p[key]).is_absolute():raise ValueError('Absolute path required: '+key)
            root=Path(p['root']).resolve()
            if root in [Path('/'),Path.home().resolve()]:raise ValueError('Dedicated instance root required')
            if (root/'.superagent-goal-owner').read_text().strip()!=p['owner']:raise ValueError('Instance ownership mismatch')
            for key in ['binary','config','state','environment','unitShaFile']:
                path=Path(p[key]);resolved=path.resolve()
                if root not in resolved.parents or path.is_symlink():raise ValueError('Instance path escapes root or is a symlink: '+key)
            state=Path(p['state']).resolve()
            if state in states or p['unit'] in units:raise ValueError('Instances cannot share state or unit')
            states.add(state);units.add(p['unit'])
            if not re.fullmatch(r'[A-Za-z0-9_@.-]+\.service',p['unit']):raise ValueError('Invalid unit name')
            expected=Path.home()/'.config/systemd/user'/p['unit']
            if Path(p['unitFile'])!=expected:raise ValueError('Unexpected user unit path')
        self.instances=instances

    def profile(self,name):
        p=self.instances[name]
        if p['kind']!='rust':raise ValueError('Legacy profile requires explicit cold migration before Rust binary updates')
        return p

    def owned_unit(self,p):
        unit=Path(p['unitFile'])
        if sha(unit)!=Path(p['unitShaFile']).read_text().strip():raise ValueError('Unit changed outside fleet ownership')
        starts=[line.split('=',1)[1] for line in unit.read_text().splitlines() if line.startswith('ExecStart=')]
        if len(starts)!=1:raise ValueError('One exact owned ExecStart required')
        args=shlex.split(starts[0])
        if len(args)!=3 or args[1]!='--config' or Path(args[0]).resolve()!=Path(p['binary']).resolve() or Path(args[2]).resolve()!=Path(p['config']).resolve():raise ValueError('Unit executable/config mismatch')

    def release(self,release):
        path=self.root/'releases'/release
        if release in ['.','..'] or not re.fullmatch(r'[a-zA-Z0-9_.-]{1,100}',release) or not path.is_dir() or path.is_symlink() or path.resolve().parent!=(self.root/'releases').resolve():raise ValueError('Unknown release')
        manifest=json.loads((path/'release.json').read_text())
        if manifest.get('id')!=release or manifest.get('stateSchema')!=11 or manifest.get('rollbackCompatibility')!='same-schema-and-events':raise ValueError('Unsupported release compatibility')
        if sha(path/'xharness-superagent')!=manifest['binarySha256']:raise ValueError('Release binary drift')
        if sha(path/'evidence.json')!=manifest['evidenceSha256']:raise ValueError('Release evidence drift')
        return path,manifest

    def stage(self,binary,commit,evidence):
        if not re.fullmatch('[0-9a-f]{40}',commit):raise ValueError('Exact source commit required')
        evidence=Path(evidence);facts=json.loads(evidence.read_text())
        if facts.get('testsFailed')!=0 or facts.get('testsPassed',0)<1 or facts.get('strictClippyPassed') is not True or not re.fullmatch('[0-9a-f]{64}',facts.get('sourceFingerprint','')):raise ValueError('Passing release evidence required')
        binary=Path(binary)
        with binary.open('rb') as f:
            if f.read(4)!=b'\x7fELF':raise ValueError('Remotely built Linux ELF binary required')
        digest=sha(binary);name=commit[:12]+'-'+digest[:12];releases=self.root/'releases';releases.mkdir(mode=0o700,exist_ok=True)
        dest=releases/name
        if dest.exists():self.release(name);return {'release':name,'existing':True}
        stage=releases/('.stage-'+uuid.uuid4().hex);stage.mkdir(mode=0o700)
        try:
            shutil.copy2(binary,stage/'xharness-superagent');os.chmod(stage/'xharness-superagent',0o500)
            shutil.copyfile(evidence,stage/'evidence.json');os.chmod(stage/'evidence.json',0o400)
            manifest={'schema':1,'id':name,'sourceCommit':commit,'sourceFingerprint':facts['sourceFingerprint'],'binarySha256':digest,'evidenceSha256':sha(stage/'evidence.json'),'stateSchema':11,'rollbackCompatibility':'same-schema-and-events','createdAt':time.time()}
            atomic(stage/'release.json',manifest)
            if sha(stage/'xharness-superagent')!=digest:raise ValueError('Artifact changed while staging')
            os.rename(stage,dest)
            return manifest
        except BaseException:
            if stage.exists():shutil.rmtree(stage)
            raise

    def inventory(self):
        rows=[]
        for name,p in self.instances.items():
            row={'instance':name,'kind':p['kind'],'channel':p.get('channel'),'port':p['port']}
            try:
                state=api(p,'state');row.update(runtime=state.get('health',{}).get('runtime'),load=state.get('load'),paused=state.get('paused'))
                if p['kind']=='rust':
                    self.owned_unit(p);row['binarySha256']=sha(p['binary']);record=Path(p['root'])/'current-release.json';row['release']=json.loads(record.read_text()).get('release') if record.exists() else 'unregistered-current-binary'
            except (OSError,ValueError,RuntimeError) as error:row['error']=str(error)
            rows.append(row)
        return rows

    def plan(self,name,release,config=None):
        p=self.profile(name);self.owned_unit(p);_,m=self.release(release)
        candidate=Path(config or p['config']);cfg=json.loads(candidate.read_text())
        if Path(cfg['stateDir']).resolve()!=Path(p['state']).resolve() or cfg.get('management',{}).get('port')!=p['port']:raise ValueError('Upgrade cannot change state ownership or management port')
        current=json.loads(Path(p['config']).read_text())
        # Model/account/source changes belong to their own explicit migration.
        if {k:v for k,v in cfg.items() if k not in ['webSearch','workspace']}!={k:v for k,v in current.items() if k not in ['webSearch','workspace']}:raise ValueError('This rolling updater permits only binary, webSearch and workspace configuration changes')
        if p.get('channel')!='canary' and not (self.root/'promotions'/(release+'.json')).is_file():raise ValueError('Stable rollout requires accepted canary promotion')
        state=api(p,'state');busy=any(state.get('load',{}).get(k,0) for k in ['running','queued','idle'])
        return {'instance':name,'release':release,'binarySha256':m['binarySha256'],'busy':bool(busy),'noOp':sha(p['binary'])==m['binarySha256'] and candidate.read_bytes()==Path(p['config']).read_bytes(),'previousPaused':state['paused'],'ready':state['health']['runtime']['state']=='ready'}

    def wait_ready(self,p,source_ids=()):
        end=time.monotonic()+100
        while time.monotonic()<end:
            try:
                state=api(p,'state');sources=api(p,'sources')
                ready={s['id'] for s in sources if s['state']=='ready'}
                if state['health']['runtime']['state']=='ready' and set(source_ids)<=ready:return state
            except (OSError,ValueError):pass
            time.sleep(.5)
        raise RuntimeError('Readiness deadline exceeded')

    def upgrade(self,name,release,config=None):
        with (self.root/'.fleet-upgrade.lock').open('a') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            plan=self.plan(name,release,config)
            if plan['noOp']:return plan
            if plan['busy'] or not plan['ready']:raise ValueError('Deferred: instance has work or is not ready; no task was stopped')
            p=self.profile(name);artifact,manifest=self.release(release)
            update=Path(p['root'])/('update-'+str(time.time_ns()));update.mkdir(mode=0o700)
            next_config=Path(config or p['config']).read_bytes()
            old_release=Path(p['root'])/'current-release.json'
            enabled=[s['id'] for s in api(p,'sources') if s.get('enabled') and s['state']=='ready']
            old_pid=int(command(['systemctl','--user','show',p['unit'],'-p','MainPID','--value']).strip())
            env=dict(x.split(b'=',1) for x in Path('/proc',str(old_pid),'environ').read_bytes().split(b'\0') if b'=' in x)
            env={k.decode():v.decode() for k,v in env.items() if k not in [b'NOTIFY_SOCKET',b'WATCHDOG_PID',b'WATCHDOG_USEC',b'JOURNAL_STREAM',b'INVOCATION_ID']}
            installed=False;activation_attempted=False;stop_attempted=False
            try:
                api(p,'executor/pause',{'paused':True})
                if any(api(p,'state')['load'][k] for k in ['running','queued','idle']):raise ValueError('Work arrived during preflight; upgrade deferred')
                stop_attempted=True
                command(['systemctl','--user','stop',p['unit']],timeout=65)
                if command(['systemctl','--user','show',p['unit'],'-p','MainPID','--value']).strip()!=b'0' or Path('/proc',str(old_pid)).exists():raise ValueError('Old service process not joined')
                spec=importlib.util.spec_from_file_location('fleet_bundle',Path(__file__).with_name('state-bundle.py'));bundle=importlib.util.module_from_spec(spec);spec.loader.exec_module(bundle)
                backup=bundle.snapshot([Path(p[k]) for k in ['state','config','environment','unitFile','binary']],update/'current-state-bundle')
                atomic(update/'backup-verification.json',backup)
                shutil.copy2(p['binary'],update/'previous-binary');shutil.copy2(p['config'],update/'previous-config.json')
                candidate=Path(p['binary']).with_name('.fleet-binary-'+uuid.uuid4().hex);shutil.copy2(artifact/'xharness-superagent',candidate);os.chmod(candidate,0o700);os.replace(candidate,p['binary']);installed=True
                target=Path(p['config']);temp=target.with_name('.fleet-config-'+uuid.uuid4().hex);temp.write_bytes(next_config);os.chmod(temp,0o600);os.replace(temp,target)
                checked=command([p['binary'],'--config',p['config'],'--check'],env=env,timeout=100);(update/'check-private.log').write_bytes(checked)
                command(['systemctl','--user','start',p['unit']]);self.wait_ready(p,enabled)
                record={**plan,'previousPaused':plan['previousPaused'],'installedAt':time.time(),'sourceCommit':manifest['sourceCommit'],'backupVerified':True,'statePreserved':True,'unitUnchanged':True,'status':'ready-before-activation'}
                atomic(update/'deployment.json',record)
                activation_attempted=True;api(p,'executor/pause',{'paused':plan['previousPaused']})
                record['status']='deployed';atomic(old_release,record);atomic(update/'deployment.json',record)
                return record
            except BaseException:
                if not activation_attempted and not stop_attempted:
                    api(p,'executor/pause',{'paused':plan['previousPaused']})
                    atomic(update/'failure.json',{'status':'deferred-before-stop','at':time.time(),'oldDatabaseRestored':False})
                elif not activation_attempted:
                    command(['systemctl','--user','stop',p['unit']],timeout=65)
                    if installed:
                        for key,source in [('binary','previous-binary'),('config','previous-config.json')]:
                            target=Path(p[key]);tmp=target.with_name('.fleet-rollback-'+uuid.uuid4().hex);shutil.copy2(update/source,tmp);os.replace(tmp,target)
                    command(['systemctl','--user','start',p['unit']]);self.wait_ready(p)
                    api(p,'executor/pause',{'paused':plan['previousPaused']})
                    atomic(update/'failure.json',{'status':'previous-binary-restored-current-state-preserved','at':time.time(),'oldDatabaseRestored':False})
                else:
                    atomic(update/'failure.json',{'status':'activation-attempted-needs-current-state-reconciliation','at':time.time(),'oldDatabaseRestored':False})
                raise


    def incident(self,name,summary):
        p=self.instances[name]
        if not summary.strip() or len(summary)>1000:raise ValueError('A concise operator incident summary is required')
        row={'id':str(time.time_ns())+'-'+name,'instance':name,'summary':summary,'at':time.time(),'kind':p['kind'],'port':p['port']}
        if p['kind']=='rust':
            row['binarySha256']=sha(p['binary']);record=Path(p['root'])/'current-release.json'
            row['release']=json.loads(record.read_text()).get('release') if record.exists() else None
        try:
            state=api(p,'state');row['runtime']=state.get('health',{}).get('runtime');row['load']=state.get('load');row['paused']=state.get('paused')
        except (OSError,ValueError) as error:row['healthError']=type(error).__name__
        directory=self.root/'incidents';directory.mkdir(mode=0o700,exist_ok=True);atomic(directory/(row['id']+'.json'),row)
        return row


def main():
    os.umask(0o077);parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--registry',required=True);sub=parser.add_subparsers(dest='action',required=True)
    sub.add_parser('inventory')
    incident=sub.add_parser('incident');incident.add_argument('--instance',required=True);incident.add_argument('--summary',required=True)
    stage=sub.add_parser('stage');stage.add_argument('--binary',required=True);stage.add_argument('--source-commit',required=True);stage.add_argument('--evidence',required=True)
    for action in ['plan','upgrade']:
        p=sub.add_parser(action);p.add_argument('--instance',required=True);p.add_argument('--release',required=True);p.add_argument('--config')
    p=sub.add_parser('promote');p.add_argument('--release',required=True);p.add_argument('--instance',required=True);p.add_argument('--acceptance',required=True)
    args=parser.parse_args();fleet=Fleet(args.registry)
    if args.action=='inventory':result=fleet.inventory()
    elif args.action=='incident':result=fleet.incident(args.instance,args.summary)
    elif args.action=='stage':result=fleet.stage(args.binary,args.source_commit,args.evidence)
    elif args.action in ['plan','upgrade']:result=getattr(fleet,args.action)(args.instance,args.release,args.config)
    else:
        p=fleet.profile(args.instance);artifact,manifest=fleet.release(args.release);acceptance=json.loads(Path(args.acceptance).read_text());record=json.loads((Path(p['root'])/'current-release.json').read_text())
        if p.get('channel')!='canary' or record['release']!=args.release or acceptance.get('accepted') is not True or acceptance.get('release')!=args.release:raise ValueError('Explicit evidence for the currently deployed canary required')
        fleet.wait_ready(p)
        pid=int(command(['systemctl','--user','show',p['unit'],'-p','MainPID','--value']).strip())
        if sha(p['binary'])!=manifest['binarySha256'] or sha(Path('/proc',str(pid),'exe'))!=manifest['binarySha256']:raise ValueError('Canary process does not match release artifact')
        dest=fleet.root/'promotions';dest.mkdir(mode=0o700,exist_ok=True);result={'release':args.release,'canaryInstance':args.instance,'acceptedAt':time.time(),'acceptanceSha256':sha(args.acceptance)};atomic(dest/(args.release+'.json'),result)
    print(json.dumps(result,ensure_ascii=False,indent=2))


if __name__=='__main__':
    try:main()
    except (OSError,ValueError,RuntimeError,KeyError,subprocess.TimeoutExpired) as error:
        print(json.dumps({'error':str(error),'oldDatabaseRestored':False},ensure_ascii=False),file=sys.stderr);raise SystemExit(1)
