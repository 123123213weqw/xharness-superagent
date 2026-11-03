#!/usr/bin/env python3
"""Offline operator-controller fault tests, not service or live rollout proof."""
import importlib.util, json, os, tempfile, unittest
from pathlib import Path
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('fleet_control',Path(__file__).with_name('fleet-control.py'))
fleet=importlib.util.module_from_spec(spec);spec.loader.exec_module(fleet)


class Tests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.base=Path(self.temp.name);self.home=self.base/'home';self.home.mkdir(mode=0o700)
        self.root=self.base/'fleet';self.root.mkdir(mode=0o700);(self.root/'.superagent-fleet-owner').write_text('test-fleet')
        self.instance=self.base/'instance';self.instance.mkdir(mode=0o700);(self.instance/'.superagent-goal-owner').write_text('test-instance')
        self.state=self.instance/'state';self.state.mkdir(mode=0o700);(self.state/'orchestrator.sqlite').write_bytes(b'CURRENT_CURSOR')
        self.binary=self.instance/'xharness-superagent';self.binary.write_bytes(b'\x7fELF_previous');self.binary.chmod(0o700)
        self.env=self.instance/'environment';self.env.write_text('DEEPSEEK_API_KEY=offline-fixture\n');self.env.chmod(0o600)
        self.config=self.instance/'config.json';self.config.write_text(json.dumps({'stateDir':str(self.state),'management':{'port':13093},'models':[]}));self.config.chmod(0o600)
        self.unit=self.home/'.config/systemd/user/fixture.service';self.unit.parent.mkdir(parents=True);self.unit.write_text('ExecStart='+str(self.binary)+' --config '+str(self.config)+'\n')
        self.unit_sha=self.instance/'unit.sha256';self.unit_sha.write_text(fleet.sha(self.unit))
        p={'kind':'rust','channel':'canary','port':13093,'root':str(self.instance),'owner':'test-instance','binary':str(self.binary),'config':str(self.config),'state':str(self.state),'environment':str(self.env),'unit':'fixture.service','unitFile':str(self.unit),'unitShaFile':str(self.unit_sha)}
        self.registry=self.root/'registry.json';self.document={'schema':1,'owner':'test-fleet','instances':{'rs':p,'legacy':{'kind':'legacy','port':13094}}};self.save()
        self.home_patch=patch.object(Path,'home',return_value=self.home);self.home_patch.start()
        self.fleet=fleet.Fleet(self.registry);self.new_binary=self.base/'new-binary';self.new_binary.write_bytes(b'\x7fELF_new_release');self.evidence=self.base/'evidence.json';self.evidence.write_text(json.dumps({'testsPassed':4,'testsFailed':0,'strictClippyPassed':True,'sourceFingerprint':'a'*64}))
        self.release=self.fleet.stage(self.new_binary,'b'*40,self.evidence)['id'];self.stopped=False;self.paused=False;self.commands=[];self.busy=False;self.fail_check=False

    def save(self):
        self.registry.write_text(json.dumps(self.document));self.registry.chmod(0o600)

    def tearDown(self):self.home_patch.stop();self.temp.cleanup()

    def api(self,p,path,body=None):
        if path=='executor/pause':self.paused=body['paused'];return {}
        if path=='sources':return [{'id':'github','enabled':True,'state':'ready'}]
        if path=='state':return {'health':{'runtime':{'state':'ready'}},'paused':self.paused,'load':{'running':int(self.busy),'queued':0,'idle':0}}
        raise AssertionError(path)

    def command(self,args,timeout=75,env=None):
        self.commands.append(list(args))
        if args[0]=='systemctl':
            if 'show' in args:return b'0\n' if self.stopped else (str(os.getpid())+'\n').encode()
            if 'stop' in args:self.stopped=True
            if 'start' in args:self.stopped=False
            return b''
        if '--check' in args:
            # A new receipt/cursor after the checkpoint must survive rollback.
            with (self.state/'orchestrator.sqlite').open('ab') as f:f.write(b'_NEW_RECEIPT')
            if self.fail_check:raise RuntimeError('fixture check failure')
            return b'{}'
        raise AssertionError(args)

    def controlled_upgrade(self,config=None):
        original=Path.exists
        original_read=Path.read_bytes
        def read_bytes(path):
            if str(path)=='/proc/'+str(os.getpid())+'/environ':return b'PATH=/usr/bin:/bin\0HOME=/offline-fixture\0'
            return original_read(path)
        def exists(path):
            if str(path)=='/proc/'+str(os.getpid()):return not self.stopped
            return original(path)
        with patch.object(fleet,'api',side_effect=self.api),patch.object(fleet,'command',side_effect=self.command),patch.object(Path,'exists',exists),patch.object(Path,'read_bytes',read_bytes):
            return self.fleet.upgrade('rs',self.release,config)

    def test_stage_is_immutable_and_detects_binary_drift(self):
        self.assertTrue(self.fleet.stage(self.new_binary,'b'*40,self.evidence)['existing'])
        artifact,_=self.fleet.release(self.release);(artifact/'xharness-superagent').chmod(0o600);(artifact/'xharness-superagent').write_bytes(b'drift')
        with self.assertRaisesRegex(ValueError,'drift'):self.fleet.release(self.release)
        with self.assertRaises(ValueError):self.fleet.release('..')

    def test_bad_or_failed_evidence_cannot_stage(self):
        for facts in [{'testsFailed':1,'testsPassed':1,'strictClippyPassed':True,'sourceFingerprint':'a'*64},{'testsFailed':0,'testsPassed':0,'strictClippyPassed':True,'sourceFingerprint':'a'*64},{'testsFailed':0,'testsPassed':1,'strictClippyPassed':False,'sourceFingerprint':'a'*64}]:
            self.evidence.write_text(json.dumps(facts))
            with self.assertRaises(ValueError):self.fleet.stage(self.new_binary,'b'*40,self.evidence)

    def test_legacy_duplicate_state_port_and_foreign_unit_are_rejected(self):
        with self.assertRaisesRegex(ValueError,'cold migration'):self.fleet.profile('legacy')
        self.document['instances']['duplicate']=dict(self.document['instances']['rs']);self.save()
        with self.assertRaisesRegex(ValueError,'port'):fleet.Fleet(self.registry)
        del self.document['instances']['duplicate'];self.unit.write_text('ExecStart=/other/bin --config /other/config\n');self.save()
        with patch.object(fleet,'api',side_effect=self.api):
            with self.assertRaisesRegex(ValueError,'Unit changed'):self.fleet.plan('rs',self.release)

    def test_plan_cannot_change_state_models_accounts_or_sources(self):
        for key,value in [('stateDir','/tmp/other-state'),('models',[{'model':'other-model'}]),('settings',{'sources':{'github':{'config':{'repository':'other/repo'}}}}),('repositoryDelivery',{'baseBranch':'changed'})]:
            cfg=json.loads(self.config.read_text());cfg[key]=value;candidate=self.instance/'next.json';candidate.write_text(json.dumps(cfg))
            with patch.object(fleet,'api',side_effect=self.api):
                with self.assertRaises(ValueError):self.fleet.plan('rs',self.release,candidate)
        self.fleet.instances['rs']['channel']='stable'
        with self.assertRaisesRegex(ValueError,'promotion'):self.fleet.plan('rs',self.release)

    def test_workspace_config_update_preserves_accounts_and_state(self):
        cfg=json.loads(self.config.read_text());cfg['workspace']={'checkout':{'enabled':True,'cache':True,'revision':{'kind':'auto'}}}
        candidate=self.instance/'workspace-next.json';candidate.write_text(json.dumps(cfg))
        result=self.controlled_upgrade(candidate)
        self.assertEqual(result['status'],'deployed');self.assertEqual(json.loads(self.config.read_text())['workspace'],cfg['workspace'])
        self.assertEqual((self.state/'orchestrator.sqlite').read_bytes(),b'CURRENT_CURSOR_NEW_RECEIPT')

    def test_busy_instance_is_deferred_without_pause_or_service_stop(self):
        self.busy=True
        with self.assertRaisesRegex(ValueError,'Deferred'):self.controlled_upgrade()
        self.assertFalse(self.commands);self.assertFalse(self.paused)

    def test_work_arriving_during_pause_does_not_get_stopped(self):
        original=self.api
        def api(p,path,body=None):
            result=original(p,path,body)
            if path=='executor/pause' and body['paused']:self.busy=True
            return result
        self.api=api
        with self.assertRaisesRegex(ValueError,'arrived'):self.controlled_upgrade()
        self.assertFalse(any('stop' in c for c in self.commands));self.assertFalse(self.paused)

    def test_upgrade_preserves_state_private_config_unit_and_records_release(self):
        result=self.controlled_upgrade();self.assertEqual(result['status'],'deployed');self.assertFalse(self.paused)
        self.assertEqual(fleet.sha(self.binary),fleet.sha(self.new_binary));self.assertEqual((self.state/'orchestrator.sqlite').read_bytes(),b'CURRENT_CURSOR_NEW_RECEIPT')
        self.assertEqual(fleet.sha(self.unit),self.unit_sha.read_text());self.assertTrue((self.instance/'current-release.json').is_file())
        self.assertEqual(self.env.stat().st_mode&0o777,0o600)
        with patch.object(fleet,'api',side_effect=self.api):self.assertTrue(self.fleet.plan('rs',self.release)['noOp'])

    def test_failed_candidate_restores_binary_not_old_cursor(self):
        old=self.binary.read_bytes();self.fail_check=True
        with self.assertRaisesRegex(RuntimeError,'fixture check failure'):self.controlled_upgrade()
        self.assertEqual(self.binary.read_bytes(),old);self.assertEqual((self.state/'orchestrator.sqlite').read_bytes(),b'CURRENT_CURSOR_NEW_RECEIPT');self.assertFalse(self.paused);self.assertFalse(self.stopped)
        self.assertFalse((self.instance/'current-release.json').exists())

    def test_incident_is_version_bound_and_does_not_export_secrets_or_chat(self):
        with patch.object(fleet,'api',side_effect=self.api):result=self.fleet.incident('rs','fixture validation failure')
        serialized=json.dumps(result);self.assertIn('binarySha256',result);self.assertNotIn('offline-fixture',serialized);self.assertNotIn('text',result)


if __name__=='__main__':unittest.main()
