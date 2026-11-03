#!/usr/bin/env python3
"""Real filesystem tests; never touches production paths or accounts."""
import importlib.util,unittest,tempfile,os
from pathlib import Path
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('bundle',Path(__file__).with_name('state-bundle.py'));b=importlib.util.module_from_spec(spec);spec.loader.exec_module(b)
class Tests(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name).resolve();self.source=self.root/'state';self.source.mkdir(mode=0o700);(self.source/'sessions').mkdir();(self.source/'sessions'/'one.jsonl').write_text('actual journal 😀\n');(self.source/'config.json').write_text('{"model":"configured"}');os.chmod(self.source/'config.json',0o600);os.symlink('config.json',self.source/'config-link');self.bundle=self.root/'bundle'
 def tearDown(self):self.temp.cleanup()
 def snapshot(self):return b.snapshot([self.source],self.bundle)
 def test_complete_tree_relocated_restore_includes_sessions_config_modes_and_symlinks(self):
  self.snapshot();r=b.restore(self.bundle,self.root/'restored');self.assertTrue(r['verified']);self.assertEqual(b.fingerprint(self.source),b.fingerprint(self.root/'restored'/'object-0'))
 def test_replace_and_keep_prior_tree_for_undo(self):
  self.snapshot();(self.source/'config.json').write_text('changed');r=b.restore(self.bundle,replace_originals=True);self.assertEqual((self.source/'config.json').read_text(),'{"model":"configured"}');self.assertEqual((Path(r['prior_trees_retained'][0])/'config.json').read_text(),'changed')
 def test_prior_session_tree_is_retained_outside_the_runtime_scan_directory(self):
  scan=self.root/'runtime-sessions';scan.mkdir();workspace=scan/'workspace';workspace.mkdir();(workspace/'session.jsonl').write_text('header-bound-to-canonical-workspace')
  b.snapshot([workspace],self.bundle);(workspace/'session.jsonl').write_text('prior-state');r=b.restore(self.bundle,replace_originals=True)
  self.assertEqual(list(scan.iterdir()),[workspace]);self.assertTrue(all(scan not in Path(p).parents for p in r['prior_trees_retained']));self.assertEqual((workspace/'session.jsonl').read_text(),'header-bound-to-canonical-workspace')
 def test_tamper_refused_before_any_restore(self):
  self.snapshot();(self.bundle/'object-0'/'config.json').write_text('tampered');self.assertRaises(ValueError,b.restore,self.bundle,self.root/'restored');self.assertFalse((self.root/'restored').exists())
 def test_overlapping_live_wal_and_existing_bundle_are_refused(self):
  self.assertRaises(ValueError,b.snapshot,[self.source,self.source/'sessions'],self.bundle);(self.source/'orchestrator.sqlite-wal').touch();self.assertRaises(ValueError,self.snapshot);self.assertFalse(self.bundle.exists());(self.source/'orchestrator.sqlite-wal').unlink();self.snapshot();self.assertRaises(FileExistsError,self.snapshot)
 def test_failure_installing_second_tree_rolls_first_back(self):
  other=self.root/'config';other.write_text('original-other');b.snapshot([self.source,other],self.bundle);(self.source/'config.json').write_text('current-state');other.write_text('current-other');real=os.replace;triggered=[]
  def fail_once(src,dst):
   if '-restore-' in str(src) and Path(dst)==other and not triggered:triggered.append(True);raise OSError('explicit second-install failure')
   return real(src,dst)
  with patch.object(b.os,'replace',fail_once):self.assertRaises(OSError,b.restore,self.bundle,None,True)
  self.assertEqual((self.source/'config.json').read_text(),'current-state');self.assertEqual(other.read_text(),'current-other');self.assertTrue(triggered)
if __name__=='__main__':unittest.main(verbosity=2)
