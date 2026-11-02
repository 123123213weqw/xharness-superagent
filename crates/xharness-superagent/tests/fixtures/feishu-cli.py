#!/usr/bin/env python3
"""Offline bot CLI boundary only. No Feishu, credentials, network or SDK stub."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
root = Path(os.environ['FEISHU_FIXTURE_ROOT'])
args = sys.argv[1:]
with (root / 'trace.jsonl').open('a') as log:
    log.write(json.dumps(args, ensure_ascii=False) + '\n')
mode = os.environ.get('FEISHU_FIXTURE_MODE', '')
assert os.environ['LARKSUITE_CLI_NO_UPDATE_NOTIFIER'] == '1'
assert os.environ['LARKSUITE_CLI_NO_SKILLS_NOTIFIER'] == '1'
def marker(name, value):
    p = root / (name + '.tmp')
    p.write_text(str(value))
    p.replace(root / name)
if args == ['auth', 'status']:
    if mode == 'auth-block':
        marker('setup-pid', os.getpid())
        child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(120)'])
        marker('setup-child', child.pid)
        time.sleep(120)
    print('fixture status banner\n' + json.dumps({'appId': 'cli_app', 'brand': 'feishu', 'identities': {'bot': {'status': 'ready' if mode != 'unready' else 'missing'}}}))
elif args == ['config', 'strict-mode']:
    print('strict-mode: ' + ('user' if mode == 'user' else 'bot'))
elif args == ['api', 'GET', '/open-apis/bot/v3/info', '--as', 'bot', '--format', 'ndjson']:
    print(json.dumps({'bot': {'open_id': 'ou_bot', 'app_name': '测试机器人 😀'}}))
elif args == ['event', 'consume', 'im.message.receive_v1', '--as', 'bot']:
    marker('consumer-pid', os.getpid())
    if mode == 'exit':
        sys.stderr.write('[event] ready'); sys.stderr.flush()
        os.write(1, json.dumps({'message_id': 'om_final', 'text': '无换行 😀'}, ensure_ascii=False).encode())
        sys.exit(0)
    child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(120)'])
    marker('consumer-child', child.pid)
    if mode == 'unready-consumer':
        time.sleep(120)
        sys.exit(0)
    sys.stderr.write('[event] ready\nWARN fixture diagnostic\n'); sys.stderr.flush()
    os.write(1, b'{bad-json\n')
    if mode == 'oversize':
        for _ in range(17):
            os.write(1, b'x' * 65536)
            time.sleep(0.002)
        os.write(1, b'\n')
    payload = json.dumps({'message_id': 'om_one', 'text': '分段 中文 😀'}, ensure_ascii=False).encode() + b'\n'
    for i in range(0, len(payload), 3):
        os.write(1, payload[i:i+3])
        time.sleep(0.001)
    time.sleep(120)
else:
    assert args[-4:] == ['--as', 'bot', '--format', 'json'], args
    if mode == 'gate':
        marker('rpc-pid', os.getpid())
        deadline = time.monotonic() + 20
        while not (root / 'rpc-release').exists():
            if time.monotonic() > deadline:
                raise RuntimeError('offline policy concurrency gate was not released')
            time.sleep(0.01)
    if mode == 'block':
        marker('rpc-pid', os.getpid())
        child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(120)'])
        marker('rpc-child', child.pid)
        time.sleep(120)
    print((root / 'reply.json').read_text())
