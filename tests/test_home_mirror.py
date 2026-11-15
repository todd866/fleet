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
