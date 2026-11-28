#!/usr/bin/env python3
"""home-mirror: what is skipped, when a pass is skipped, and the rsync it runs.

Breaks this guards against: a cloud placeholder read (iCloud downloads it), an exclude that
matches the wrong file, a pass over a metered link or on battery, a copy that loses deleted
files instead of keeping them, or a copy that runs outside the hub's disk slot. The personal pass:
personal data left uncopied because the whole home does not fit, a personal copy that skips its
own space check or never yields the disk, a list of personal paths kept anywhere but
placement.toml, or a copy counted twice, or as off-site before the hub's backup has run.
"""
import importlib.machinery
import importlib.util
import json
import os
import sys
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
# No test reads this Mac's own home: the personal pass looks for <HOME>/.config/fleet/placement.toml.
M.HOME = Path("/nonexistent/home-mirror-tests")
# placement.toml is TOML: under a python3 older than 3.11, home-mirror borrows a newer one.
READS_TOML = sys.version_info >= (3, 11) or any(os.path.exists(p) for p in M.PYTHONS)
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

    def test_an_unmounted_drive_never_gets_a_new_image_on_the_boot_disk(self):
        cfg = json.loads(self.cfg.read_text())
        cfg["image"] = "/V/Drive/home.sparsebundle"
        self.cfg.write_text(json.dumps(cfg))
        calls = []
        def run(args, **kw):
            calls.append(args)
            return SimpleNamespace(returncode=0, stdout=b"secret\n", stderr=b"")
        rc = M.hub_attach(self.cfg, run=run, ismount=lambda p: False, exists=lambda p: False, stdin=b"")
        self.assertEqual(rc, 1)
        self.assertFalse(any(a[0] == "/usr/bin/hdiutil" for a in calls))

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

    def test_guarded_copy_stops_cleanly_on_slice(self):
        import sys
        cfg = dict(CFG, require_ac=False, slice_s=1)
        with tempfile.TemporaryFile(mode="w+") as buf:
            res = M.guarded_copy([sys.executable, "-c", "import time; time.sleep(10)"], buf, cfg)
            buf.seek(0)
            out = buf.read()
        self.assertEqual(res.returncode, 75)
        self.assertIn("Slice used (1s)", out)

    def test_run_once_records_slice_used_without_marking_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            state = Path(tmp)
            home = Path(tmp) / "home"
            home.mkdir()
            cfg = dict(CFG, require_ac=False, skip_dataless=[], slice_s=1)
            def fake_copy(args, handle, _cfg):
                handle.write("\nSlice used (1800s); partial copy retained, continues next pass.\n")
                return SimpleNamespace(returncode=75)
            with patch.object(M, "STATE", state), \
                 patch.object(M, "METERED", state / "METERED_LINK"), \
                 patch.object(M, "HOME", home), \
                 patch.object(M, "remote_free_gb", return_value=500.0), \
                 patch.object(M, "guarded_copy", side_effect=fake_copy):
                st = M.run_once(cfg)
            self.assertEqual(st["state"], "slice used, continues")
            self.assertNotIn("last_success_epoch", st)

    def test_disk_slot_busy_in_preflight_is_deferred_not_failed(self):
        with tempfile.TemporaryDirectory() as tmp:
            state = Path(tmp)
            cfg = dict(CFG, preflight_space=True)
            def fake_copy(args, handle, _cfg):
                handle.write("disk-slot: disk7 is busy, pid 123 has waited longer; home-mirror not started\n")
                return SimpleNamespace(returncode=12)
            with patch.object(M, "STATE", state), \
                 patch.object(M, "METERED", state / "METERED_LINK"), \
                 patch.object(M, "on_ac", return_value=True), \
                 patch.object(M, "remote_free_gb", return_value=500.0), \
                 patch.object(M, "dataless", return_value=[]), \
                 patch.object(M, "guarded_copy", side_effect=fake_copy):
                st = M.run_once(cfg)
            self.assertEqual(st["state"], "deferred: hub disk busy (disk-slot)")

    def test_busy_or_unknown_hub_priority_lock_defers_new_copy(self):
        cfg=dict(CFG,hub_priority_lock='/state/archive/run.lock')
        self.assertFalse(M.priority_busy(CFG,run=lambda *a,**kw: self.fail('unexpected call')))
        self.assertTrue(M.priority_busy(cfg,run=lambda *a,**kw: SimpleNamespace(returncode=75)))
        self.assertFalse(M.priority_busy(cfg,run=lambda *a,**kw: SimpleNamespace(returncode=0)))


class HubTrigger(unittest.TestCase):
    def test_unreachable_peer_exits_zero_and_skips_run(self):
        import io
        buf, calls = io.StringIO(), []
        def run(args, **kw):
            calls.append(args)
            return SimpleNamespace(returncode=255, stdout="", stderr="ssh: connect timed out")
        rc = M.hub_trigger("peer-host", run=run, out=buf)
        self.assertEqual(rc, 0)
        self.assertEqual(len(calls), 1)
        self.assertIn("unreachable", buf.getvalue())

    def test_reachable_peer_runs_remote_home_mirror_and_propagates_exit_code(self):
        for remote_rc in (0, 1):
            calls = []
            def run(args, **kw):
                calls.append(args)
                return SimpleNamespace(returncode=0 if args[-1] == "true" else remote_rc, stdout="", stderr="")
            rc = M.hub_trigger("peer-host", run=run)
            self.assertEqual(rc, remote_rc)
            self.assertEqual(len(calls), 2)
            self.assertEqual(calls[1][-1], '"$HOME/.local/bin/home-mirror" run')


PLACEMENT = """
[[dataset]]
name = "Projects"
class = "personal-unique"
paths = ["Projects"]

[[dataset]]
name = "Documents"
class = "personal-unique"
paths = ["Documents"]

[[dataset]]
name = "Music"
class = "personal-unique"
paths = ["Music/Liked", "Music/Gone"]

[[dataset]]
name = "Old phone"
class = "personal-unique"
paths = ["Pictures/Old"]

[[dataset]]
name = "Games"
class = "re-obtainable"
paths = ["Games"]
"""
GB = 10**9
SIZES = {"Projects": 400, "Documents": 20, "Music/Liked": 10, "Notes": 1, "Documents/Scans": 1}   # GB each folder holds
SLICE = (75, "\nSlice used (1800s); partial copy retained, continues next pass.\n")
STOPPED = (75, "\nDeferred: laptop activity, power or network changed; partial copy retained.\n")


@unittest.skipUnless(READS_TOML, "no python3 with tomllib on this Mac")
class PersonalPass(unittest.TestCase):
    """A home with three personal data sets present (a big one declared first), one path absent,
    one set excluded, and a folder that can be had again. The hub is a fake: each rsync is
    recorded, a plan answers with what is still to send, and a copy uses up the hub's free space."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home, self.state = Path(self.tmp.name) / "home", Path(self.tmp.name) / "state"
        for rel in ("Projects", "Documents", "Music/Liked", "Music/Other", "Pictures/Old", "Games", ".config/fleet"):
            (self.home / rel).mkdir(parents=True)
        self.placement = self.home / ".config/fleet/placement.toml"
        self.placement.write_text(PLACEMENT)
        self.cfg = dict(CFG, require_ac=False, skip_dataless=[], slice_s=1800, first_pass_free_gb=850,
                        exclude=["/Library/Caches/", "/Pictures/Old/"])
        self.calls, self.free, self.to_send = [], 0.0, dict(SIZES)
        self.patches = [patch.object(M, "STATE", self.state), patch.object(M, "HOME", self.home),
                        patch.object(M, "METERED", self.state / "METERED_LINK")]
        for item in self.patches:
            item.start()

    def tearDown(self):
        for item in self.patches:
            item.stop()
        self.tmp.cleanup()

    def run_once(self, free=None, stops=None, dry=False, full=True, **cfg):
        """One run. `free` sets the hub's free GB; `stops` maps a set's path (or "full") to how its copy ends."""
        if free is not None:
            self.free = float(free)

        def fake_copy(args, handle, used_cfg):
            sources = [arg.split("/./", 1)[1] for arg in args if "/./" in arg]
            which = ",".join(sources) if "--relative" in args else "full"
            step = "plan" if "--dry-run" in args else "copy"
            self.calls.append((which, step, args, used_cfg.get("slice_s")))
            if step == "copy" and which in (stops or {}):
                handle.write(stops[which][1])
                return SimpleNamespace(returncode=stops[which][0])
            total = sum(SIZES[s] for s in sources) if sources else 700
            send = sum(self.to_send[s] for s in sources) if sources else 700
            handle.write(f"Number of regular files transferred: 5\nTotal file size: {total * GB:,} bytes\n"
                         f"Total transferred file size: {send * GB:,} bytes\n")
            if step == "copy":
                self.free -= send
                self.to_send.update({s: 0 for s in sources})
            return SimpleNamespace(returncode=0)

        with patch.object(M, "remote_free_gb", side_effect=lambda cfg: self.free), \
             patch.object(M, "guarded_copy", side_effect=fake_copy):
            status = M.run_once(dict(self.cfg, **cfg), dry=dry, full=full)
        if not dry:
            M.write_status(status)
        return status

    def steps(self):
        return [(which, step) for which, step, _, _ in self.calls]

    def states(self, status):
        return {s["name"]: s["state"] for s in status["personal"]["sets"]}

    def test_a_big_first_set_waits_and_the_small_sets_after_it_are_copied(self):
        st = self.run_once(free=100)   # Projects needs 400 x 1.1 + the 40 GB reserve
        personal = st["personal"]
        self.assertEqual(self.states(st), {
            "Projects": "waiting for space: 100.0 GB free, this set needs 480.0 GB including reserve",
            "Documents": "ok", "Music": "ok"})
        self.assertEqual(self.steps(), [("Projects", "plan"), ("Documents", "plan"), ("Documents", "copy"),
                                        ("Music/Liked", "plan"), ("Music/Liked", "copy")])
        self.assertEqual((personal["sets_total"], personal["sets_copied"], personal["sets_waiting"]), (3, 2, 1))
        self.assertEqual(personal["copied_paths"], ["Documents", "Music/Liked"])
        self.assertNotIn("last_success_epoch", personal)   # not every set is copied
        self.assertTrue(personal["state"].startswith("waiting for space"), personal["state"])
        self.assertTrue(personal["state"].endswith("[Projects]; 2 of 3 sets copied"), personal["state"])
        big, small = personal["sets"][0], personal["sets"][1]
        self.assertEqual((big["name"], big["paths"], big["planned_bytes"]), ("Projects", ["Projects"], 400 * GB))
        self.assertAlmostEqual(big["required_free_gb"], 480.0)
        self.assertNotIn("last_success_epoch", big)
        self.assertTrue(small["last_success_epoch"])
        self.assertEqual((personal["planned_bytes"], round(personal["required_free_gb"])), (400 * GB, 480))
        # Only the copied sets are copies for placement; the full pass waits as it always did.
        rows = M.replica_rows(dict(self.cfg, offsite={}), st, ["Documents", "Music", "Projects"])
        self.assertEqual([r["path"] for r in rows], ["Documents", "Music/Liked"])
        self.assertIn("first pass needs 850", st["state"])
        self.assertNotIn("last_success_epoch", st)

    def test_each_set_is_checked_against_what_is_free_after_the_sets_before_it(self):
        self.to_send["Music/Liked"] = 40   # 84 GB with headroom and reserve: fits in 100, not in the 80 left
        st = self.run_once(free=100)
        self.assertEqual(self.states(st)["Documents"], "ok")
        self.assertEqual(self.states(st)["Music"],
                         "waiting for space: 80.0 GB free, this set needs 84.0 GB including reserve")
        self.assertNotIn(("Music/Liked", "copy"), self.steps())

    def test_a_priority_field_moves_a_set_and_the_files_order_breaks_ties(self):
        self.assertEqual([s["name"] for s in M.personal_sets(self.cfg)["sets"]], ["Projects", "Documents", "Music"])
        self.placement.write_text(PLACEMENT.replace('name = "Music"', 'name = "Music"\npriority = -1')
                                  .replace('name = "Projects"', 'name = "Projects"\npriority = 5'))
        self.assertEqual([s["name"] for s in M.personal_sets(self.cfg)["sets"]], ["Music", "Documents", "Projects"])
        self.run_once(free=2000)
        self.assertEqual([which for which, step in self.steps() if step == "copy"],
                         ["Music/Liked", "Documents", "Projects", "full"])

    def test_a_set_that_uses_its_slice_ends_the_run_and_the_next_run_resumes_at_it(self):
        st = self.run_once(free=2000, stops={"Documents": SLICE})
        self.assertEqual(self.states(st), {"Projects": "ok", "Documents": "slice used, continues",
                                           "Music": "queued: Documents goes first"})
        self.assertEqual(self.steps(), [("Projects", "plan"), ("Projects", "copy"),
                                        ("Documents", "plan"), ("Documents", "copy")])
        self.assertEqual([slice_s for _, _, _, slice_s in self.calls], [0, 1800, 0, 1800])   # plans are not sliced
        self.assertEqual(st["state"], "deferred: personal pass goes first (Documents: slice used, continues)")
        self.assertNotIn("last_success_epoch", st["personal"]["sets"][1])
        self.calls.clear()
        st = self.run_once()   # Projects is done for this cycle: not planned, not copied again
        self.assertEqual(self.steps(), [("Documents", "plan"), ("Documents", "copy"),
                                        ("Music/Liked", "plan"), ("Music/Liked", "copy"), ("full", "copy")])
        self.assertEqual((st["personal"]["state"], st["state"]), ("ok", "ok"))
        self.assertTrue(st["personal"]["last_success_epoch"])

    def test_a_copy_stopped_by_the_laptop_is_deferred_not_failed(self):
        st = self.run_once(free=2000, stops={"Documents": STOPPED})
        self.assertEqual(self.states(st)["Documents"], M.STOPPED)
        self.assertTrue(M.STOPPED.startswith("deferred"))
        self.assertTrue(st["personal"]["state"].startswith("deferred"), st["personal"]["state"])
        self.assertEqual(self.states(st)["Music"], "queued: Documents goes first")
        self.assertNotIn(("full", "copy"), self.steps())
        self.assertTrue(st["state"].startswith("deferred: personal pass goes first"), st["state"])
        # The full pass reads the same stop the same way.
        self.placement.unlink()
        (self.state / "status.json").unlink()
        self.assertEqual(self.run_once(free=2000, stops={"full": STOPPED})["state"], M.STOPPED)

    def test_a_set_that_did_not_fit_is_not_planned_again_soon_and_the_others_still_are(self):
        st = self.run_once(free=50)
        self.assertTrue(all(state.startswith("waiting for space") for state in self.states(st).values()))
        self.assertEqual(self.steps(), [("Projects", "plan"), ("Documents", "plan"), ("Music/Liked", "plan")])
        self.calls.clear()
        st = self.run_once(free=65)   # room for Documents now (62), which leaves 45: not for Music (51)
        self.assertEqual(self.steps(), [("Documents", "plan"), ("Documents", "copy")])
        self.assertEqual(self.states(st)["Documents"], "ok")
        self.assertIn("recent plan needs 480.0 GB", self.states(st)["Projects"])
        self.assertIn("45.0 GB free, recent plan needs 51.0 GB", self.states(st)["Music"])
        # A plan older than the cache is made again, for that set only.
        doc = json.loads((self.state / "status.json").read_text())
        doc["personal"]["sets"][0]["attempt_epoch"] -= 7 * 3600
        (self.state / "status.json").write_text(json.dumps(doc))
        self.calls.clear()
        self.run_once()
        self.assertEqual(self.steps(), [("Projects", "plan")])

    def test_no_set_is_gated_by_the_first_seed_figure_and_the_reserve_still_holds(self):
        st = self.run_once(free=100)
        self.assertIn("first pass needs 850", st["state"])
        copy = next(args for which, step, args, _ in self.calls if (which, step) == ("Documents", "copy"))
        self.assertIn(str(int(62.0 * 1e9 / 1024)), copy[copy.index("--rsync-path") + 1])   # the hub rechecks that set's figure
        (self.state / "status.json").unlink()
        self.calls.clear()
        low = self.run_once(free=30)
        self.assertEqual(set(self.states(low).values()), {"skipped: 30 GB free at the destination (floor 40)"})
        self.assertEqual(self.steps(), [])

    def test_a_set_is_copied_to_the_same_place_in_the_same_tree_with_its_own_logs_left_out(self):
        state = self.home / ".local/state/home-mirror"
        with patch.object(M, "STATE", state):
            self.run_once(free=100)
        args = next(args for which, step, args, _ in self.calls if (which, step) == ("Music/Liked", "copy"))
        self.assertEqual(args[-2:], [f"{self.home}/./Music/Liked", "hub:/Volumes/D/laptop-home/"])
        self.assertIn("--relative", args)
        self.assertNotIn(f"{self.home}/", args)   # never the whole home
        self.assertEqual(args[args.index("--exclude-from") + 1], str(state / "exclude.txt"))
        self.assertIn("disk-slot --label home-mirror", args[args.index("--rsync-path") + 1])
        excludes = (state / "exclude.txt").read_text().splitlines()
        self.assertEqual(excludes, ["/Library/Caches/", "/Pictures/Old/", "/.local/state/home-mirror/personal/"])
        self.assertEqual(sorted(p.name for p in (state / "personal").iterdir()),
                         ["documents.plan.log", "documents.rsync.log", "music.plan.log", "music.rsync.log",
                          "projects.plan.log"])
        whole = M.rsync_args(self.cfg, Path("/tmp/ex"), "stamp")
        self.assertEqual(whole[-2:], [f"{self.home}/", "hub:/Volumes/D/laptop-home/"])
        self.assertNotIn("--relative", whole)

    def test_the_list_comes_from_placement_and_a_new_personal_data_set_is_covered(self):
        got = M.personal_sets(self.cfg)
        self.assertEqual(got["sets"], [{"name": "Projects", "paths": ["Projects"]},   # not Games: it can be had again
                                       {"name": "Documents", "paths": ["Documents"]},
                                       {"name": "Music", "paths": ["Music/Liked"]}])
        self.assertEqual({(s["path"], s["why"]) for s in got["skipped"]},
                         {("Music/Gone", "not on this Mac"), ("Pictures/Old", "excluded in home-mirror.json")})
        (self.home / "Notes").mkdir()
        (self.home / "Documents/Scans").mkdir()
        self.placement.write_text(PLACEMENT + '\n[[dataset]]\nname = "New"\nclass = "personal-unique"\n'
                                  'paths = ["Notes", "Notes/Old", "Documents/Scans", "/elsewhere/archive"]\n')
        (self.home / "Notes/Old").mkdir()
        got = M.personal_sets(self.cfg)
        self.assertEqual(got["sets"][-1], {"name": "New", "paths": ["Documents/Scans", "Notes"]})   # Notes/Old rides with Notes
        self.assertIn(("/elsewhere/archive", "outside this home"), {(s["path"], s["why"]) for s in got["skipped"]})
        st = self.run_once(free=2000)
        self.assertEqual((st["personal"]["sets_total"], self.states(st)["New"]), (4, "ok"))
        # One row per copy: Scans is inside Documents, which is copied too.
        self.assertEqual(st["personal"]["copied_paths"], ["Documents", "Music/Liked", "Notes", "Projects"])

    def test_an_unreadable_placement_file_is_a_failure_and_a_missing_one_is_no_personal_pass(self):
        self.placement.write_text("[[dataset]\nname = broken")
        st = self.run_once(free=2000)
        self.assertTrue(st["personal"]["state"].startswith("failed: placement.toml"), st["personal"])
        self.assertNotIn("last_success_epoch", st["personal"])
        self.assertEqual((st["state"], self.steps()), ("ok", [("full", "copy")]))   # the full pass is as before
        self.placement.unlink()
        (self.state / "status.json").unlink()
        st = self.run_once(free=2000)
        self.assertTrue(st["personal"]["state"].startswith("not applicable"), st["personal"])
        self.assertEqual(st["state"], "ok")

    def test_a_dry_run_of_the_personal_pass_plans_every_set_and_copies_nothing(self):
        self.to_send["Music/Liked"] = 40
        st = self.run_once(free=100, dry=True, full=False)
        self.assertEqual(self.steps(), [("Projects", "plan"), ("Documents", "plan"), ("Music/Liked", "plan")])
        self.assertEqual(self.states(st), {
            "Projects": "waiting for space: 100.0 GB free, this set needs 480.0 GB including reserve",
            "Documents": "dry run: fits",   # and its 20 GB is counted against the sets after it
            "Music": "waiting for space: 80.0 GB free, this set needs 84.0 GB including reserve"})
        self.assertEqual([s["planned_bytes"] for s in st["personal"]["sets"]], [400 * GB, 20 * GB, 40 * GB])
        self.assertNotIn("state", st)   # the full pass is left alone
        self.assertEqual(self.free, 100.0)
        self.assertFalse((self.state / "status.json").exists())


def sets(**last):
    """Per-set records: name -> (paths, last good copy or None)."""
    return [dict({"name": name, "paths": paths}, **({"last_success_epoch": when, "state": "ok"} if when
                                                    else {"state": "waiting for space: too big"}))
            for name, (paths, when) in last.items()]


class PersonalReplicas(unittest.TestCase):
    CFG = dict(Replicas.CFG, exclude=["/Library/Caches/", "/.claude/tmp/", "/Music/Radio/"])
    PERSONAL = {"total_bytes": 7, "copied_paths": [".claude", "Documents", "Music/Liked"],
                "sets": sets(Agents=([".claude"], 200), Documents=(["Documents"], 200),
                             Music=(["Music/Liked"], 300), Projects=(["Projects"], None))}

    def local(self, rows):
        return [(r["path"], r["pass"]) for r in rows if not r.get("offsite")]

    def offsite(self, rows):
        return [r["path"] for r in rows if r.get("offsite")]

    def test_a_copied_set_is_an_encrypted_copy_and_a_waiting_set_is_none(self):
        rows = M.replica_rows(self.CFG, {"personal": self.PERSONAL}, ["Documents", "Music", "Projects"])
        self.assertEqual(self.local(rows), [(".claude", 200), ("Documents", 200), ("Music/Liked", 300)])
        self.assertTrue(all(r["encrypted"] and r["verified"] and r["bytes"] == 7 for r in rows))
        self.assertEqual(rows[0]["except"], [".claude/tmp"])
        none = dict(self.PERSONAL, sets=sets(Projects=(["Projects"], None)), copied_paths=[])
        self.assertEqual(M.replica_rows(self.CFG, {"personal": none}, ["Projects"]), [])

    def test_a_set_is_not_off_site_until_the_hubs_backup_has_run_after_its_own_copy(self):
        st = {"personal": self.PERSONAL}
        for held in (None, 150):
            self.assertEqual(self.offsite(M.replica_rows(self.CFG, st, [], held_at=held)), [])
        rows = M.replica_rows(self.CFG, st, [], held_at=250)   # after two sets' copies, before the third's
        self.assertEqual(self.offsite(rows), [".claude", "Documents"])
        self.assertTrue(all(r["volume"] == "cloud" for r in rows if r.get("offsite")))
        self.assertEqual(self.offsite(M.replica_rows(self.CFG, st, [], held_at=350)), [".claude", "Documents", "Music/Liked"])

    def test_one_copy_is_one_row_and_a_newer_set_copy_raises_the_off_site_bar(self):
        st = {"last_success_epoch": 100, "total_bytes": 9, "personal": self.PERSONAL}
        asked = []
        def held(since):
            asked.append(since)
            return 150   # the hub's backup ran after the full pass, before the personal copies
        rows = M.replica_rows(self.CFG, st, ["Documents", "Library", "Music"], held_at=held)
        self.assertEqual(self.local(rows), [("Documents", 200), ("Library", 100), ("Music", 300), (".claude", 200)])
        self.assertEqual(self.offsite(rows), ["Library"])
        self.assertEqual(sorted(set(asked)), [100, 200, 300])

    def test_a_copy_inside_a_set_still_to_copy_is_not_counted_as_that_set(self):
        # placement reads a row beneath a data set's path as a copy of the data set.
        personal = {"sets": sets(Projects=(["Projects"], None), Archives=(["Downloads", "Projects/old"], 200),
                                 Documents=(["Documents"], 200), Scans=(["Documents/Scans"], 300))}
        M.summarise(personal, CFG)
        self.assertEqual(personal["copied_paths"], ["Documents", "Downloads"])
        self.assertEqual((personal["sets_total"], personal["sets_copied"], personal["sets_waiting"]), (4, 3, 1))
        self.assertNotIn("last_success_epoch", personal)
        personal["sets"][0].update(last_success_epoch=400, state="ok")
        M.summarise(personal, CFG)
        self.assertEqual(personal["copied_paths"], ["Documents", "Downloads", "Projects"])
        self.assertEqual((personal["state"], personal["last_success_epoch"]), ("ok", 200))   # the oldest set's

    def test_the_hub_is_asked_once_for_each_pass_time(self):
        calls = []
        def run(args, **kw):
            calls.append(args)
            return SimpleNamespace(stdout=json.dumps({"last_start": {"at": 180}, "held": {"Home": 190}}))
        held = M.held_since(self.CFG, run=run)
        self.assertEqual([held(100), held(200), held(100), held(200)], [190, None, 190, None])
        self.assertEqual(len(calls), 2)
