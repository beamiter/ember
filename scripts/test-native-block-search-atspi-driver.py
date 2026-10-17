#!/usr/bin/env python3
"""Pure target and inert orchestration checks; no native/bus/PTY execution."""
import copy
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock,patch
import importlib.util


def load(name,file):
 spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file));m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
p=load('adapter','native-block-search-atspi-driver.py')
fixtures=load('fixtures','test-native-block-search-atspi-interaction.py')

class Tests(unittest.TestCase):
 def tree(self):return fixtures.Tests().tree()
 def driver(self,out):
  smoke=Mock(out=Path(out),events=[]);d=p.Driver(smoke,'window');d.expected='E03DISCOVERY';return d
 def button_tree(self):
  t=self.tree();picker=t['nodes'][2];n=copy.deepcopy(t['nodes'][3]);n.update(path=picker['path']+'/action',name='Stop search',role=43,states=[(1<<25)|(1<<30),0]);picker['children'].append(n['path']);t['nodes'].append(n);return t
 def test_exact_named_owned_button(self):
  t=self.button_tree();self.assertEqual(p.action_button(t,'E03DISCOVERY','Stop search')['role'],43)
  for field,value in [('role',29),('name','Stop searching'),('states',[0,0]),('screen_extents',[1000,1000,2,2])]:
   bad=copy.deepcopy(t);bad['nodes'][-1][field]=value
   with self.subTest(field=field),self.assertRaises(RuntimeError):p.action_button(bad,'E03DISCOVERY','Stop search')
 def test_duplicate_or_foreign_subtree_button_rejected(self):
  t=self.button_tree();n=copy.deepcopy(t['nodes'][-1]);n['path']+='2';t['nodes'][2]['children'].append(n['path']);t['nodes'].append(n)
  with self.assertRaises(RuntimeError):p.action_button(t,'E03DISCOVERY','Stop search')
  t=self.button_tree();n=t['nodes'][-1];t['nodes'][2]['children'].remove(n['path']);t['nodes'][1]['children'].append(n['path']);n['parent']=t['nodes'][1]['path']
  with self.assertRaises(RuntimeError):p.action_button(t,'E03DISCOVERY','Stop search')
 def test_actual_named_action_with_inert_owned_bus(self):
  dbus,broker,action,_=fixtures.Tests().dbus()
  with patch.dict('sys.modules',dbus=dbus):result=p.invoke_button(self.button_tree(),'E03DISCOVERY','Stop search')
  action.DoAction.assert_called_once_with(0,timeout=3);self.assertEqual(broker.GetConnectionUnixProcessID.call_count,2)
  self.assertEqual(result['response'],'pending existing search oracle')
 def test_named_action_rejects_foreign_owner_and_changed_actions(self):
  for foreign in (True,False):
   dbus,broker,action,_=fixtures.Tests().dbus(pid=99 if foreign else 42)
   if not foreign:action.GetActions.return_value=[['different','','']]
   with patch.dict('sys.modules',dbus=dbus),self.assertRaises(RuntimeError):p.invoke_button(self.button_tree(),'E03DISCOVERY','Stop search')
   action.DoAction.assert_not_called()
 def test_already_out_is_verified_without_dispatch(self):
  with tempfile.TemporaryDirectory() as td:
   d=self.driver(td);t=self.tree();f=p.public.controls(t,d.expected);f['All']['states'][0]&=~(1<<20);f['Out']['states'][0]|=1<<20;d.read=Mock(return_value=(t,f))
   with patch.object(p.public,'mutation') as mutate:d.output_scope()
   mutate.assert_not_called();self.assertEqual(d.smoke.events[-1]['event'],'verified-existing-state')
 def test_already_focused_does_not_claim_transition(self):
  with tempfile.TemporaryDirectory() as td:
   d=self.driver(td);t=self.tree();d.read=Mock(return_value=(t,p.public.controls(t,d.expected)))
   with patch.object(p.public,'mutation') as mutate:d.focus_query()
   mutate.assert_not_called();self.assertEqual(d.smoke.events[-1]['state'],'query-focused')
 def test_ambiguous_scope_rejects_without_dispatch(self):
  with tempfile.TemporaryDirectory() as td:
   d=self.driver(td);t=self.tree();f=p.public.controls(t,d.expected);f['Cmd']['states'][0]|=1<<20;d.read=Mock(return_value=(t,f))
   with patch.object(p.public,'mutation') as mutate:
    with self.assertRaises(RuntimeError):d.output_scope()
   mutate.assert_not_called()
 def test_scope_true_without_response_is_not_pass(self):
  with tempfile.TemporaryDirectory() as td:
   d=self.driver(td);t=self.tree();f=p.public.controls(t,d.expected);d.read=Mock(return_value=(t,f))
   with patch.object(p.public,'mutation',return_value={}),patch.object(p.time,'monotonic',side_effect=[0,9]):
    with self.assertRaisesRegex(RuntimeError,'response unproven'):d.output_scope()
 def test_late_matching_response_cannot_pass_deadline(self):
  with tempfile.TemporaryDirectory() as td:
   d=self.driver(td);t=self.tree();before=p.public.controls(t,d.expected);d.read=Mock(return_value=(t,before))
   with patch.object(p.public,'mutation',return_value={}),patch.object(p.time,'monotonic',side_effect=[0,0,9]):
    with self.assertRaisesRegex(RuntimeError,'deadline exhausted'):d.transition('Out','click',t,before,lambda _:True,'late')
   self.assertFalse(any(e.get('response_verified') for e in d.smoke.events))
 def test_replacement_target_cannot_complete_transition(self):
  with tempfile.TemporaryDirectory() as td:
   d=self.driver(td);t=self.tree();before=p.public.controls(t,d.expected);after=copy.deepcopy(before);after['Out']['path']+='new';d.read=Mock(return_value=(t,after))
   with patch.object(p.public,'mutation',return_value={}):
    with self.assertRaisesRegex(RuntimeError,'identity changed'):d.transition('Out','click',t,before,lambda _:True,'replace')
 def test_query_records_actual_clipboard_and_text(self):
  with tempfile.TemporaryDirectory() as td:
   d=self.driver(td);d.focus_query=Mock();d.smoke.read_clipboard.return_value=b'NEWQUERY';t=self.tree();t['nodes'][-1]['text']='NEWQUERY';d.read=Mock(return_value=(t,p.public.controls(t,'NEWQUERY')))
   d.query('NEWQUERY');record=json.loads((d.out/'0000-clipboard.json').read_text())
   self.assertEqual((d.out/'0000-clipboard.bin').read_bytes(),b'NEWQUERY');self.assertEqual(record['actual_text'],'NEWQUERY');self.assertEqual(record['expected_query'],'NEWQUERY')
 def test_late_exact_clipboard_retained_and_rejected(self):
  with tempfile.TemporaryDirectory() as td:
   d=self.driver(td);d.focus_query=Mock();d.smoke.read_clipboard.return_value=b'NEWQUERY';d.read=Mock()
   with patch.object(p.time,'monotonic',side_effect=[0,0,9]):
    with self.assertRaisesRegex(RuntimeError,'clipboard deadline exhausted'):d.query('NEWQUERY')
   self.assertEqual((d.out/'0000-clipboard.bin').read_bytes(),b'NEWQUERY')
   record=json.loads((d.out/'0000-clipboard.json').read_text())
   self.assertEqual(record['actual_hex'],b'NEWQUERY'.hex());self.assertEqual(record['sha256'],p.hashlib.sha256(b'NEWQUERY').hexdigest())
   self.assertEqual(d.expected,'E03DISCOVERY');d.read.assert_not_called();d.smoke.screenshot.assert_not_called()
   self.assertNotIn((('Right',),{}),[(c.args,c.kwargs) for c in d.smoke.key.call_args_list])

 def test_query_mismatch_retained_and_rejected(self):
  with tempfile.TemporaryDirectory() as td:
   d=self.driver(td);d.focus_query=Mock();d.smoke.read_clipboard.return_value=b'wrong'
   with patch.object(p.time,'monotonic',side_effect=[0,0,9]),patch.object(p.time,'sleep'):
    with self.assertRaisesRegex(RuntimeError,'clipboard mismatch'):d.query('NEWQUERY')
   self.assertEqual((d.out/'0000-clipboard.bin').read_bytes(),b'wrong')
 def test_no_latest_or_private_controller_fallback(self):
  with tempfile.TemporaryDirectory() as td:
   d=self.driver(td)
   with self.assertRaises(RuntimeError):d.button('Latest','latest')
  source=Path(p.__file__).read_text();self.assertNotIn('SetText(',source);self.assertNotIn('refresh_block_search_hits',source)
 def test_legacy_search_oracles_and_budget_remain(self):
  source=Path(p.__file__).with_name('native-block-search.py').read_text()
  for text in ['active_hit_claim(rows, token)',"stopped['claim'] == claim", "self.wait_active_hit('E03EARLYHIT', 'loading-return-rejected')", "'Complete for this retained output only'", "f'ROW{row + 1:06d}'", "'input p95 and presentation latency'",'self.memory_stop.wait(0.01)','for record in range(64)']:
   self.assertIn(text,source)
  self.assertNotIn('self.query_point',source);self.assertEqual(source.count('self.key(*BLOCK_SEARCH_CHORD)'),1)

class EvidenceTests(unittest.TestCase):
 tree=Tests.tree
 driver=Tests.driver
 button_tree=Tests.button_tree
 def result_tree(self):
  tree=self.button_tree();node=tree['nodes'][-1]
  found=p.public.controls(tree,'E03DISCOVERY');found['All']['states'][0]&=~(1<<20);found['Out']['states'][0]|=1<<20
  node['role']=62;node['states'][0]|=1<<20
  node['name']='Result 1 of 1; E03SNAPSHOT00 ROW000016 ....; output line 16 for e03emit snapshot 0'
  return tree
 def test_exact_result_excludes_query_and_fuzzy_or_other_record(self):
  tree=self.result_tree();line='E03SNAPSHOT00 ROW000016 ....'
  self.assertEqual(p.snapshot_hit(tree,'E03DISCOVERY',line)['path'],tree['nodes'][-1]['path'])
  for change in ['EO3SNAPSHOTOO ROW000016 ....','E03SNAPSHOT01 ROW000016 ....','E03SNAPSHOT00 ROW000017 ....','E03SNAPSHOT00']:
   with self.subTest(change=change),self.assertRaises(RuntimeError):p.snapshot_hit(tree,'E03DISCOVERY',change)
  for key,value in [('role',79),('role',43),('states',[(1<<25)|(1<<30),0]),('states',[0,0]),('screen_extents',[1000,1000,3,3]),('pid',99),('owner',':9.9'),('actions',[])]:
   bad=copy.deepcopy(tree);bad['nodes'][-1][key]=value
   with self.subTest(key=key),self.assertRaises(RuntimeError):p.snapshot_hit(bad,'E03DISCOVERY',line)
 def test_duplicate_result_fails(self):
  tree=self.result_tree();node=copy.deepcopy(tree['nodes'][-1]);node['path']+='dup'
  tree['nodes'][2]['children'].append(node['path']);tree['nodes'].append(node)
  with self.assertRaises(RuntimeError):p.snapshot_hit(tree,'E03DISCOVERY','E03SNAPSHOT00 ROW000016 ....')
 def inert_evidence(self,directory):
  driver=self.driver(directory);tree=self.button_tree()
  driver.read=Mock(return_value=(tree,p.public.controls(tree,driver.expected)))
  driver.smoke.run.return_value.stdout='1280 900'
  def screenshot(label):(driver.smoke.out/(label+'.png')).write_bytes(b'immutable fixture')
  driver.smoke.screenshot.side_effect=screenshot
  return driver,tree
 def test_evidence_brackets_exact_target_and_hash(self):
  with tempfile.TemporaryDirectory() as directory:
   driver,tree=self.inert_evidence(directory)
   driver.evidence(lambda t:p.action_button(t,driver.expected,'Stop search'),'stop')
   driver.smoke.screenshot.assert_called_once();self.assertEqual(driver.read.call_count,2)
   record=json.loads(next(driver.out.glob('public-evidence*.json')).read_text())
   self.assertTrue(record['after_verified']);self.assertEqual(record['image_sha256'],p.hashlib.sha256(b'immutable fixture').hexdigest())
   with self.assertRaisesRegex(RuntimeError,'reuse'):driver.evidence(lambda t:p.action_button(t,driver.expected,'Stop search'),'stop')
 def test_changed_target_geometry_text_owner_or_image_fails(self):
  for case in ['bounds','text','owner','image','dimensions','offscreen']:
   with self.subTest(case=case),tempfile.TemporaryDirectory() as directory:
    driver,tree=self.inert_evidence(directory);after=copy.deepcopy(tree)
    if case=='bounds':after['nodes'][-1]['screen_extents'][0]+=1
    elif case=='text':after['nodes'][-1]['name']='Restart this output'
    elif case=='owner':after['owner']=':99.99'
    elif case=='dimensions':driver.smoke.run.return_value.stdout='1280 9001280 900'
    elif case=='offscreen':driver.smoke.run.return_value.stdout='1 1'
    calls=[0]
    def read(label):
     calls[0]+=1
     if calls[0]==2:
      if case=='image':next(driver.smoke.out.glob('*.png')).write_bytes(b'changed')
      return after,{}
     return tree,{}
    driver.read.side_effect=read
    with self.assertRaises(RuntimeError):driver.evidence(lambda t:p.action_button(t,driver.expected,'Stop search'),'stop')
    self.assertEqual(list(driver.out.glob('public-evidence*.json')),[])
 def test_late_evidence_is_rejected(self):
  with tempfile.TemporaryDirectory() as directory:
   driver,tree=self.inert_evidence(directory)
   with patch.object(p.time,'monotonic',side_effect=[0,9]),self.assertRaisesRegex(RuntimeError,'deadline'):
    driver.evidence(lambda t:p.action_button(t,driver.expected,'Stop search'),'stop')
 def test_only_missing_before_capture_can_retry(self):
  with tempfile.TemporaryDirectory() as directory:
   driver=self.driver(directory);driver.evidence=Mock(side_effect=[p.MissingTarget('absent'),('tree','node')])
   with patch.object(p.time,'sleep'):self.assertEqual(driver.wait_evidence(None,'wait'),('tree','node'))
   self.assertEqual(driver.evidence.call_count,2)
   driver.evidence=Mock(side_effect=RuntimeError('ambiguous'))
   with self.assertRaisesRegex(RuntimeError,'ambiguous'):driver.wait_evidence(None,'wait')
   self.assertEqual(driver.evidence.call_count,1)
 def test_missing_after_capture_is_fatal_not_retryable(self):
  with tempfile.TemporaryDirectory() as directory:
   driver,tree=self.inert_evidence(directory);selector=Mock(side_effect=[tree['nodes'][-1],p.MissingTarget('gone')])
   with self.assertRaisesRegex(RuntimeError,'disappeared') as caught:driver.evidence(selector,'gone')
   self.assertNotIsInstance(caught.exception,p.MissingTarget)

 def test_no_operation_on_failed_evidence(self):
  with tempfile.TemporaryDirectory() as directory:
   driver=self.driver(directory);driver.evidence=Mock(side_effect=RuntimeError('changed'))
   with patch.object(p,'invoke_button') as invoke,self.assertRaises(RuntimeError):driver.button('Stop search','stop')
   invoke.assert_not_called()

class RawEvidenceTests(unittest.TestCase):
 tree=Tests.tree
 driver=Tests.driver
 button_tree=Tests.button_tree
 result_tree=EvidenceTests.result_tree
 inert_evidence=EvidenceTests.inert_evidence
 def raw_tree(self):
  tree=self.result_tree();hit=tree['nodes'][-1]
  hit['name']='Result 1 of 1; retained output; Retained row #18 · literal match: E03DISCOVERY'
  stop=copy.deepcopy(hit);stop.update(path=hit['path']+'-stop',name='Stop search',role=43)
  tree['nodes'][2]['children'].append(stop['path']);tree['nodes'].append(stop)
  return tree
 def test_same_tree_stop_exact_hit_and_stable_identity(self):
  tree=self.raw_tree();got=p.raw_hit(tree,'E03DISCOVERY','active')
  self.assertEqual(got['claim']['retained_row'],18)
  for case in ['no-stop','no-hit','query-only','wrong-token','unselected','wrong-role','continue','foreign-owner','foreign-pid']:
   bad=copy.deepcopy(tree);hit=bad['nodes'][-2];stop=bad['nodes'][-1]
   if case=='no-stop':stop['name']='Search this retained output'
   elif case=='no-hit':hit['name']='No matches'
   elif case=='query-only':hit['name']='E03DISCOVERY';hit['role']=79
   elif case=='wrong-token':hit['name']=hit['name'].replace('E03DISCOVERY','EO3DISCOVERY')
   elif case=='unselected':hit['states'][0]&=~(1<<20)
   elif case=='wrong-role':hit['role']=43
   elif case=='continue':stop['name']='Continue this output'
   elif case=='foreign-owner':hit['owner']=':9.9'
   else:hit['pid']=99
   with self.subTest(case=case),self.assertRaises(RuntimeError):p.raw_hit(bad,'E03DISCOVERY','active')
 def test_stopped_and_completion_cannot_accept_loading_or_continue(self):
  tree=self.raw_tree()
  with self.assertRaises(RuntimeError):p.raw_hit(tree,'E03DISCOVERY','stopped')
  tree['nodes'][-1]['name']='Restart this output'
  self.assertEqual(p.raw_hit(tree,'E03DISCOVERY','stopped')['claim']['retained_row'],18)
  tree['nodes'][-1]['name']='Continue this output'
  with self.assertRaises(RuntimeError):p.raw_hit(tree,'E03DISCOVERY','stopped')
  with self.assertRaises(RuntimeError):p.raw_hit(tree,'E03DISCOVERY','complete')
 def test_stale_stop_plus_continue_or_completion_never_passes(self):
  for label in ['Continue this output','Retained blocks #1–#6 · Block #2: Complete for this retained output only; 1 chunks this window']:
   tree=self.raw_tree();extra=copy.deepcopy(tree['nodes'][-1]);extra['path']+='stale';extra['name']=label
   tree['nodes'][2]['children'].append(extra['path']);tree['nodes'].append(extra)
   with self.subTest(label=label),self.assertRaises(RuntimeError):p.raw_hit(tree,'E03DISCOVERY','active')
 def test_exact_completion_text_and_stable_claim(self):
  tree=self.raw_tree();node=tree['nodes'][-1]
  node.update(name='Retained blocks #1–#6 · Block #2: Complete for this retained output only; 100 chunks this window',role=29)
  node['interfaces'].append(p.public.TEXT);node['text']=node['name']
  got=p.raw_hit(tree,'E03DISCOVERY','complete');self.assertEqual(got['claim']['retained_row'],18)
  for text in ['Complete for this retained output only','Partial output: operation time limit; Continue this output']:
   bad=copy.deepcopy(tree);bad['nodes'][-1].update(name=text,text=text)
   with self.assertRaises(RuntimeError):p.raw_hit(bad,'E03DISCOVERY','complete')

 def test_raw_budget_is_shared_from_action_not_reset_by_observation(self):
  with tempfile.TemporaryDirectory() as d:
   driver=self.driver(d);driver.raw_deadline=9.5;driver.wait_evidence=Mock(return_value=({},{}))
   with patch.object(p.time,'monotonic',return_value=2):driver.raw_result('active','first')
   self.assertEqual(driver.wait_evidence.call_args.kwargs['timeout'],7.5)
   with patch.object(p.time,'monotonic',return_value=9.5),self.assertRaises(RuntimeError):driver.raw_result('active','late')
   self.assertEqual(driver.wait_evidence.call_count,1)
 def test_changed_retained_row_across_capture_rejected(self):
  with tempfile.TemporaryDirectory() as d:
   driver,unused=self.inert_evidence(d);tree=self.raw_tree();after=copy.deepcopy(tree)
   after['nodes'][-2]['name']=after['nodes'][-2]['name'].replace('#18','#19')
   driver.read=Mock(side_effect=[(tree,{}),(after,{})])
   with self.assertRaisesRegex(RuntimeError,'changed'):driver.evidence(lambda t:p.raw_hit(t,driver.expected,'active'),'raw')

if __name__=='__main__':unittest.main()
