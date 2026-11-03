#!/usr/bin/env python3
"""Offline configuration/ownership tests; not live endpoint acceptance."""
import importlib.util,json,os,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('route',Path(__file__).with_name('git-route-config.py'));route=importlib.util.module_from_spec(spec);spec.loader.exec_module(route)
class Tests(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name);self.root.chmod(0o700);(self.root/'.superagent-goal-owner').write_text('owned-route');self.binary=self.root/'existing-mihomo';self.binary.write_text('fixture');self.binary.chmod(0o700);self.profile=self.root/'profile.json';self.data={'schema':1,'owner':'owned-route','listenPort':13219,'mihomoBinary':str(self.binary)};self.save()
 def save(self):self.profile.write_text(json.dumps(self.data));self.profile.chmod(0o600)
 def tearDown(self):self.temp.cleanup()
 def prepare(self,probe=None):
  with patch.object(route,'official_candidates',return_value=(['1.1.1.1','2.2.2.2'],'a'*64)),patch.object(route,'probe',side_effect=probe or (lambda address:{'address':address,'elapsedMs':2 if address=='2.2.2.2' else 10})):return route.prepare(self.profile)
 def test_route_is_loopback_github_only_and_endpoint_is_runtime_data(self):
  result=self.prepare();cfg=json.loads((self.root/'route.yaml').read_text());self.assertEqual(result['selected'],['2.2.2.2']);self.assertEqual(cfg['bind-address'],'127.0.0.1');self.assertEqual(cfg['rules'],['DOMAIN,github.com,DIRECT','MATCH,REJECT']);self.assertEqual(cfg['hosts']['github.com'],result['selected']);self.assertNotIn('skip-cert-verify',json.dumps(cfg));self.assertNotIn('credential',(self.root/'git-network.config').read_text());self.assertFalse(result['requiresLaptopTunnel']);self.assertEqual((self.root/'route.yaml').stat().st_mode&0o777,0o600)
 def test_no_healthy_endpoint_is_not_a_success_or_cache_fallback(self):
  with self.assertRaisesRegex(ValueError,'No TLS-verified'):self.prepare(lambda address:None)
  self.assertFalse((self.root/'route.yaml').exists())
 def test_private_owner_and_strict_fields_are_required(self):
  for edit in [{'owner':'foreign'},{'listenPort':80},{'listenPort':True},{'token':'not-supported'}]:
   original=dict(self.data);self.data.update(edit);self.save()
   with self.assertRaises(ValueError):self.prepare()
   self.data=original
  self.save();self.profile.chmod(0o644)
  with self.assertRaises(ValueError):self.prepare()
 def test_restart_refreshes_endpoint_data_not_a_baked_address(self):
  self.prepare()
  self.prepare(lambda address:{'address':address,'elapsedMs':1} if address=='1.1.1.1' else None)
  self.assertEqual(json.loads((self.root/'route.yaml').read_text())['hosts']['github.com'],['1.1.1.1'])
if __name__=='__main__':unittest.main()
