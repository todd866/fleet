#!/usr/bin/env python3
"""idrive-ingest: which sources mount, what is blanked, when a backup starts, how runs are read.

Each test names a production break: a second copy uploaded (a mask lost), a source read while a
migration still holds its disk, a run started twice a day or never after a source is added, or a
failed run read as a success.
"""
import importlib.machinery
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load():
    loader = importlib.machinery.SourceFileLoader("idrive_ingest", str(ROOT / "bin/idrive-ingest"))
    spec = importlib.util.spec_from_loader("idrive_ingest", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


M = load()


def cfg(**extra):
    base = {"vm": "v", "image": "img:1", "every_h": 24, "count_from": 0, "vm_mounts": ["/Volumes", "~"],
            "mask_everywhere": [".Spotlight-V100"], "sources": []}
    base.update(extra)
    return base


class Plan(unittest.TestCase):
    def test_masks_only_existing_folders_and_never_climb_out(self):
        dirs = {"/V/A", "/V/A/.Spotlight-V100", "/V/A/Old.sparsebundle"}
        out = M.plan(cfg(sources=[{"name": "A", "path": "/V/A", "mask": ["Old.sparsebundle", "gone", "../etc"]}]),
                     isdir=lambda p: p in dirs)
        self.assertEqual(out[0]["masks"], [".Spotlight-V100", "Old.sparsebundle"])

    def test_a_source_waits_for_its_stamp_and_for_a_busy_process(self):
        sources = [{"name": "T", "path": "/V/T", "after": "/s/done/verify"},
                   {"name": "W", "path": "/V/W", "wait_for": "migrate"}]
        out = M.plan(cfg(sources=sources), isdir=lambda p: True, busy=lambda pat: True, exists=lambda p: False)
        self.assertTrue(out[0]["waiting"].startswith("after"))
        self.assertEqual(out[1]["waiting"], "migrate")
        out = M.plan(cfg(sources=sources), isdir=lambda p: True, busy=lambda pat: False, exists=lambda p: True)
        self.assertEqual([s["waiting"] for s in out], [None, None])

    def test_a_source_still_waiting_is_not_called_missing(self):
        out = M.plan(cfg(sources=[{"name": "X", "path": "/V/X", "after": "/s/done"}]),
                     isdir=lambda p: False, exists=lambda p: False)
        self.assertFalse(out[0]["missing"])
        self.assertTrue(out[0]["waiting"])

    def test_a_missing_volume_is_reported_not_mounted(self):
        out = M.plan(cfg(sources=[{"name": "X", "path": "/V/X"}]), isdir=lambda p: False)
        self.assertTrue(out[0]["missing"])


class Coverage(unittest.TestCase):
    def test_every_volume_is_a_source_inside_one_or_has_a_reason(self):
        c = cfg(sources=[{"name": "A", "path": "/V/A"}, {"name": "T", "path": "/V/T/sub"}],
                not_sources={"/V/R": "system volume"})
        self.assertEqual(M.coverage(c, ["/V/A", "/V/T", "/V/R", "/V/New", ""]), ["/V/New"])

    def test_a_mask_needs_a_reason_unless_it_is_a_shared_default(self):
        c = cfg(sources=[{"name": "A", "path": "/V/A", "mask": ["x", {"path": "y", "why": "copy of z"}, {"path": "w"}]}])
        self.assertEqual(M.unexplained_masks(c), ["A/x", "A/w"])

    def test_mask_objects_are_planned_like_names(self):
        c = cfg(sources=[{"name": "A", "path": "/V/A", "mask": [{"path": "Old.sparsebundle", "why": "image"}]}])
        out = M.plan(c, isdir=lambda p: p in {"/V/A", "/V/A/Old.sparsebundle"})
        self.assertEqual(out[0]["masks"], ["Old.sparsebundle"])

    def test_count_gap(self):
        self.assertEqual(M.count_gap(1000, 990), 0.01)
        self.assertIsNone(M.count_gap(0, 5))


class Container(unittest.TestCase):
    def test_sources_mount_read_only_and_masks_are_tmpfs(self):
        sources = [{"name": "Media Mirror", "path": "/V/Media Mirror", "masks": ["Old.sparsebundle"],
                    "waiting": None, "missing": False}]
        args, spec = M.container_args(cfg(), sources)
        self.assertIn("/V/Media Mirror:/backup/Media Mirror:ro", args)
        self.assertIn("type=tmpfs,destination=/backup/Media Mirror/Old.sparsebundle,tmpfs-size=1m", args)
        self.assertIn(f"fleet.idrive-ingest.spec={spec}", args)
        self.assertEqual(args[-1], "img:1")

    def test_the_spec_changes_with_the_sources(self):
        one = [{"name": "A", "path": "/a", "masks": []}]
        two = one + [{"name": "B", "path": "/b", "masks": []}]
        self.assertNotEqual(M.spec_hash(cfg(), one), M.spec_hash(cfg(), two))
        self.assertEqual(M.spec_hash(cfg(), one), M.spec_hash(cfg(), list(one)))


class Runs(unittest.TestCase):
    def test_runs_newest_first_with_counts(self):
        logs = {"1794707038_Success_Manual": "Files considered for backup : 3\nFiles already present in your account : 3\n",
                "1794708000_Failure_Scheduled": "Files failed to backup : 7\n"}
        rows = M.parse_runs(["junk.txt", *logs], logs.get)
        self.assertEqual([r["status"] for r in rows], ["Failure", "Success"])
        self.assertEqual(rows[0]["failed"], 7)
        self.assertEqual(rows[1]["present"], 3)

    def test_progress_reads_the_last_redraw(self):
        tail = "\x1b[JBackup scan progress\r\nTotal Files Scanned : 10\r\n\x1b8Total Files Scanned : 359300\r\n"
        self.assertEqual(M.parse_progress(tail), {"scanned": 359300})

    def test_run_progress_sums_the_workers_not_one_files_percent(self):
        w = "Full Backup\n459.53 MB\n{}\n1000\n345.1\n/backup/A/x.mp4\n396\n82%\n"
        text = "\n==\n".join([w.format(100), w.format(150), ""])
        self.assertEqual(M.parse_workers(text), {"sent_bytes": 250, "total_bytes": 1000, "percent": 25.0})
        self.assertIsNone(M.parse_workers(""))


class Held(unittest.TestCase):
    def test_a_success_holds_the_sources_of_its_own_start_only(self):
        start = {"at": 1000, "sources": ["A", "B"]}
        self.assertEqual(M.holds({}, {"epoch": 1004}, start), {"A": 1004, "B": 1004})
        self.assertEqual(M.holds({"A": 5}, None, start), {"A": 5})
        later = {"at": 9000, "sources": ["A", "B", "New"]}
        self.assertEqual(M.holds({"A": 1004}, {"epoch": 1004}, later), {"A": 1004},
                         "an older success says nothing about a source added since")


class Due(unittest.TestCase):
    NOW = 1_800_000_000

    def test_daily_and_after_a_source_change(self):
        fresh = {"epoch": self.NOW - 3600}
        self.assertTrue(M.due(self.NOW, None, 24, "s1", {}, False))
        self.assertFalse(M.due(self.NOW, fresh, 24, "s1", {"spec": "s1"}, False))
        self.assertTrue(M.due(self.NOW, {"epoch": self.NOW - 25 * 3600}, 24, "s1", {"spec": "s1"}, False))
        self.assertTrue(M.due(self.NOW, fresh, 24, "s2", {"spec": "s1"}, False))
        self.assertFalse(M.due(self.NOW, None, 24, "s1", {}, True), "never twice within the hour")


if __name__ == "__main__":
    unittest.main()

class PriorityCycle(unittest.TestCase):
    def test_scoped_mount_keeps_existing_cloud_path_and_masks(self):
        base=cfg(sources=[{'name':'Archive','path':'/Volumes/Archive','mask':[{'path':'laptop/cache','why':'cache'}]}])
        scoped=M.priority_config(base,{'paths':['/Volumes/Archive/laptop']})
        source=M.plan(scoped,isdir=lambda p:True)[0]
        self.assertEqual(source['name'],'Archive/laptop')
        self.assertEqual(source['path'],'/Volumes/Archive/laptop')
        self.assertEqual(source['masks'],['cache'])
        args,_=M.container_args(scoped,[source])
        self.assertIn('/Volumes/Archive/laptop:/backup/Archive/laptop:ro',args)

    def test_priority_never_selects_outside_or_masked_data(self):
        base=cfg(sources=[{'name':'A','path':'/data','mask':['skip']}])
        for path in ['/other','/data/skip/deep']:
            with self.assertRaises(ValueError):M.priority_config(base,{'paths':[path]})

    def test_priority_then_bulk_then_next_priority_cycle(self):
        base=cfg(sources=[{'name':'A','path':'/data'}],priority_batches=[{'name':'laptop','paths':['/data/laptop'],'every_h':24}])
        self.assertEqual(M.cycle_choice(base,{},100000)[0],'laptop')
        receipts={'receipts':{'laptop':{'completed_at':100000,'paths':['/data/laptop']}}}
        self.assertEqual(M.cycle_choice(base,receipts,100001)[0],'bulk')
        self.assertEqual(M.cycle_choice(base,receipts,200000)[0],'laptop')

    def test_new_verified_offload_requeues_priority_even_after_recent_success(self):
        base=cfg(sources=[{'name':'A','path':'/data'}],priority_batches=[{'name':'laptop','paths':['/data/laptop'],'every_h':24}],
                 _priority_requests={'/data/laptop/papers':100050})
        ledger={'receipts':{'laptop':{'completed_at':100100,'paths':['/data/laptop'],'start':{'at':100000}}}}
        self.assertEqual(M.cycle_choice(base,ledger,100101)[0],'laptop')
        ledger['receipts']['laptop']['start']['at']=100051
        self.assertEqual(M.cycle_choice(base,ledger,100101)[0],'bulk')

class CycleTransitions(unittest.TestCase):
    def test_priority_success_is_not_a_full_success_when_bulk_starts(self):
        import tempfile,json
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'data/laptop').mkdir(parents=True);state=root/'state';state.mkdir()
            base=cfg(sources=[{'name':'A','path':str(root/'data')}],priority_batches=[{'name':'laptop','paths':[str(root/'data/laptop')]}])
            phase=M.priority_config(base,base['priority_batches'][0]);sources=M.plan(phase);signature=M.spec_hash(phase,sources)
            start={'at':100,'spec':signature,'sources':[sources[0]['name']],
                   'source_paths':{sources[0]['name']:{'path':sources[0]['path'],'masks':[]}}}
            observed={'running':False,'last_start':start,'last_success':{'epoch':110,'status':'Success','failed':0},'spec':signature}
            (state/'cycle.json').write_text(json.dumps({'phase':'laptop','phase_config':phase,'receipts':{}}))
            with patch.object(M,'STATE',state),patch.object(M.time,'time',return_value=100000),patch.object(M,'tick',side_effect=[observed,{'running':True}]) as tick:
                result=M.cycle(base)
            self.assertEqual(result['cycle']['phase'],'bulk')
            self.assertIsNone(result['last_full_success_epoch'])
            self.assertEqual(result['scope_receipts'][0]['paths'],[str(root/'data/laptop')])
            self.assertEqual(tick.call_args_list[-1].args[0]['sources'],base['sources'])

    def test_old_narrow_success_is_not_recorded_as_bulk(self):
        import tempfile,json
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'data/laptop').mkdir(parents=True);state=root/'state';state.mkdir()
            base=cfg(sources=[{'name':'A','path':str(root/'data')}],priority_batches=[{'name':'laptop','paths':[str(root/'data/laptop')]}])
            phase=M.priority_config(base,base['priority_batches'][0]);signature=M.spec_hash(phase,M.plan(phase))
            observed={'running':False,'last_start':{'at':100,'spec':signature,'source_paths':{'A/laptop':{'path':str(root/'data/laptop')}}},'last_success':{'epoch':110,'status':'Success','failed':0},'spec':M.spec_hash(base,M.plan(base))}
            (state/'cycle.json').write_text(json.dumps({'phase':'bulk','phase_config':base,'receipts':{}}))
            with patch.object(M,'STATE',state),patch.object(M.time,'time',return_value=100000),patch.object(M,'tick',side_effect=[observed,{'running':True}]):
                result=M.cycle(base)
            self.assertNotIn('bulk',result['cycle']['receipts'])
            self.assertIsNone(result['last_full_success_epoch'])
