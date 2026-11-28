#!/usr/bin/env python3
"""fleet morning: rows that were red for the wrong reason, and one that was missing.

Each test names a row from the 13 Nov 2026 report: heat counted against a line the slot policy
no longer uses, one disk floor for every Mac, a Recovery volume as a second disk, retired jobs
that failed for ever, UTC jobs judged in local time, a job failure counted again as a project
failure, and a queue that stopped dispatching without telling anyone.
"""
import json
import os
import plistlib
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from fleet.morning import evaluate, load_config, render  # noqa: E402
from fleet.morning import cli, collect, probe  # noqa: E402

NOW = datetime(2026, 11, 7, 9, 0, tzinfo=timezone.utc)
NOW_S = int(NOW.timestamp())
HOUR = 3600


def config(**override):
    cfg = load_config(None)
    cfg["tz"] = "UTC"
    cfg.update(override)
    return cfg


def host(**extra):
    base = {"ok": True}
    base.update(extra)
    return base


def by_key(findings):
    return {item.key: item for item in findings}


def rows(snapshot, cfg=None):
    return by_key(evaluate(snapshot, cfg or config(), NOW_S))


def hot_samples(chip, hours=3, thermal="nominal"):
    minutes = hours * 60
    return [{"t": NOW_S - (minutes - i) * 60, "chipC": chip, "thermal": thermal} for i in range(minutes + 1)]


class ThermalLine(unittest.TestCase):
    def test_heat_is_counted_against_the_macs_own_cool_target(self):
        warm = hot_samples(71)
        row = rows({"hosts": {"hub": host(samples=warm, cool_c=95)}})["thermal:hub"]
        self.assertEqual(row.status, "pass")
        self.assertEqual(row.datum, "≥95C")
        row = rows({"hosts": {"hub": host(samples=hot_samples(96), cool_c=95)}})["thermal:hub"]
        self.assertEqual(row.status, "warn")
        self.assertEqual(row.datum, "≥95C")

    def test_the_macs_own_target_wins_over_the_config_line(self):
        cfg = config()
        cfg["thermal"]["hot_c"] = 60
        self.assertEqual(rows({"hosts": {"hub": host(samples=hot_samples(71), cool_c=95)}}, cfg)["thermal:hub"].status, "pass")
        # A Mac that sent no target is judged by the config line, when there is one.
        self.assertEqual(rows({"hosts": {"hub": host(samples=hot_samples(71))}}, cfg)["thermal:hub"].status, "warn")

    def test_no_target_anywhere_judges_thermal_pressure_only(self):
        self.assertNotIn("hot_c", load_config(None)["thermal"])
        row = rows({"hosts": {"hub": host(samples=hot_samples(99))}})["thermal:hub"]
        self.assertEqual(row.status, "pass")
        self.assertEqual(row.datum, "no cool target")
        row = rows({"hosts": {"hub": host(samples=hot_samples(50, thermal="serious"))}})["thermal:hub"]
        self.assertEqual(row.status, "warn")
        # heavy-slot reads 0 as "no cool gate"; so does the report.
        row = rows({"hosts": {"hub": host(samples=hot_samples(99), cool_c=0)}})["thermal:hub"]
        self.assertEqual(row.status, "pass")

    def test_probe_reads_the_cool_file_heavy_slot_reads(self):
        with tempfile.TemporaryDirectory() as tmp:
            state = Path(tmp) / ".local/state/heavy-slot"
            state.mkdir(parents=True)
            with patch.object(probe, "HOME", Path(tmp)), patch.dict(os.environ, {}, clear=False):
                os.environ.pop("HEAVY_SLOT_DIR", None)
                self.assertIsNone(probe._cool_target())
                (state / "cool").write_text("95 set by the owner\n")
                self.assertEqual(probe._cool_target(), 95)
                (state / "cool").write_text("warm\n")
                self.assertIsNone(probe._cool_target())
            other = Path(tmp) / "elsewhere"
            other.mkdir()
            (other / "cool").write_text("88.5\n")
            with patch.dict(os.environ, {"HEAVY_SLOT_DIR": str(other)}):
                self.assertEqual(probe._cool_target(), 88)

    def test_cool_target_travels_in_the_snapshot(self):
        row = collect._host_snapshot("hub", {"cool_c": 95, "samples": [{"t": 1}]}, {}, config(), {}, NOW_S)
        self.assertEqual(row["cool_c"], 95)


class DiskTargets(unittest.TestCase):
    TARGETS = {"hub": {"target_gb": 60.0, "floor_gb": 40.0}, "primary": {"target_gb": 250.0, "floor_gb": 125.0}}

    def row(self, name, free_gb, cfg=None, targets=TARGETS):
        snap = {"hosts": {name: host(disks=[{"name": "internal", "free_bytes": free_gb * 1e9, "trend_h24": 0}])}}
        if targets is not None:
            snap["disk_targets"] = targets
        return rows(snap, cfg)[f"disk:{name}:internal"]

    def test_each_mac_is_judged_by_its_own_target_and_floor(self):
        self.assertEqual(self.row("hub", 61).status, "pass")
        warn = self.row("hub", 50)
        self.assertEqual((warn.status, warn.datum), ("warn", "target 60 GB"))
        fail = self.row("hub", 39)
        self.assertEqual((fail.status, fail.datum), ("fail", "floor 40 GB"))
        self.assertEqual(self.row("primary", 76).status, "fail")
        self.assertEqual(self.row("primary", 200).status, "warn")
        self.assertIn("target 250 GB; floor 125 GB", self.row("primary", 200).detail)
        self.assertEqual(self.row("primary", 260).status, "pass")

    def test_a_mac_without_a_row_keeps_the_example_default(self):
        self.assertEqual(self.row("shared", 41).status, "pass")
        self.assertEqual(self.row("shared", 30).status, "warn")
        self.assertEqual(self.row("shared", 19).status, "fail")
        self.assertEqual(self.row("hub", 30, targets=None).status, "warn")

    def test_a_floor_written_in_the_report_config_wins(self):
        cfg = config()
        cfg["disk"]["floors"] = {"hub": {"internal": 20}}
        self.assertEqual(self.row("hub", 30, cfg).status, "pass")

    def test_targets_are_for_the_internal_disk_only(self):
        snap = {"disk_targets": self.TARGETS,
                "hosts": {"hub": host(disks=[{"name": "archive", "free_bytes": 45e9, "trend_h24": 0}])}}
        self.assertEqual(rows(snap)["disk:hub:archive"].status, "pass")

    def test_targets_file_is_read_per_host(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            folder = home / ".local/state/mac-sync/storage-audit"
            folder.mkdir(parents=True)
            (folder / "free-targets.tsv").write_text(
                "# host\ttarget_empty\ttarget_with_purgeable\tfloor_empty\tnote\n"
                "primary\t250\t500\t125\t2 TB disk\n"
                "hub\t60\t75\t40\tnote\n"
                "broken\tlots\n"
                "\n")
            found = collect._disk_targets(home, config())
            self.assertEqual(found, {
                "primary": {"target_gb": 250.0, "with_purgeable_gb": 500.0, "floor_gb": 125.0},
                "hub": {"target_gb": 60.0, "with_purgeable_gb": 75.0, "floor_gb": 40.0},
            })
            elsewhere = home / "targets.tsv"
            elsewhere.write_text("shared\t40\t60\t20\n")
            cfg = config()
            cfg["disk"]["targets_file"] = str(elsewhere)
            self.assertEqual(list(collect._disk_targets(home, cfg)), ["shared"])
            cfg["disk"]["targets_file"] = str(home / "missing.tsv")
            self.assertEqual(collect._disk_targets(home, cfg), {})


class SystemVolumes(unittest.TestCase):
    def test_recovery_is_not_a_second_disk(self):
        disks = [
            {"name": "internal", "free_bytes": 79e9, "mounted": True, "volume_uuids": ["A"]},
            {"name": "Recovery", "free_bytes": 79e9, "mounted": True, "volume_uuids": ["B"]},
        ]
        found = rows({"hosts": {"shared": host(disks=disks)}})
        self.assertIn("disk:shared:internal", found)
        self.assertEqual([key for key in found if "Recovery" in key], [])
        # Older snapshots carry no mount flag.
        found = rows({"hosts": {"shared": host(disks=[{"name": n, "free_bytes": 79e9} for n in ("internal", "Recovery", "Preboot")])}})
        self.assertEqual(sorted(key for key in found if key.startswith("disk:")), ["disk:shared:internal"])

    def test_probe_does_not_collect_system_volumes(self):
        mounts = [
            {"name": "internal", "uuid": "A", "free_bytes": 10, "internal": True},
            {"name": "Recovery", "uuid": "B", "free_bytes": 10, "internal": False},
            {"name": "Archive", "uuid": "C", "free_bytes": 10, "internal": False},
        ]
        self.assertEqual([row["name"] for row in probe.disks_from_mounts({}, mounts, NOW_S)], ["internal", "Archive"])
        summary = {"locations": [
            {"name": "internal", "kind": "volume", "free_bytes": 1},
            {"name": "Recovery", "kind": "volume", "free_bytes": 1},
            {"name": "Update", "kind": "volume", "free_bytes": 1},
        ]}
        self.assertEqual([row["name"] for row in probe._disks_from_summary(summary)], ["internal"])


class RetiredJobs(unittest.TestCase):
    REGISTRY = {
        "com.example.kept": {"loaded": False, "status": "0", "when": "every 30m"},
        "com.example.retired": {"loaded": False, "status": "0", "when": "every 30m"},
        "com.example.elsewhere": {"loaded": True, "status": "0", "when": "daily 09:00"},
    }

    def test_registry_file_names_each_jobs_home(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            (folder / "jobs.tsv").write_text(
                "# label\thome\twhat\n"
                "com.example.kept\thub\tkept\n"
                "com.example.everywhere\tall\teverywhere\n"
                "com.example.elsewhere\tprimary, shared\tnot here\n")
            found = collect._registered_jobs(folder)
            self.assertEqual(found["com.example.elsewhere"], ["primary", "shared"])
            self.assertEqual(found["com.example.everywhere"], ["all"])
            self.assertIsNone(collect._registered_jobs(folder / "missing"))

    def test_a_job_with_no_plist_and_no_registry_line_is_gone(self):
        registered = {"com.example.kept": ["hub"], "com.example.elsewhere": ["primary"]}
        seen = [{"label": "com.example.live", "loaded": True, "exit": 0, "runs": 3, "state": "not running", "when": "every 5m"}]
        merged = collect._merge_jobs(self.REGISTRY, seen, registered=registered, here="hub")
        self.assertEqual(sorted(job["label"] for job in merged), ["com.example.kept", "com.example.live"])
        # No jobs.tsv on this Mac: nothing can be called retired.
        merged = collect._merge_jobs(self.REGISTRY, seen)
        self.assertEqual(len(merged), 4)

    def test_evaluator_drops_retired_rows_from_an_older_snapshot(self):
        jobs = [
            {"label": "com.example.retired", "loaded": False, "exit": 0, "when": "every 30m", "cmd": ""},
            {"label": "com.example.retired-check", "loaded": True, "exit": 0, "when": "daily 09:00", "last_epoch": 1, "cmd": ""},
            {"label": "com.example.kept", "loaded": False, "exit": 0, "when": "every 30m", "cmd": ""},
            {"label": "com.example.unlisted-but-installed", "loaded": False, "exit": None, "runs": None, "state": "", "when": "every 30m"},
        ]
        snap = {"registry_jobs": {"com.example.kept": ["all"]}, "hosts": {"hub": host(jobs=jobs)}}
        found = rows(snap)
        self.assertEqual(sorted(key for key in found if key.startswith("job:")),
                         ["job:hub:kept", "job:hub:unlisted-but-installed", "job:unregistered"])
        self.assertEqual(found["job:hub:kept"].status, "fail")
        # Without the registry list the old judgement stands.
        self.assertEqual(len([k for k in rows({"hosts": {"hub": host(jobs=jobs)}}) if k.startswith("job:")]), 4)


class UtcJobs(unittest.TestCase):
    CMD = "/bin/zsh /home/.local/bin/utc-daily --label com.example.nightly --at 14:00 --retry -- /bin/true"

    def job(self, **extra):
        # 09:00Z now; the 14:00Z slot last came round 19 h ago and the job ran then.
        base = {"label": "com.example.nightly", "loaded": True, "exit": 0, "runs": 60, "state": "not running",
                "when": "daily 14:00", "last_epoch": NOW_S - 19 * HOUR, "cmd": self.CMD}
        base.update(extra)
        return base

    def test_a_utc_job_is_judged_in_utc_whatever_the_report_zone(self):
        cfg = config(tz="Australia/Perth")   # 17:00 local: a local 14:00 slot would look missed
        row = rows({"hosts": {"hub": host(jobs=[self.job(utc=True)])}}, cfg)["job:hub:nightly"]
        self.assertEqual((row.status, row.datum), ("pass", "daily 14:00Z"))
        # An older snapshot has no flag; the command line says it is on the wrapper.
        row = rows({"hosts": {"hub": host(jobs=[self.job()])}}, cfg)["job:hub:nightly"]
        self.assertEqual((row.status, row.datum), ("pass", "daily 14:00Z"))
        plain = self.job(cmd="/bin/true", last_epoch=NOW_S - 30 * HOUR)
        self.assertEqual(rows({"hosts": {"hub": host(jobs=[plain])}}, cfg)["job:hub:nightly"].status, "fail")

    def test_the_utc_flag_survives_the_registry_merge(self):
        merged = collect._merge_jobs({}, [self.job(utc=True, retry_pending=True)])
        self.assertTrue(merged[0]["utc"])
        self.assertTrue(merged[0]["retry_pending"])
        self.assertTrue(merged[0]["plist"])

    def test_a_run_ended_by_term_is_not_a_failure_while_it_runs_or_waits_to_retry(self):
        for code in (-15, 143):
            running = self.job(utc=True, exit=code, state="running")
            self.assertEqual(rows({"hosts": {"hub": host(jobs=[running])}})["job:hub:nightly"].status, "pass", code)
            waiting = self.job(utc=True, exit=code, retry_pending=True)
            row = rows({"hosts": {"hub": host(jobs=[waiting])}})["job:hub:nightly"]
            self.assertEqual((row.status, row.datum), ("pass", "retry due"), code)
            # Cut and not coming back: that is a failed run.
            self.assertEqual(rows({"hosts": {"hub": host(jobs=[self.job(utc=True, exit=code)])}})["job:hub:nightly"].status, "fail", code)
        failed = self.job(utc=True, exit=1, retry_pending=True)
        self.assertEqual(rows({"hosts": {"hub": host(jobs=[failed])}})["job:hub:nightly"].status, "fail")

    def test_probe_reads_the_wrappers_own_record(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            agents = home / "Library/LaunchAgents"
            agents.mkdir(parents=True)
            state = home / ".local/state/utc-daily/com.example.nightly"
            state.mkdir(parents=True)
            with (agents / "com.example.nightly.plist").open("wb") as handle:
                plistlib.dump({"Label": "com.example.nightly", "StartInterval": 300,
                               "ProgramArguments": self.CMD.split()}, handle)
            (state / "last").write_text(f"{NOW_S - 100} {NOW_S - 50} 14:00 143\n")

            def gather(running_text, launchd_state="not running", alive=True):
                (state / "running").write_text(running_text)
                printed = {"com.example.nightly": {"state": launchd_state, "runs": 9, "exit": 0}}
                with patch.object(probe, "HOME", home), patch.object(probe, "_launchctl", return_value={}), \
                        patch.object(probe, "_launchctl_print", return_value=printed), \
                        patch.object(probe, "_pid_alive", return_value=alive), \
                        patch.object(probe, "_boot_time", return_value="1794000000"):
                    return probe._jobs()[0]

            # launchd's last exit status is the last tick's (a reload resets it); the wrapper's is the run's.
            idle = gather("")
            self.assertEqual((idle["exit"], idle["last_epoch"], idle["utc"]), (143, NOW_S - 100, True))
            self.assertNotIn("retry_pending", idle)
            waiting = gather("1400 2026-11-06 - - 1\n")
            self.assertTrue(waiting["retry_pending"])
            self.assertEqual(waiting["exit"], 143)
            live = gather("1400 2026-11-07 4242 1794000000 2\n")
            self.assertEqual(live["state"], "running")
            self.assertNotIn("retry_pending", live)
            # A pid from before this boot, or one that is gone, is not a run.
            self.assertEqual(gather("1400 2026-11-07 4242 1793000000 2\n")["state"], "not running")
            self.assertEqual(gather("1400 2026-11-07 4242 1794000000 2\n", alive=False)["state"], "not running")


class ProjectRows(unittest.TestCase):
    def test_a_job_failure_is_counted_once(self):
        job = {"label": "com.example.selector-push", "loaded": True, "exit": 1, "runs": 3,
               "state": "not running", "when": "every 6h", "cmd": "/x/Projects/selector/push"}
        project = {"name": "selector", "git": {"dirty": False, "ahead": 0, "behind": 0, "unpushed": 0},
                   "ci": None, "jobs": [job]}
        found = rows({"hosts": {"hub": host(jobs=[job])}, "projects": [project]})
        self.assertEqual(found["job:hub:selector-push"].status, "fail")
        self.assertEqual(found["project:selector"].status, "pass")
        self.assertIn("job com.example.selector-push exit 1 (see its job row)", found["project:selector"].detail)
        _text, document = render(list(found.values()), NOW_S, "2026-11-07")
        self.assertEqual(document["counts"]["fail"], 1)

    def test_a_job_that_is_only_restarting_does_not_fail_its_project(self):
        job = {"label": "com.example.selector-push", "loaded": True, "exit": -15, "runs": 3,
               "state": "running", "when": "always on", "cmd": ""}
        project = {"name": "selector", "git": {}, "ci": None, "jobs": [job]}
        found = rows({"hosts": {"hub": host(jobs=[job])}, "projects": [project]})
        self.assertEqual({row.status for row in found.values()}, {"pass"})

    def test_a_job_no_host_reports_still_fails_its_project(self):
        project = {"name": "selector", "git": {}, "ci": None,
                   "jobs": [{"label": "com.example.selector-worker", "exit": 2}]}
        self.assertEqual(rows({"projects": [project]})["project:selector"].status, "fail")
        away = {"hosts": {"hub": {"ok": False, "jobs": project["jobs"]}}, "projects": [project]}
        self.assertEqual(rows(away)["project:selector"].status, "fail")


class QueueStall(unittest.TestCase):
    STALL = {"id": "review-7", "state": "needs_review", "revision": 12, "host": "hub",
             "since": NOW_S - 5 * HOUR, "blocker": "No live owner or completion receipt; outcome uncertain",
             "clear": "fleet-queue show review-7; then fleet-queue cancel review-7 --revision 12 --note '<what you found>'"}

    def test_an_orphaned_attempt_is_a_finding_that_says_how_to_clear_it(self):
        snap = {"hosts": {"hub": host(queue={"observed_at": NOW_S - 60, "stalls": [self.STALL]})}}
        row = rows(snap)["queue:hub:review-7"]
        self.assertEqual((row.status, row.area, row.name, row.value), ("fail", "job", "hub queue", "blocked"))
        self.assertEqual(row.datum, "since 04:00")
        for part in ("review-7", "2026-11-07 04:00Z", "No live owner", "fleet-queue cancel review-7 --revision 12"):
            self.assertIn(part, row.detail)
        self.assertEqual([k for k in rows({"hosts": {"hub": host(queue={"stalls": []})}}) if k.startswith("queue:")], [])

    def test_probe_reads_the_queue_summary(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            with patch.object(probe, "HOME", home):
                self.assertIsNone(probe._queue())
                folder = home / ".local/state/fleet/jobs"
                folder.mkdir(parents=True)
                (folder / "summary.json").write_text(json.dumps(
                    {"observed_at": NOW_S, "jobs": [{"id": "big"}], "dispatch_stalls": [self.STALL]}))
                self.assertEqual(probe._queue(), {"observed_at": NOW_S, "stalls": [self.STALL]})
        row = collect._host_snapshot("hub", {"queue": {"observed_at": NOW_S, "stalls": [self.STALL]}}, {}, config(), {}, NOW_S)
        self.assertEqual(row["queue"]["stalls"][0]["id"], "review-7")


class Cadence(unittest.TestCase):
    def plist(self, home, label, args, **extra):
        agents = home / "Library/LaunchAgents"
        agents.mkdir(parents=True, exist_ok=True)
        with (agents / f"{label}.plist").open("wb") as handle:
            plistlib.dump(dict({"Label": label, "ProgramArguments": args}, **extra), handle)

    def test_summary_says_daily_and_when_the_next_one_is(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            wrapper = ["/bin/zsh", "/h/.local/bin/utc-daily", "--label", "com.example.fleet-morning", "--at", "22:30", "--"]
            self.plist(home, "com.example.fleet-morning-follow", wrapper + ["/h/bin/fleet", "morning", "follow"], StartInterval=300)
            self.plist(home, "com.example.fleet-snapshot", ["/usr/bin/python3", "/h/bin/fleet", "morning", "collect", "--local"], StartInterval=3600)
            self.plist(home, "com.example.fleet-morning", wrapper + ["/usr/bin/python3", "/h/bin/fleet", "morning"], StartInterval=300)
            found = cli._cadence(home, NOW_S, {})
            self.assertEqual(found["every"], "daily")
            self.assertEqual(found["at"], ["22:30Z"])
            self.assertEqual(found["next"], int(datetime(2026, 11, 7, 22, 30, tzinfo=timezone.utc).timestamp()))
            self.assertEqual(found["text"], "daily, next at 22:30Z")
            self.assertEqual(found["label"], "com.example.fleet-morning")
            # After today's slot the next one is tomorrow's.
            late = cli._cadence(home, NOW_S + 14 * HOUR, {})
            self.assertEqual(late["next"], found["next"] + 24 * HOUR)

    def test_the_job_that_is_running_names_itself(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            self.plist(home, "com.example.report", ["/h/bin/fleet", "morning"], StartInterval=1800)
            self.plist(home, "com.example.other", ["/h/bin/fleet", "morning"], StartInterval=7200)
            found = cli._cadence(home, NOW_S, {"XPC_SERVICE_NAME": "com.example.other"})
            self.assertEqual((found["every"], found["next"], found["text"]), ("every 2h", NOW_S + 7200, "every 2h"))
            self.assertIsNone(cli._cadence(Path(tmp) / "nowhere", NOW_S, {}))

    def test_cadence_is_written_into_the_summary(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            self.plist(home, "com.example.fleet-morning",
                       ["/bin/zsh", "/h/utc-daily", "--label", "com.example.fleet-morning", "--at", "22:30", "--", "/h/fleet", "morning"],
                       StartInterval=300)
            snap = root / "snap.json"
            snap.write_text(json.dumps({"hosts": {}, "projects": []}))
            keys = ("COWORK_STATE", "COWORK_CONFIG", "COWORK_LOCAL", "COWORK_FLUSH", "COWORK_NOW", "XPC_SERVICE_NAME")
            saved = {key: os.environ.get(key) for key in keys}
            os.environ.update(COWORK_STATE=str(root / "cowork"), COWORK_CONFIG=str(root / "none.json"),
                              COWORK_LOCAL="1", COWORK_FLUSH="0", COWORK_NOW=str(NOW_S))
            os.environ.pop("XPC_SERVICE_NAME", None)
            try:
                code = cli.main(["morning", "--snapshot", str(snap), "--state", str(root / "state"),
                                 "--board", str(root / "items.json"), "--here", "hub", "--home", str(home),
                                 "--now", str(NOW_S)], notify=lambda *_: None)
            finally:
                for key, value in saved.items():
                    if value is None:
                        os.environ.pop(key, None)
                    else:
                        os.environ[key] = value
            self.assertEqual(code, 0)
            summary = json.loads((root / "state/morning/summary.json").read_text())
            self.assertEqual(summary["cadence"]["text"], "daily, next at 22:30Z")
            self.assertEqual(summary["generated"], NOW_S)


class AuditFixes(unittest.TestCase):
    def test_check_result_respects_where_host(self):
        from fleet.morning.discover import Check
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            res = home / ".local/state/fleet/found/check.json"
            res.parent.mkdir(parents=True)
            res.write_text(json.dumps({"status": "fail", "value": "25/59", "datum": "search", "detail": "bad", "generated": NOW_S}))
            spec = Check("fleet", None, 120, "primary", False, "~/.local/state/fleet/found/check.json", 26)
            # Non-matching host must return None instead of shadowing the designated host's pass.
            self.assertIsNone(collect._check_result(spec, home / "Projects/fleet", home, "hub", NOW_S, config()))
            # Designated host reads the file.
            got = collect._check_result(spec, home / "Projects/fleet", home, collect.site.primary(), NOW_S, config())
            self.assertIsNotNone(got)
            self.assertEqual(got["status"], "fail")

    def test_running_job_with_stable_uptime_is_not_failed_by_prior_exit(self):
        snap = {
            "hosts": {
                "primary": host(jobs=[
                    {"label": "com.example.stable", "loaded": True, "state": "running", "exit": -9, "uptime_s": 3600, "when": "always on"},
                    {"label": "com.example.crashloop", "loaded": True, "state": "running", "exit": 1, "uptime_s": 12, "when": "always on"},
                    {"label": "com.example.dead", "loaded": True, "state": "idle", "exit": 1, "when": "every 5m"},
                ])
            }
        }
        r = rows(snap)
        self.assertEqual(r["job:primary:stable"].status, "pass")
        self.assertEqual(r["job:primary:crashloop"].status, "fail")
        self.assertEqual(r["job:primary:dead"].status, "fail")
        merged = collect._merge_jobs({}, [{"label": "com.example.stable", "loaded": True, "state": "running", "exit": -9, "uptime_s": 3600}])
        self.assertEqual(merged[0]["uptime_s"], 3600)


if __name__ == "__main__":
    unittest.main()
