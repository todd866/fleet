#!/usr/bin/env python3
"""home-mirror: what is skipped, when a pass is skipped, and the rsync it runs.

Breaks this guards against: a cloud placeholder read (iCloud downloads it), an exclude that
matches the wrong file, a pass over a metered link or on battery, a copy that loses deleted
files instead of keeping them, or a copy that runs outside the hub's disk slot.
"""
import importlib.machinery
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


def load():
    loader = importlib.machinery.SourceFileLoader("home_mirror", str(ROOT / "bin/home-mirror"))
    spec = importlib.util.spec_from_loader("home_mirror", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


M = load()
CFG = {"hub": "hub", "dest": "/Volumes/D/laptop-home", "every_h": 20, "min_free_gb": 40,
       "require_ac": True, "exclude": ["/Library/Caches/"], "skip_dataless": ["Desktop"]}


class Patterns(unittest.TestCase):
    def test_a_placeholder_path_is_anchored_and_its_wildcards_escaped(self):
        self.assertEqual(M.rsync_pattern("Desktop/a [1]*?.pdf"), "/Desktop/a \\[1\\]\\*\\?.pdf")

    def test_dataless_lists_only_files_under_home(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / "Desktop").mkdir()
            out = SimpleNamespace(stdout=f"{home}/Desktop/x.pdf\n/elsewhere/y\n")
            self.assertEqual(M.dataless(home, ["Desktop", "Absent"], run=lambda *a, **k: out), ["Desktop/x.pdf"])


class Rsync(unittest.TestCase):
    def test_deletions_are_kept_and_the_hub_side_holds_a_disk_slot(self):
        args = M.rsync_args(CFG, Path("/tmp/ex"), "20261115T000000Z")
        self.assertIn("--backup-dir=/Volumes/D/laptop-home-versions/20261115T000000Z", args)
        self.assertIn("--delete", args)
        remote = args[args.index("--rsync-path") + 1]
        self.assertIn("disk-slot --label home-mirror", remote)
        self.assertIn("'/Volumes/D'", remote)
        self.assertEqual(args[-1], "hub:/Volumes/D/laptop-home/")

    def test_stats_are_read(self):
        text = "Number of regular files transferred: 1,234\nTotal transferred file size: 5,000 bytes\n"
        self.assertEqual(M.stats(text), {"files": 1234, "bytes": 5000})


class Replicas(unittest.TestCase):
    CFG = {"hub": "hub", "volume": "Home", "encrypted": True,
           "offsite": {"name": "cloud", "status": "s.json", "source": "Home"},
           "exclude": ["/Library/Caches/", "/Projects/Big/", "/Games/ROMs/", "node_modules/", "/x/*-tmp/"]}

    def test_rows_skip_hidden_and_excluded_entries_and_carry_inner_excludes(self):
        st = {"last_success_epoch": 200, "first_success_epoch": 100, "total_bytes": 9}
        rows = M.replica_rows(self.CFG, st, ["Projects", ".ssh", "Library", "Music"])
        self.assertEqual([r["path"] for r in rows], ["Library", "Music", "Projects"])
        proj = rows[2]
        self.assertEqual(proj["except"], ["Projects/Big"])
        self.assertTrue(proj["encrypted"] and proj["verified"] and not proj.get("offsite"))

    def test_offsite_must_cover_the_latest_pass_not_only_the_first(self):
        st = {"last_success_epoch": 200, "first_success_epoch": 100, "total_bytes": 9}
        self.assertFalse(any(r.get("offsite") for r in M.replica_rows(self.CFG, st, ["Music"], held_at=50)))
        rows = M.replica_rows(self.CFG, st, ["Music"], held_at=150)
        self.assertFalse(any(r.get("offsite") for r in rows))
        rows = M.replica_rows(self.CFG, st, ["Music"], held_at=250)
        self.assertEqual([(r["volume"], bool(r.get("offsite"))) for r in rows], [("Home", False), ("cloud", True)])

    def test_no_rows_before_a_good_pass(self):
        self.assertEqual(M.replica_rows(self.CFG, {"total_bytes": 9}, ["Music"]), [])


class HubAttach(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.cfg = Path(self.tmp.name) / "hub.json"
        self.cfg.write_text(json.dumps({"image": "/D/home.sparsebundle", "volname": "Home", "size": "10g",
                                        "password_cmd": "cat pw.txt", "volumes_root": "/V"}))

    def tearDown(self):
        self.tmp.cleanup()

    def test_an_attached_image_is_left_alone(self):
        calls = []
        rc = M.hub_attach(self.cfg, run=lambda *a, **k: calls.append(a), ismount=lambda p: p == "/V/Home")
        self.assertEqual((rc, calls), (0, []))

    def test_the_password_goes_on_stdin_only_and_a_missing_image_is_created_encrypted(self):
        calls, mounted = [], []
        def run(args, **kw):
            calls.append((args, kw.get("input")))
            if args[:2] == ["/usr/bin/hdiutil", "attach"]:
                mounted.append(True)
            out = b"secret\n" if args[0] == "/bin/zsh" else b""
            return SimpleNamespace(returncode=0, stdout=out, stderr=b"")
        rc = M.hub_attach(self.cfg, run=run, ismount=lambda p: bool(mounted), exists=lambda p: False, stdin=b"")
        self.assertEqual(rc, 0)
        create, attach = calls[1], calls[2]
        self.assertIn("AES-256", create[0])
        self.assertEqual((create[1], attach[1]), (b"secret", b"secret"))
        self.assertTrue(all("secret" not in " ".join(args) for args, _ in calls))

    def test_a_piped_password_wins_and_password_cmd_is_not_run(self):
        calls, mounted = [], []
        def run(args, **kw):
            calls.append((args, kw.get("input")))
            if args[:2] == ["/usr/bin/hdiutil", "attach"]:
                mounted.append(True)
            return SimpleNamespace(returncode=0, stdout=b"wrong\n", stderr=b"")
        rc = M.hub_attach(self.cfg, run=run, ismount=lambda p: bool(mounted), exists=lambda p: True, stdin=b"piped\n")
        self.assertEqual(rc, 0)
        self.assertEqual([a[0] for a, _ in calls], ["/usr/bin/hdiutil"])
        self.assertEqual(calls[0][1], b"piped")


class Skips(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.state = Path(self.tmp.name)
        self.patches = [patch.object(M, "STATE", self.state),
                        patch.object(M, "METERED", self.state / "METERED_LINK")]
        for item in self.patches:
            item.start()

    def tearDown(self):
        for item in self.patches:
            item.stop()
        self.tmp.cleanup()

    def test_not_due_metered_battery_and_space(self):
        now = 1_800_000_000
        (self.state / "status.json").write_text(json.dumps({"last_success_epoch": now - 3600}))
        with patch.object(M.time, "time", return_value=now):
            self.assertEqual(M.run_once(CFG)["state"], "ok")
        (self.state / "status.json").write_text("{}")
        (self.state / "METERED_LINK").write_text("")
        self.assertEqual(M.run_once(CFG)["state"], "skipped: metered link")
        (self.state / "METERED_LINK").unlink()
        with patch.object(M, "on_ac", return_value=False):
            self.assertEqual(M.run_once(CFG)["state"], "skipped: on battery")
        with patch.object(M, "on_ac", return_value=True), patch.object(M, "remote_free_gb", return_value=10):
            self.assertIn("GB free at the destination", M.run_once(CFG)["state"])
        with patch.object(M, "on_ac", return_value=True), patch.object(M, "remote_free_gb", return_value=None):
            self.assertIn("unreachable", M.run_once(CFG)["state"])


if __name__ == "__main__":
    unittest.main()

class CapacitySafety(unittest.TestCase):
    def test_receiver_rechecks_capacity_inside_disk_slot(self):
        cfg = dict(CFG, required_free_gb=12, free_path='/Volumes/D')
        args = M.rsync_args(cfg, Path('/tmp/ex'), 'stamp')
        remote = args[args.index('--rsync-path') + 1]
        self.assertIn('disk-slot', remote)
        self.assertIn('destination reserve changed', remote)
        self.assertIn('exit 75', remote)

    def test_failed_placeholder_scan_is_not_an_empty_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp); (home/'Desktop').mkdir()
            with self.assertRaises(RuntimeError):
                M.dataless(home, ['Desktop'], run=lambda *a, **k: SimpleNamespace(returncode=1, stdout=''))


class BackupScheduling(unittest.TestCase):
    def test_recent_impossible_plan_only_defers_and_expires(self):
        import os
        with tempfile.TemporaryDirectory() as tmp:
            state=Path(tmp);log=state/'preflight.log'
            log.write_text('Total transferred file size: 900,000,000,000 bytes\n')
            os.utime(log,(1000,1000))
            with patch.object(M,'STATE',state):
                self.assertIn('waiting for space',M.cached_capacity(dict(CFG,min_free_gb=150),500,1100)['state'])
                self.assertIsNone(M.cached_capacity(dict(CFG,min_free_gb=150),1200,1100))
                self.assertIsNone(M.cached_capacity(dict(CFG,min_free_gb=150),500,30000))

    def test_busy_or_unknown_hub_priority_lock_defers_new_copy(self):
        cfg=dict(CFG,hub_priority_lock='/state/archive/run.lock')
        self.assertFalse(M.priority_busy(CFG,run=lambda *a,**kw: self.fail('unexpected call')))
        self.assertTrue(M.priority_busy(cfg,run=lambda *a,**kw: SimpleNamespace(returncode=75)))
        self.assertFalse(M.priority_busy(cfg,run=lambda *a,**kw: SimpleNamespace(returncode=0)))
