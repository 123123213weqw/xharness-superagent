#!/usr/bin/env python3
"""Operator-only GitHub route preparation for an existing local Mihomo binary.

No tokens, repositories, model traffic, proxy subscription changes, TLS bypass,
Git objects or laptop tunnel. Only fresh official endpoint data is persisted.
The separate owned user unit executes this before launching its loopback proxy.
"""
import argparse, concurrent.futures, hashlib, ipaddress, json, os, socket, ssl, time, urllib.request
from pathlib import Path


def private(path):
    path=Path(path)
    if path.is_symlink() or path.stat().st_uid!=os.getuid() or path.stat().st_mode&0o077:
        raise ValueError('Owned private regular profile/directory required')
    return path


def atomic(path,value):
    path=Path(path);tmp=path.with_name('.'+path.name+'.next')
    with tmp.open('x') as f:
        os.chmod(tmp,0o600);f.write(value);f.flush();os.fsync(f.fileno())
    os.replace(tmp,path)


def official_candidates():
    # Never read a proxy subscription, credential store or repository contents.
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}),urllib.request.HTTPSHandler(context=ssl.create_default_context()))
    request=urllib.request.Request('https://api.github.com/meta',headers={'User-Agent':'xharness-git-route-operator','Accept':'application/vnd.github+json'})
    for attempt in range(3):
        try:
            with opener.open(request,timeout=12) as response:raw=response.read(1048577)
            if len(raw)>1048576:raise ValueError('Oversized GitHub metadata')
            data=json.loads(raw);break
        except (OSError,ValueError):
            if attempt==2:raise
            time.sleep(.5*2**attempt)
    result=[]
    for value in data['web']:
        network=ipaddress.ip_network(value)
        if network.version==4 and network.prefixlen==32 and network.network_address.is_global:
            result.append(str(network.network_address))
    if not result or len(result)>128:raise ValueError('Invalid official GitHub endpoint set')
    return sorted(set(result)),hashlib.sha256(raw).hexdigest()


def probe(address):
    started=time.monotonic()
    try:
        with socket.create_connection((address,443),timeout=3) as connection:
            # SNI and certificate validation remain github.com, not the IP.
            with ssl.create_default_context().wrap_socket(connection,server_hostname='github.com') as stream:
                stream.settimeout(3);stream.sendall(b'HEAD / HTTP/1.1\r\nHost: github.com\r\nConnection: close\r\n\r\n')
                first=stream.recv(128).split(b'\r\n',1)[0]
                if first.split(b' ')[1:2]!=[b'200']:return None
        return {'address':address,'elapsedMs':round((time.monotonic()-started)*1000)}
    except (OSError,ValueError):return None


def prepare(profile):
    profile=private(profile);root=private(profile.parent);document=json.loads(profile.read_text())
    if document.get('schema')!=1 or (root/'.superagent-goal-owner').read_text().strip()!=document['owner']:
        raise ValueError('Route ownership mismatch')
    if set(document)!={'schema','owner','listenPort','mihomoBinary'}:raise ValueError('Unexpected route profile fields')
    port=document['listenPort'];binary=Path(document['mihomoBinary'])
    if isinstance(port,bool) or not isinstance(port,int) or not 1024<=port<=65535:raise ValueError('Explicit unprivileged loopback port required')
    if not binary.is_absolute() or not binary.is_file() or not os.access(binary,os.X_OK):raise ValueError('Existing absolute Mihomo binary required')
    candidates,metadata_sha=official_candidates()
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        healthy=sorted((row for row in pool.map(probe,candidates) if row),key=lambda row:row['elapsedMs'])
    if not healthy:raise ValueError('No TLS-verified official GitHub endpoint is currently reachable')
    # Prefer the fastest currently healthy endpoint. Refresh on this owned
    # helper's restart; no provider address is embedded in source or unit.
    selected=[healthy[0]['address']]
    config={'mixed-port':port,'bind-address':'127.0.0.1','allow-lan':False,'mode':'rule','log-level':'warning','ipv6':False,'external-controller':'','hosts':{'github.com':selected},'dns':{'enable':False},'rules':['DOMAIN,github.com,DIRECT','MATCH,REJECT']}
    atomic(root/'route.yaml',json.dumps(config,indent=2)+'\n')
    atomic(root/'git-network.config',f'[http "https://github.com"]\nproxy = http://127.0.0.1:{port}\n[http]\nversion = HTTP/1.1\n')
    receipt={'generatedAt':time.time(),'officialMetadataSha256':metadata_sha,'candidatesProbed':len(candidates),'healthyEndpoints':healthy,'selected':selected,'bind':'127.0.0.1','tlsVerified':True,'githubOnly':True,'requiresLaptopTunnel':False,'sharedProxyChanged':False}
    atomic(root/'route-health.json',json.dumps(receipt,indent=2)+'\n');return receipt


if __name__=='__main__':
    os.umask(0o077);parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--profile',required=True);args=parser.parse_args()
    try:print(json.dumps(prepare(args.profile)))
    except (OSError,ValueError,KeyError) as error:raise SystemExit(str(error))
