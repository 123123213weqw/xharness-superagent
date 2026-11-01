"""Fetch exact supplemental oracle artifacts; no install scripts or compilation.
Usage: python3 conformance/reference/fetch-full-agent-loop.py NEW_CACHE_DIR
Existing destinations are refused. All HTTP and tar/file hashes are verified.
"""
from pathlib import Path
import base64,hashlib,io,json,sys,tarfile,time,urllib.request
lock=json.loads(Path(__file__).with_name('full-agent-loop-source.json').read_text())
root=Path(sys.argv[1]).resolve();root.mkdir(parents=True,exist_ok=False)
def fetch(url):
 if not url.startswith('https://registry.npmjs.org/'):raise ValueError('Non-registry URL')
 last=None
 for delay in (0,1,3):
  if delay:time.sleep(delay)
  try:
   with urllib.request.urlopen(url,timeout=30)as response:
    if not response.geturl().startswith('https://registry.npmjs.org/'):raise ValueError('Non-registry redirect')
    data=response.read(8*1024*1024+1)
    if len(data)>8*1024*1024:raise ValueError('Artifact exceeds bound')
    return data
  except OSError as error:last=error
 raise last
for entry in lock['packages']:
 name=entry['cache_name'];data=fetch(entry['tarball'])
 if hashlib.sha256(data).hexdigest()!=entry['tarball_sha256']or 'sha512-'+base64.b64encode(hashlib.sha512(data).digest()).decode()!=entry['integrity']:raise ValueError('Artifact integrity mismatch: '+name)
 folder=root/name;folder.mkdir()
 with tarfile.open(fileobj=io.BytesIO(data),mode='r:gz')as archive:
  for member in archive.getmembers():
   if not (member.isfile()or member.isdir())or not(folder/member.name).resolve().is_relative_to(folder.resolve()):raise ValueError('Invalid tar entry')
  archive.extractall(folder)
 package=folder/'package'
 actual={str(p.relative_to(package)):hashlib.sha256(p.read_bytes()).hexdigest()for p in package.rglob('*')if p.is_file()}
 if actual!=entry['files']:raise ValueError('Extracted tree mismatch: '+name)
 (root/(name+'.tgz')).write_bytes(data)
 print(name,entry['version'],entry['tarball_sha256'],flush=True)
# The official flock entry resolves its optional prebuilt through Node require;
# this lab-local link is the only dependency materialization, not npm install.
parent=root/'node-addon-system/package/node_modules/@deepseek-ai';parent.mkdir(parents=True)
(parent/'node-addon-system-linux-x64').symlink_to('../../../../node-addon-system-linux-x64/package',target_is_directory=True)
