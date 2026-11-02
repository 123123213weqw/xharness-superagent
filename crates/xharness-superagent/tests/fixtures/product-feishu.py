#!/usr/bin/env python3
"""Explicit offline Feishu CLI endpoints for the real product. No SDK stub/network."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
root = Path(os.environ['FEISHU_PRODUCT_ROOT'])
args = sys.argv[1:]
assert os.environ['LARKSUITE_CLI_NO_UPDATE_NOTIFIER'] == '1'
assert os.environ['LARKSUITE_CLI_NO_SKILLS_NOTIFIER'] == '1'
with (root/'calls.jsonl').open('a') as log:
    log.write(json.dumps(args, ensure_ascii=False)+'\n')
def atomic(name, data):
    file = root/(name+'.'+str(os.getpid())+'.tmp')
    file.write_text(data)
    file.replace(root/name)
def result(data):
    print(json.dumps({'ok':True,'data':data}, ensure_ascii=False))
def messages():
    return json.loads((root/'messages.json').read_text())
if args == ['auth','status']:
    print(json.dumps({'appId':'cli_app','brand':'feishu','identities':{'bot':{'status':'ready'}}}))
    sys.exit(0)
if args == ['config','strict-mode']:
    print('strict-mode: bot');sys.exit(0)
if args == ['api','GET','/open-apis/bot/v3/info','--as','bot','--format','ndjson']:
    print(json.dumps({'bot':{'open_id':'ou_bot','app_name':'飞书机器人 😀'}},ensure_ascii=False));sys.exit(0)
if args == ['event','consume','im.message.receive_v1','--as','bot']:
    count = int((root/'consumer-count').read_text())+1 if (root/'consumer-count').exists() else 1
    atomic('consumer-count',str(count));atomic('consumer-pid',str(os.getpid()))
    with (root/'consumer-times.jsonl').open('a') as f:f.write(json.dumps(time.monotonic())+'\n')
    sys.stderr.write('[event] ready\n');sys.stderr.flush()
    if (root/'consumer-exit-once').exists() and count == 1:
        sys.exit(7)
    child = subprocess.Popen([sys.executable,'-c','import time; time.sleep(300)'])
    atomic('consumer-child',str(child.pid))
    emitted = 0
    deadline = time.monotonic()+180
    while time.monotonic()<deadline:
        events = json.loads((root/'events.json').read_text())
        while emitted < len(events):
            event = events[emitted];emitted += 1
            line = (event if isinstance(event,str) else json.dumps(event,ensure_ascii=False))+'\n'
            raw = line.encode()
            for i in range(0,len(raw),7):
                os.write(1,raw[i:i+7])
                time.sleep(0.0002)
        time.sleep(0.02)
    sys.exit(0)
assert args[-4:] == ['--as','bot','--format','json'],args
args = args[:-4]
if args == ['im','+chat-list','--page-all','--page-size','100']:
    result({'chats':[{'chat_id':'oc_chat','name':'产品测试群 😀'}]})
elif args[:2] == ['im','+messages-reply']:
    assert args[2] == '--message-id' and args[4] == '--markdown' and args[6:8] == ['--reply-in-thread','--idempotency-key'] and len(args)==9,args
    path = root/'posts.json'
    rows = json.loads(path.read_text()) if path.exists() else []
    receipt = args[8]
    if not any(row['id']==receipt for row in rows):
        rows.append({'id':receipt,'message_id':args[3],'text':args[5]})
    atomic('posts.json',json.dumps(rows,ensure_ascii=False))
    result({'message_id':'om_reply'+str(len(rows))})
elif args[:2] == ['im','+messages-mget']:
    assert args[2] == '--message-ids' and args[4:] == ['--no-reactions'],args
    result({'messages':[m for m in messages() if m['message_id']==args[3]]})
elif args[:3] == ['api','GET','/open-apis/im/v1/messages/mget']:
    assert args[3]=='--params' and len(args)==5,args
    ids=json.loads(args[4])['message_ids']
    fields=['message_id','chat_id','root_id','parent_id','thread_id','deleted']
    result({'items':[{k:v for k,v in m.items() if k in fields} for m in messages() if m['message_id'] in ids]})
elif args[:2] == ['im','+threads-messages-list']:
    assert args[2]=='--thread' and args[4]=='--order' and args[6]=='--page-size' and args[8]=='--no-reactions',args
    assert len(args)==9 or (len(args)==11 and args[9]=='--page-token'),args
    values=[m for m in messages() if m.get('thread_id')==args[3]]
    values.sort(key=lambda m:m['create_time'],reverse=args[5]=='desc')
    result({'messages':values,'has_more':False,'thread_id':args[3]})
elif args[:3] == ['api','GET','/open-apis/im/v1/messages']:
    assert args[3]=='--params' and len(args)==5,args
    params=json.loads(args[4]);assert params['container_id_type'] in ['chat','thread']
    history=root/'history.json'
    pages=json.loads(history.read_text()) if history.exists() else {}
    key=params['container_id']+':'+params.get('page_token','')
    result(pages.get(key,{'items':[],'has_more':False}))
elif args == ['im','+chat-members-list','--chat-id','oc_chat','--member-types','user','--page-all','--page-limit','0']:
    result({'users':[{'member_id':'ou_alice','name':'Alice'}],'has_more':False})
else:
    print('Unsupported offline product Feishu request: '+repr(args),file=sys.stderr);sys.exit(2)
