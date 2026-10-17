#!/usr/bin/env python3
"""Pure/static checks only; never launch an application, desktop or IPC service."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('bench',ROOT/'scripts/benchmark-session-startup-plan.py')
bench=importlib.util.module_from_spec(spec);spec.loader.exec_module(bench)
PLAN=ROOT/'tests/fixtures/session-startup/paired-plan.json'

class FixedPlanTests(unittest.TestCase):
    def test_reviewed_plan(self):
        self.assertEqual(bench.read_plan(PLAN)['measured_pairs'],240)

    def test_reject_plan_mutations(self):
        original=json.loads(PLAN.read_text())
        for key in ['schema','baseline_sha','candidate_sha','driver_control_sha','warmup_pairs',
                    'measured_pairs','budget_ms','bootstrap_resamples','bootstrap_seed',
                    'overlay_sha256','legacy_helper_sha256','scope','endpoint']:
            with self.subTest(key=key),tempfile.TemporaryDirectory() as d:
                value=copy.deepcopy(original);value[key]=None
                p=Path(d)/'plan.json';p.write_text(json.dumps(value))
                with self.assertRaises(ValueError):bench.read_plan(p)
        for value in [240.0,True,'240']:
            with tempfile.TemporaryDirectory() as d:
                bad=copy.deepcopy(original);bad['measured_pairs']=value
                p=Path(d)/'plan.json';p.write_text(json.dumps(bad))
                with self.assertRaises(ValueError):bench.read_plan(p)

    def test_fixed_balanced_schedule(self):
        rows=list(bench.schedule(bench.read_plan(PLAN)))
        self.assertEqual(len(rows),245)
        self.assertEqual(sum(w for _,w,_ in rows),5)
        measured=[order for _,warmup,order in rows if not warmup]
        self.assertEqual(measured.count(('baseline','candidate')),120)
        self.assertEqual(measured.count(('candidate','baseline')),120)
        self.assertEqual(len({i for i,_,_ in rows}),245)

    def test_percentile_uses_paired_deltas(self):
        pairs=[{'baseline':1_000_000_000,'candidate':1_000_000_000+i*1_000_000} for i in range(1,241)]
        result=bench.summarize(pairs)
        self.assertEqual(result['p95_delta_ms'],228)
        self.assertEqual(result['paired_delta_ms'],list(range(1,241)))

    def test_deterministic_bootstrap(self):
        pairs=[{'baseline':100,'candidate':100+i} for i in range(1,6)]
        self.assertEqual(bench.summarize(pairs),bench.summarize(pairs))

    def test_real_budget_boundary(self):
        for delta,status in [(49_999_000,'pass'),(50_000_000,'inconclusive-or-over-budget')]:
            pairs=[{'baseline':1_000_000_000,'candidate':1_000_000_000+delta}]*240
            self.assertEqual(bench.evaluate(pairs,240)['status'],status)

    def test_both_gates_required(self):
        pairs=[{'baseline':1,'candidate':2}]*240
        for upper,tail in [(50,0),(0,50),(50,50)]:
            with patch.object(bench,'summarize',return_value={'mean_delta_bootstrap_95ci_ms':[0,upper],'p95_delta_ms':tail}):
                self.assertNotEqual(bench.evaluate(pairs,240)['status'],'pass')

    def test_incomplete_or_invalid_never_reaches_statistics(self):
        for pairs in [[],[{'baseline':1,'candidate':2}]*239,[{'baseline':1}]*240,
                      [{'baseline':1,'candidate':0}]*240,[{'baseline':1,'candidate':True}]*240]:
            with patch.object(bench,'summarize') as stats,self.assertRaises(ValueError):
                try:bench.evaluate(pairs,240)
                finally:stats.assert_not_called()

    def test_failed_attempt_rejected(self):
        observed={'status':'pass','cleanup_verified':True}
        supervisor={'status':'pass','cleanup':{'empty':True}}
        bench.validate_attempt(0,observed,supervisor)
        for code,obs,sup in [(1,observed,supervisor),(None,observed,supervisor),
                (0,dict(observed,status='fail'),supervisor),
                (0,dict(observed,cleanup_verified=False),supervisor),
                (0,observed,dict(supervisor,status='fail')),
                (0,observed,dict(supervisor,cleanup={'empty':False}))]:
            with self.assertRaises(RuntimeError):bench.validate_attempt(code,obs,sup)

    def test_input_hashes_and_closure_fail_closed(self):
        plan=bench.read_plan(PLAN)
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)
            for relative in plan['driver_files']:
                p=root/relative;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(b'fixture')
            overlay=root/'overlay';overlay.write_bytes(b'overlay')
            fake=copy.deepcopy(plan)
            fake['driver_files']={p:bench.digest(root/p) for p in fake['driver_files']}
            fake['overlay_sha256']=bench.digest(overlay)
            bench.verify_inputs(fake,root,overlay)
            driver=root/'scripts/native-desktop-smoke.py';driver.write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError,'hash mismatch'):bench.verify_inputs(fake,root,overlay)
            driver.write_bytes(b'fixture')
            bench.verify_inputs(fake,root,overlay)
            missing=copy.deepcopy(fake);missing['driver_files'].pop('scripts/native-pty-capture.py')
            with self.assertRaisesRegex(ValueError,'closure'):bench.verify_inputs(missing,root,overlay)
            overlay.write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError,'Overlay'):bench.verify_inputs(fake,root,overlay)

    def test_exact_legacy_helper_and_shared_function_identity(self):
        self.assertEqual(bench.digest(bench.LEGACY_PATH),bench.HELPER_SHA)
        for name in ['summarize','wait_supervisor','drain_children','digest']:
            self.assertIs(getattr(bench,name),getattr(bench.legacy,name))
        source=(ROOT/'scripts/benchmark-session-startup-plan.py').read_text()
        self.assertIn("str(LEGACY_PATH), '--sample-supervisor'",source)
        self.assertIn("'--sample-helper'",bench.LEGACY_PATH.read_text())

    def test_overlay_is_unchanged(self):
        plan=bench.read_plan(PLAN)
        self.assertEqual(bench.digest(ROOT/'tests/fixtures/session-startup/benchmark-overlay.patch'),plan['overlay_sha256'])

    def test_workflow_dispatch_only_and_hash_pins(self):
        text=(ROOT/'.github/workflows/session-startup-benchmark.yml').read_text()
        self.assertIn('on:\n  workflow_dispatch:\npermissions:',text)
        self.assertNotIn('inputs.',text)
        self.assertIn('fail-fast: false',text)
        self.assertIn('backend: [x11, wayland]',text)
        self.assertIn('driver-control/scripts/native-desktop-smoke.py',text)
        for p in [PLAN,ROOT/'scripts/benchmark-session-startup-plan.py']:
            self.assertIn(bench.digest(p),text)

if __name__=='__main__':unittest.main()
