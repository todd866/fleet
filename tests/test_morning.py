#!/usr/bin/env python3
"""fleet morning: thresholds, an absent Mac, a compact digest, deduped work, check discovery.

Each test names a production break: a threshold on the wrong side of its
config value, an unreachable Mac reported as a disk failure, a digest row
that wraps or adds a header, a second work item for the same key, or a
project check that is skipped or run on the active Mac.
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from fleet.morning import (  # noqa: E402
    discover,
    evaluate,
    file_items,
    load_config,
    notification_text,
    plan_check,
    render,
    work_path,
)
from fleet.morning.cli import main  # noqa: E402
from fleet.morning.discover import Check  # noqa: E402


NOW = datetime(2026, 11, 7, 9, 0, tzinfo=timezone.utc)
NOW_S = int(NOW.timestamp())
HOUR = 3600


def config():
    return {
        "hosts": ["primary", "hub", "shared"],
        "hub": "hub",
        "notify_host": "primary",
        "tz": "UTC",
        "disk": {
            "floor_gb": 40,
            "fail_ratio": 0.5,
            "trend_warn_gb": -20,
            "trend_fail_gb": -80,
            "floors": {"primary": {"internal": 80}},
        },
        "backups": {
            "idrive_warn_h": 36,
            "idrive_fail_h": 72,
            "drive_mirror_warn_h": 26,
            "drive_mirror_fail_h": 48,
            "hdd_backup_warn_h": 36,
            "hdd_backup_fail_h": 168,
            "icloud_stall_h": 6,
        },
        "jobs": {"overdue_h": 2},
        "thermal": {
            "hot_c": 60,
            "hot_hours_warn": 2,
            "hot_hours_fail": 6,
            "wait_warn_s": 600,
            "gap_s": 900,
        },
        "capacity": {"near_empty_pct": 10, "near_empty_jobs": 5},
        "agents": {"stale_claim_h": 6, "unread_mail_h": 12},
        "projects": {"heavy_runtime_s": 60},
    }


def host(**extra):
    base = {"ok": True}
    base.update(extra)
    return base


def by_key(findings):
    return {item.key: item for item in findings}


class Thresholds(unittest.TestCase):
    # A floor, age, exit code, or pool compared the wrong way, or against a
    # number other than the one in config, leaves the wrong status on that key.
    def status(self, snapshot, key):
        found = by_key(evaluate(snapshot, config(), NOW_S))
        self.assertIn(key, found, sorted(found))
        return found[key].status

    def test_disk_free_and_trend_use_the_configured_lines(self):
        floor = 40 * 1e9
        cases = [
            (floor, -1e9, "pass"),
            (30e9, -1e9, "warn"),
            (20e9, -1e9, "warn"),
            (19e9, -1e9, "fail"),
            (200e9, -20e9, "warn"),
            (200e9, -80e9, "fail"),
            (200e9, -90e9, "fail"),
        ]
        for free, trend, want in cases:
            snap = {"hosts": {"hub": host(disks=[{
                "name": "internal", "free_bytes": free, "trend_h24": trend,
            }])}}
            self.assertEqual(self.status(snap, "disk:hub:internal"), want, (free, trend))

    def test_primary_disk_uses_its_own_floor(self):
        snap = {"hosts": {"primary": host(disks=[{
            "name": "internal", "free_bytes": 50e9, "trend_h24": 0,
        }])}}
        self.assertEqual(self.status(snap, "disk:primary:internal"), "warn")

    def test_an_idrive_hold_is_a_warning_however_old_the_last_session(self):
        held = {"last_epoch": NOW_S - 200 * HOUR, "failed": 3, "held_since": "2026-11-11 17:19:29"}
        self.assertEqual(self.status({"hosts": {"hub": host(idrive=held)}}, "backup:hub:idrive"), "warn")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(idrive={"last_epoch": None, "error": "no local IDrive summary",
                                           "held_since": "2026-11-11"})}}, "backup:hub:idrive"), "warn")

    def test_idrive_ingest_daily_backup(self):
        # The Linux-client backup on the hub: a fresh success passes; a stale tick, a VM or
        # container down, or no success for the fail age fails; a first long seed is a warning
        # until it has run past the fail age; failed files in the last success warn.
        def ingest(**extra):
            base = {"checked_at": NOW_S - 600, "running": False, "problems": [],
                    "last_success_epoch": NOW_S - 5 * HOUR,
                    "last_success": {"considered": 1000, "failed": 0}, "storage": {"used_gb": 3500, "total_gb": 5000}}
            base.update(extra)
            return {"hosts": {"hub": host(idrive_ingest=base)}}
        key = "backup:hub:idrive-ingest"
        self.assertEqual(self.status(ingest(), key), "pass")
        self.assertEqual(self.status(ingest(last_success_epoch=NOW_S - 40 * HOUR), key), "warn")
        self.assertEqual(self.status(ingest(last_success_epoch=NOW_S - 80 * HOUR), key), "fail")
        self.assertEqual(self.status(ingest(checked_at=NOW_S - 3 * HOUR), key), "fail")
        self.assertEqual(self.status(ingest(problems=["VM Stopped"]), key), "fail")
        self.assertEqual(self.status(ingest(last_success={"considered": 1000, "failed": 4}), key), "warn")
        seeding = dict(last_success_epoch=None, last_success=None, running=True, started_at=NOW_S - 30 * HOUR,
                       progress={"scanned": 5000})
        self.assertEqual(self.status(ingest(**seeding), key), "warn")
        seeding["started_at"] = NOW_S - 8 * 24 * HOUR
        self.assertEqual(self.status(ingest(**seeding), key), "fail")
        self.assertEqual(self.status(ingest(last_success_epoch=None, last_success=None), key), "fail")
        # Coverage gaps turn a fresh success amber: an unplaced drive, a missing source, a count gap.
        self.assertEqual(self.status(ingest(coverage={"unplaced": ["/Volumes/New"]}), key), "warn")
        self.assertEqual(self.status(ingest(sources=[{"name": "A", "missing": True, "waiting": None}]), key), "warn")
        self.assertEqual(self.status(ingest(sources=[{"name": "A", "missing": True, "waiting": "after x"}]), key), "pass")
        self.assertEqual(self.status(ingest(coverage={"gap": 0.02}), key), "warn")
        self.assertEqual(self.status(ingest(coverage={"gap": 0.001}), key), "pass")
        # A Mac with no ingest status file gets no row at all.
        self.assertNotIn(key, by_key(evaluate({"hosts": {"hub": host()}}, config(), NOW_S)))

    def test_diskmap_without_full_disk_access_is_one_row(self):
        key = "disk:hub:diskmap-access"
        trees = [{"path": "~u/Desktop", "bytes": 12_000_000_000}, {"path": "/Volumes/Ext1", "bytes": 1}]
        snap = lambda access: {"hosts": {"hub": host(diskmap_access=access)}}
        row = by_key(evaluate(snap({"full_disk": False, "checked_at": NOW_S, "unmeasured": trees}), config(), NOW_S))[key]
        self.assertEqual(row.status, "warn")
        self.assertIn("diskmap: 2 trees unmeasured, switch on Full Disk Access for diskmap to measure them", row.detail)
        self.assertIn("~u/Desktop 12 GB", row.detail)
        for access in ({"full_disk": True, "unmeasured": []}, None):
            self.assertNotIn(key, by_key(evaluate(snap(access), config(), NOW_S)))

    def test_home_mirror_ages_and_skips(self):
        key = "backup:hub:home-mirror"
        snap = lambda **s: {"hosts": {"hub": host(home_mirror=s)}}
        self.assertEqual(self.status(snap(last_success_epoch=NOW_S - 5 * HOUR, state="ok"), key), "pass")
        self.assertEqual(self.status(snap(last_success_epoch=NOW_S - 60 * HOUR, state="skipped: metered link"), key), "warn")
        self.assertEqual(self.status(snap(last_success_epoch=NOW_S - 200 * HOUR, state="ok"), key), "fail")
        self.assertEqual(self.status(snap(last_success_epoch=NOW_S - 5 * HOUR, state="failed: rsync exit 23"), key), "warn")
        self.assertEqual(self.status(snap(state="skipped: on battery"), key), "warn")
        self.assertEqual(self.status(snap(state="failed: rsync exit 12"), key), "fail")

    def test_backup_ages_idrive_mirror_hdd_and_icloud(self):
        fresh = NOW_S - HOUR
        self.assertEqual(self.status(
            {"hosts": {"hub": host(idrive={"last_epoch": fresh})}},
            "backup:hub:idrive"), "pass")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(idrive={"last_epoch": NOW_S - 72 * HOUR})}},
            "backup:hub:idrive"), "fail")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(idrive={"last_epoch": NOW_S - 40 * HOUR})}},
            "backup:hub:idrive"), "warn")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(idrive=None)}},
            "backup:hub:idrive"), "fail")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(drive_mirror_epoch=NOW_S - 30 * HOUR)}},
            "backup:hub:drive-mirror"), "warn")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(drive_mirror_epoch=NOW_S - 48 * HOUR)}},
            "backup:hub:drive-mirror"), "fail")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(hdd_backup_epoch=fresh)}},
            "backup:hub:hdd-backup"), "pass")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(hdd_backup_epoch=NOW_S - 200 * HOUR)}},
            "backup:hub:hdd-backup"), "fail")
        self.assertEqual(self.status(
            {"hosts": {"primary": host(icloud={
                "pending_upload": 4, "last_sync_epoch": NOW_S - 7 * HOUR,
            })}},
            "backup:primary:icloud"), "warn")
        self.assertEqual(self.status(
            {"hosts": {"primary": host(icloud={
                "pending_upload": 0, "last_sync_epoch": fresh,
            })}},
            "backup:primary:icloud"), "pass")

    def test_syncthing_errors_sync_and_receive_only(self):
        self.assertEqual(self.status(
            {"hosts": {"hub": host(syncthing={"errors": 2, "out_of_sync": 0, "receive_only_local": 0})}},
            "sync:hub"), "fail")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(syncthing={"errors": 0, "out_of_sync": 1, "receive_only_local": 0})}},
            "sync:hub"), "warn")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(syncthing={"errors": 0, "out_of_sync": 0, "receive_only_local": 3})}},
            "sync:hub"), "fail")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(syncthing={"errors": 0, "out_of_sync": 0, "receive_only_local": 0})}},
            "sync:hub"), "pass")

    def test_glancebar_behind_or_missing_a_release(self):
        self.assertEqual(self.status(
            {"hosts": {"primary": host(glancebar={"behind": 2, "release": "1.4", "installed": "1.2"})}},
            "update:primary"), "warn")
        self.assertEqual(self.status(
            {"hosts": {"primary": host(glancebar={"behind": 0, "release": None, "installed": "1.2"})}},
            "update:primary"), "warn")
        self.assertEqual(self.status(
            {"hosts": {"primary": host(glancebar={"behind": 0, "release": "1.2", "installed": "1.2"})}},
            "update:primary"), "pass")

    def test_launchd_failure_and_a_missed_daily_run(self):
        ran = {"label": "com.example.diskmap", "loaded": True, "exit": 0,
               "last_epoch": NOW_S - 600, "when": "daily 06:30"}
        self.assertEqual(self.status(
            {"hosts": {"hub": host(jobs=[ran])}}, "job:hub:diskmap"), "pass")
        failed = dict(ran, exit=1)
        self.assertEqual(self.status(
            {"hosts": {"hub": host(jobs=[failed])}}, "job:hub:diskmap"), "fail")
        missed = dict(ran, last_epoch=NOW_S - 26 * HOUR)
        self.assertEqual(self.status(
            {"hosts": {"hub": host(jobs=[missed])}}, "job:hub:diskmap"), "fail")
        unloaded = dict(ran, loaded=False)
        self.assertEqual(self.status(
            {"hosts": {"hub": host(jobs=[unloaded])}}, "job:hub:diskmap"), "fail")
        # Exit 75 is the job saying "not now" (a drive another job holds, a metered link): it
        # tries again at its next slot. A deferral is a warn that names itself, never a fail,
        # and it is a warn even while the job is running again.
        deferred = by_key(evaluate({"hosts": {"hub": host(jobs=[dict(ran, exit=75)])}}, config(), NOW_S))["job:hub:diskmap"]
        self.assertEqual((deferred.status, deferred.value), ("warn", "deferred"))
        text_code = by_key(evaluate({"hosts": {"hub": host(jobs=[dict(ran, exit="75")])}}, config(), NOW_S))["job:hub:diskmap"]
        self.assertEqual(text_code.status, "warn")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(jobs=[dict(ran, exit=74)])}}, "job:hub:diskmap"), "fail")

    def test_interval_job_older_than_two_intervals_has_not_run(self):
        job = {"label": "com.example.diskmap", "loaded": True, "exit": 0,
               "last_epoch": NOW_S - 2 * HOUR, "when": "every 30m"}
        self.assertEqual(self.status(
            {"hosts": {"hub": host(jobs=[job])}}, "job:hub:diskmap"), "fail")
        job["last_epoch"] = NOW_S - 20 * 60
        self.assertEqual(self.status(
            {"hosts": {"hub": host(jobs=[job])}}, "job:hub:diskmap"), "pass")

    def test_heavy_slot_failures_starvation_and_hot_hours(self):
        self.assertEqual(self.status(
            {"hosts": {"hub": host(heavy=[{"label": "sim", "rc": 1, "waited": 0, "end": NOW_S}])}},
            "thermal:hub"), "warn")
        # stopped on purpose: not a failure at all
        self.assertEqual(self.status(
            {"hosts": {"hub": host(heavy=[{"label": "sim", "rc": 143, "waited": 0, "end": NOW_S}])}},
            "thermal:hub"), "pass")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(heavy=[{"label": "sim", "rc": 75, "waited": 3, "end": NOW_S}])}},
            "thermal:hub"), "warn")
        self.assertEqual(self.status(
            {"hosts": {"hub": host(heavy=[{"label": "sim", "rc": 0, "waited": 900, "end": NOW_S}])}},
            "thermal:hub"), "warn")
        hot = [{"t": NOW_S - (180 - i) * 60, "chipC": 71, "thermal": "nominal"} for i in range(181)]
        self.assertEqual(self.status(
            {"hosts": {"hub": host(samples=hot)}}, "thermal:hub"), "warn")
        cool = [{"t": NOW_S - 60, "chipC": 42, "thermal": "nominal"}]
        self.assertEqual(self.status(
            {"hosts": {"hub": host(samples=cool, heavy=[{"label": "sim", "rc": 0, "waited": 1, "end": NOW_S}])}},
            "thermal:hub"), "pass")

    def test_capacity_near_empty_only_while_the_window_is_still_open(self):
        future = NOW_S + 5 * HOUR
        past = NOW_S - HOUR

        def pool(**extra):
            base = {"engine": "claude", "pool": "7d", "remaining_pct": 40,
                    "jobs_left": None, "reset_epoch": future, "stale": False}
            base.update(extra)
            return {"capacity": {"pools": [base]}}

        low = by_key(evaluate(pool(remaining_pct=4), config(), NOW_S))["capacity:claude:7d"]
        self.assertEqual(low.status, "warn")
        self.assertEqual(low.value, "4%")
        self.assertTrue(low.datum.startswith("resets "))
        empty = by_key(evaluate(pool(remaining_pct=0), config(), NOW_S))["capacity:claude:7d"]
        self.assertEqual(empty.status, "fail")
        stale = by_key(evaluate(pool(remaining_pct=4, reset_epoch=past), config(), NOW_S))["capacity:claude:7d"]
        self.assertEqual(stale.status, "warn")
        self.assertEqual(stale.value, "stale")
        self.assertNotIn("4%", stale.value)
        jobs = by_key(evaluate(pool(jobs_left=2), config(), NOW_S))["capacity:claude:7d"]
        self.assertEqual(jobs.status, "warn")
        unread = by_key(evaluate(pool(remaining_pct=None, stale=True), config(), NOW_S))["capacity:claude:7d"]
        self.assertEqual(unread.value, "unread")
        fine = by_key(evaluate(pool(), config(), NOW_S))["capacity:claude:7d"]
        self.assertEqual(fine.status, "pass")
        absent = {"capacity": {"pools": [{"engine": "claude", "pool": "5h", "remaining_pct": None,
                  "jobs_left": None, "reset_epoch": None, "stale": False, "absent": True}]}}
        none = by_key(evaluate(absent, config(), NOW_S))["capacity:claude:5h"]
        self.assertEqual((none.status, none.value), ("pass", "none"))

    def test_a_guard_turned_off_in_site_config_is_info_not_a_failure(self):
        guards = [{"name": "hard-delete", "installed": False, "trusted": False, "firing": False, "detail": "disabled (site)"},
                  {"name": "push", "installed": False, "trusted": True, "firing": False, "detail": ""}]
        kit = {"ok": True, "issues": [], "conformance": {"providers": [{"id": "claude", "guards": guards}]}}
        found = by_key(evaluate({"agentkit": kit}, config(), NOW_S))
        self.assertEqual((found["kit:claude:hard-delete"].status, found["kit:claude:hard-delete"].value), ("info", "off"))
        self.assertEqual(found["kit:claude:push"].status, "fail")

    def test_agent_board_mail_and_project_defaults(self):
        stale = {"work": "export", "state": "stale", "heartbeat": NOW_S - 10 * HOUR,
                 "ttl": HOUR, "conflicts": [], "owner": {"session": "abc"}}
        clash = {"work": "mirror", "state": "active", "heartbeat": NOW_S, "ttl": HOUR,
                 "conflicts": [{"work": "mirror-2", "mail": "hub:fleet"}],
                 "owner": {"session": "def"}}
        board = {"claims": [stale, clash], "presence": [
            {"id": "legacy", "event": "activity", "at": NOW_S, "state": "active"},
        ]}
        found = by_key(evaluate({"board": board}, config(), NOW_S))
        self.assertEqual(found["agent:stale"].status, "warn")
        self.assertEqual(found["agent:conflict"].status, "fail")
        self.assertEqual(found["agent:session"].status, "warn")

        recent = {"hosts": {"hub": host(mail=[{"at": NOW_S - HOUR, "read": False}])}}
        self.assertEqual(self.status(recent, "agent:mail:hub"), "pass")
        old = {"hosts": {"hub": host(mail=[{"at": NOW_S - 13 * HOUR, "read": False}])}}
        mail = by_key(evaluate(old, config(), NOW_S))["agent:mail:hub"]
        self.assertEqual(mail.status, "warn")
        self.assertIn("13 h", mail.datum)

        clean = {"name": "selector", "git": {"dirty": False, "ahead": 0, "behind": 0, "unpushed": 0},
                 "ci": {"conclusion": "success"}, "jobs": []}
        self.assertEqual(self.status({"projects": [clean]}, "project:selector"), "pass")
        dirty = dict(clean, git={"dirty": True, "ahead": 2, "behind": 0, "unpushed": 2})
        row = by_key(evaluate({"projects": [dirty]}, config(), NOW_S))["project:selector"]
        self.assertEqual(row.status, "warn")
        self.assertEqual(row.value, "dirty")
        ci = dict(clean, ci={"conclusion": "failure"})
        self.assertEqual(self.status({"projects": [ci]}, "project:selector"), "fail")
        job = dict(clean, jobs=[{"label": "com.example.selector-worker", "exit": 2}])
        self.assertEqual(self.status({"projects": [job]}, "project:selector"), "fail")
        # The repo's own check is its own row: a failing check does not replace the project
        # row, and a passing check does not hide a dirty checkout.
        custom = dict(clean, check_result={"status": "fail", "value": "fail", "datum": "deck", "detail": "2 failed"})
        found = by_key(evaluate({"projects": [custom]}, config(), NOW_S))
        self.assertEqual(found["check:selector"].status, "fail")
        self.assertEqual(found["check:selector"].datum, "deck")
        self.assertEqual(found["project:selector"].status, "pass")
        hidden = dict(dirty, check_result={"status": "pass", "value": "ok", "datum": "media", "detail": ""})
        found = by_key(evaluate({"projects": [hidden]}, config(), NOW_S))
        self.assertEqual(found["check:selector"].status, "pass")
        self.assertEqual(found["project:selector"].status, "warn")
        # Declared, and no Mac has written it: said once, as a warn, not left out.
        silent = dict(clean, check_declared=True, check_result=None)
        missing = by_key(evaluate({"projects": [silent]}, config(), NOW_S))["check:selector"]
        self.assertEqual((missing.status, missing.value), ("warn", "missing"))
        self.assertNotIn("check:selector", by_key(evaluate({"projects": [clean]}, config(), NOW_S)))
        # A status the digest does not know is not a pass.
        odd = dict(clean, check_result={"status": "green", "value": "ok"})
        self.assertEqual(self.status({"projects": [odd]}, "check:selector"), "fail")


class AbsentHost(unittest.TestCase):
    # An unreachable Mac must show "away since …" and must not grow a disk
    # or backup failure for readings that were never collected.
    def test_unreachable_host_is_away_since_and_skips_its_instruments(self):
        snap = {"hosts": {
            "shared": {"ok": False, "away_since": int(datetime(2026, 11, 7, 2, 10, tzinfo=timezone.utc).timestamp()),
                       "error": "timed out"},
            "hub": host(disks=[{"name": "internal", "free_bytes": 200e9, "trend_h24": 0}]),
        }}
        found = by_key(evaluate(snap, config(), NOW_S))
        away = found["host:shared"]
        self.assertEqual(away.status, "warn")
        self.assertEqual(away.symbol, "?")
        self.assertEqual(away.value, "away")
        self.assertEqual(away.datum, "since 02:10")
        self.assertNotIn("disk:shared:internal", found)
        self.assertNotIn("backup:hub:idrive", found)

    def test_away_on_an_earlier_day_names_that_day(self):
        snap = {"hosts": {"shared": {
            "ok": False,
            "away_since": int(datetime(2026, 11, 6, 22, 0, tzinfo=timezone.utc).timestamp()),
        }}}
        away = by_key(evaluate(snap, config(), NOW_S))["host:shared"]
        self.assertEqual(away.datum, "since 6 Nov")

    def test_away_with_no_last_contact_says_so(self):
        snap = {"hosts": {"shared": {"ok": False, "away_since": None}}}
        away = by_key(evaluate(snap, config(), NOW_S))["host:shared"]
        self.assertEqual(away.datum, "no contact")


class DigestWidth(unittest.TestCase):
    # Failures lead, passes collapse to one line, and a long name is not clipped.
    # Pass details stay in the JSON.
    def test_rows_share_columns_and_keep_full_names(self):
        findings = evaluate({
            "hosts": {
                "hub": host(disks=[{"name": "internal", "free_bytes": 213e9, "trend_h24": -2.1e9}]),
                "shared": {"ok": False, "away_since": int(datetime(2026, 11, 7, 2, 10, tzinfo=timezone.utc).timestamp())},
            },
            "projects": [{
                "name": "glancebar",
                "git": {"dirty": False, "ahead": 0, "behind": 0, "unpushed": 0},
                "ci": None,
                "jobs": [],
                "check_result": {"status": "fail", "value": "fail", "datum": "morning check", "detail": "tests failed on the worker"},
            }],
        }, config(), NOW_S)
        text, doc = render(findings, generated=NOW_S, date="2026-11-07")
        self.assertNotIn("…", text)
        instruments = []
        summary = None
        for line in text.splitlines():
            if not line or line[0] not in "*~!?":
                continue
            if line.endswith("checks passed"):
                summary = line
                continue
            head = line[2:].split("  ", 1)[0]
            if ":" in head:
                continue
            instruments.append(line)
        self.assertIsNotNone(summary)
        self.assertEqual(instruments[0][0], "!")
        self.assertEqual(instruments[-1][0], "?")
        self.assertNotIn("hub internal", text)
        stops = [line.rfind("  ") for line in instruments]
        self.assertTrue(all(stop > 0 for stop in stops), instruments)
        self.assertEqual(len(set(stops)), 1, instruments)
        self.assertTrue(all("\n" not in line for line in instruments))
        lowered = text.lower()
        self.assertNotIn("needs attention", lowered)
        self.assertNotIn("all clear", lowered)
        self.assertFalse(any(line.startswith("#") for line in text.splitlines()))
        fail_keys = [item.key for item in findings if item.status == "fail"]
        self.assertTrue(fail_keys)
        for key in fail_keys:
            self.assertIn(key, text)
        for item in findings:
            if item.status == "pass":
                self.assertNotIn(item.key, text)
                self.assertNotIn(item.detail, text)
        self.assertEqual(doc["version"], 1)
        self.assertEqual(len(doc["rows"]), len(findings))
        self.assertEqual({row["symbol"] for row in doc["rows"]}, {item.symbol for item in findings})
        self.assertFalse(doc["ok"])
        note = notification_text(findings)
        self.assertIsNotNone(note)
        self.assertNotIn("\n", note)
        self.assertLessEqual(len(note), 80)
        self.assertIn("fail", note)
        self.assertIsNone(notification_text([item for item in findings if item.status == "pass"]))


class WorkItems(unittest.TestCase):
    # Filing the same warn twice, or filing a pass, must not grow a second open item.
    def finding(self, status="warn", key="backup:hub:idrive"):
        found = evaluate(
            {"hosts": {"hub": host(idrive={"last_epoch": NOW_S - (40 if status == "warn" else 80) * HOUR})}},
            config(), NOW_S)
        row = by_key(found)[key]
        self.assertEqual(row.status, "fail" if status == "fail" else "warn")
        return row

    def test_same_key_stays_one_open_item(self):
        warn = self.finding("warn")
        first = file_items([warn], [], 100)
        second = file_items([warn], first, 200)
        self.assertEqual(len(second), 1)
        self.assertEqual(second[0]["key"], "backup:hub:idrive")
        self.assertEqual(second[0]["state"], "open")
        self.assertEqual(second[0]["opened"], 100)
        self.assertEqual(second[0]["updated"], 200)
        again = file_items([warn, warn], second, 300)
        self.assertEqual(len([item for item in again if item["state"] == "open"]), 1)

    def test_a_pass_does_not_open_work_and_closes_the_old_item(self):
        warn = self.finding("warn")
        opened = file_items([warn], [], 100)
        healthy = evaluate(
            {"hosts": {"hub": host(idrive={"last_epoch": NOW_S - HOUR})}},
            config(), NOW_S)
        closed = file_items(healthy, opened, 400)
        self.assertEqual(len(closed), 1)
        self.assertEqual(closed[0]["state"], "done")
        self.assertEqual(closed[0]["key"], "backup:hub:idrive")
        nothing = file_items(healthy, [], 400)
        self.assertEqual(nothing, [])

    def test_cowork_board_wins_over_taskhub_when_present(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cowork = root / "cowork"
            cowork.mkdir()
            (cowork / "board.json").write_text("{}\n")
            self.assertEqual(work_path(root), cowork / "inbox.json")
            bare = root / "empty"
            bare.mkdir()
            self.assertEqual(work_path(bare), bare / "taskhub" / "items.json")


class ProjectChecks(unittest.TestCase):
    # A missing .fleet/check must not invent a command, and a declared one
    # must keep its runtime and where. A long check must not be planned on
    # the active primary.
    def test_discovers_a_repo_check_and_defaults_the_rest(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            projects = root / "Projects"
            check = projects / "glancebar" / ".fleet"
            check.mkdir(parents=True)
            (check / "check").write_text(
                'command = "tools/ops/menubar-morning-check.sh --quick"\n'
                "max_runtime_s = 180\n"
                'where = "hub"\n')
            (projects / "selector").mkdir(parents=True)
            (projects / "old").mkdir(parents=True)
            registry = root / "registry"
            registry.mkdir()
            (registry / "descriptions.tsv").write_text(
                "glancebar\tapp\tactive\tmenu bar\n"
                "selector\tapp\tactive\tdeck\n"
                "old\tapp\tarchive\told\n")
            found = {item.name: item for item in discover(registry, projects)}
            self.assertEqual(found["glancebar"].command, "tools/ops/menubar-morning-check.sh --quick")
            self.assertEqual(found["glancebar"].max_runtime_s, 180)
            self.assertEqual(found["glancebar"].where, "hub")
            self.assertFalse(found["glancebar"].default)
            self.assertTrue(found["selector"].default)
            self.assertIsNone(found["selector"].command)
            self.assertNotIn("old", found)

    # A repo's own check reaches the digest only through the result file its `.fleet/check`
    # names. Each case below is a way that file could lie: absent, old, broken, or unknown.
    def test_check_file_names_a_result_and_a_broken_file_is_reported(self):
        from fleet.morning.discover import UNREADABLE
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            projects = root / "Projects"
            for name, text in (
                ("media", 'command = "tools/check"\nresult = "~/.local/state/media/check.json"\nmax_age_h = 12\n'),
                ("resultonly", 'result = "state/check.json"\n'),
                ("broken", 'command = "unterminated\n'),
            ):
                folder = projects / name / ".fleet"
                folder.mkdir(parents=True)
                (folder / "check").write_text(text)
            registry = root / "registry"
            registry.mkdir()
            (registry / "descriptions.tsv").write_text("media\tapp\tactive\tmusic\n")
            found = {item.name: item for item in discover(registry, projects)}
            self.assertEqual(found["media"].result, "~/.local/state/media/check.json")
            self.assertEqual(found["media"].max_age_h, 12)
            self.assertEqual(found["media"].command, "tools/check")
            self.assertIsNone(found["resultonly"].command)
            self.assertEqual(found["resultonly"].result, "state/check.json")
            self.assertIsNone(found["resultonly"].max_age_h)
            self.assertEqual(found["broken"].result, UNREADABLE)

    def test_result_file_is_read_and_never_trusted_when_absent_old_or_broken(self):
        from fleet.morning.collect import _check_result
        from fleet.morning.discover import UNREADABLE
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            repo = home / "Projects" / "media"
            repo.mkdir(parents=True)
            state = home / ".local" / "state" / "media"
            state.mkdir(parents=True)
            target = state / "check.json"
            spec = Check("media", "tools/check", 30, "any", False, "~/.local/state/media/check.json", None)

            def read(check=spec, cfg=None, now=NOW_S):
                return _check_result(check, repo, home, "primary", now, cfg or {})

            # No file on this Mac: nothing to say (another Mac may run the check).
            self.assertIsNone(read())
            self.assertIsNone(_check_result(Check("plain", None, None, "any", True), repo, home, "primary", NOW_S, {}))

            body = {"version": 1, "generated": NOW_S - HOUR, "status": "fail", "value": "1 fail",
                    "datum": "upload", "detail": "upload: failed 36 h ago", "rows": [{"key": "media:upload"}]}
            target.write_text(json.dumps(body))
            got = read()
            self.assertEqual((got["status"], got["value"], got["datum"]), ("fail", "1 fail", "upload"))
            self.assertEqual(got["detail"], "upload: failed 36 h ago")
            self.assertEqual((got["generated"], got["host"]), (NOW_S - HOUR, "primary"))
            self.assertNotIn("rows", got)

            # 30 h is the default limit: a result on the limit still counts, one past it does not.
            target.write_text(json.dumps(dict(body, status="pass", generated=NOW_S - 30 * HOUR)))
            self.assertEqual(read()["status"], "pass")
            target.write_text(json.dumps(dict(body, status="pass", detail="", generated=NOW_S - 31 * HOUR)))
            stale = read()
            self.assertEqual((stale["status"], stale["value"]), ("warn", "stale"))
            self.assertTrue(stale["datum"].startswith("since "), stale)
            self.assertIn("31 h ago", stale["detail"])
            self.assertIn("last result pass", stale["detail"])
            # The repo's own limit wins over the default, and config moves the default.
            tight = Check("media", None, None, "any", False, "~/.local/state/media/check.json", 0.5)
            target.write_text(json.dumps(dict(body, status="pass")))
            self.assertEqual(read(tight)["value"], "stale")
            self.assertEqual(read(cfg={"projects": {"check_max_age_h": 0.5}})["value"], "stale")
            # No `generated`: the file's own time is the clock.
            target.write_text(json.dumps({"status": "pass"}))
            os.utime(target, (NOW_S - 40 * HOUR, NOW_S - 40 * HOUR))
            self.assertEqual(read()["value"], "stale")
            os.utime(target, (NOW_S - HOUR, NOW_S - HOUR))
            self.assertEqual((read()["status"], read()["value"], read()["datum"]), ("pass", "pass", "check"))

            for text in ("{not json", "[]", json.dumps({"status": "green"}), json.dumps({"value": "ok"})):
                target.write_text(text)
                bad = read()
                self.assertEqual((bad["status"], bad["value"]), ("warn", "unreadable"), text)
            broken = Check("media", None, None, "any", False, UNREADABLE)
            self.assertEqual(read(broken)["datum"], ".fleet/check")

            # Found in review: values that used to crash the collector or pass for ever.
            from fleet.morning.collect import _safe_check_result
            target.write_text('{"status": "pass", "generated": 1e309}')  # parses to infinity
            os.utime(target, (NOW_S - HOUR, NOW_S - HOUR))
            self.assertEqual(read()["status"], "pass")  # the file's own time is the clock
            os.utime(target, (NOW_S - 40 * HOUR, NOW_S - 40 * HOUR))
            self.assertEqual(read()["value"], "stale")
            # A date ahead of the clock can never go stale, so it is not a pass.
            target.write_text(json.dumps({"status": "pass", "generated": NOW_S + 10 * HOUR}))
            ahead = read()
            self.assertEqual((ahead["status"], ahead["value"]), ("warn", "unreadable"))
            self.assertIn("future", ahead["detail"])
            target.write_text(json.dumps({"status": "pass", "generated": NOW_S + 60}))  # clock skew
            self.assertEqual(read()["status"], "pass")
            # A config value that is not a number keeps the default limit.
            target.write_text(json.dumps({"status": "pass", "generated": NOW_S - 29 * HOUR}))
            self.assertEqual(read(cfg={"projects": {"check_max_age_h": "abc"}})["status"], "pass")
            target.write_text(json.dumps({"status": "pass", "generated": NOW_S - 31 * HOUR}))
            self.assertEqual(read(cfg={"projects": {"check_max_age_h": "abc"}})["value"], "stale")
            # No clock handed in: the wall clock is used, so an ancient result is still stale.
            target.write_text(json.dumps({"status": "pass", "generated": 1}))
            self.assertEqual(_check_result(spec, repo, home, "primary", None, {})["value"], "stale")
            # Anything unforeseen inside is a warn row, never an exception.
            odd = Check("media", None, None, "any", False, "~/.local/state/media/check.json", "soon")
            target.write_text(json.dumps({"status": "pass", "generated": NOW_S}))
            safe = _safe_check_result(odd, repo, home, "primary", NOW_S, {})
            self.assertEqual((safe["status"], safe["value"]), ("warn", "unreadable"))

            # A relative result is inside the repo.
            (repo / "state").mkdir()
            (repo / "state" / "check.json").write_text(json.dumps({"status": "warn", "generated": NOW_S}))
            inside = Check("media", None, None, "any", False, "state/check.json")
            self.assertEqual(read(inside)["status"], "warn")

    def test_two_macs_reading_one_check_show_the_worse_then_the_newer(self):
        from fleet.morning.snapshot import merge_snapshots
        def doc(host_id, result, declared=True):
            return {"host": host_id, "collected_at": NOW_S, "reading": {"ok": True},
                    "projects": [{"name": "media", "git": {}, "ci": None, "jobs": [],
                                  "check_declared": declared, "check_result": result}]}
        def merged(*docs):
            return merge_snapshots({item["host"]: item for item in docs}, None)["projects"][0]
        ok = {"status": "pass", "value": "ok", "generated": NOW_S - 10}
        bad = {"status": "fail", "value": "1 fail", "generated": NOW_S - 500}
        self.assertEqual(merged(doc("primary", ok), doc("hub", bad))["check_result"]["value"], "1 fail")
        self.assertEqual(merged(doc("hub", bad), doc("primary", ok))["check_result"]["value"], "1 fail")
        newer = {"status": "pass", "value": "newer", "generated": NOW_S}
        self.assertEqual(merged(doc("primary", ok), doc("hub", newer))["check_result"]["value"], "newer")
        # A Mac without the file says nothing; it does not erase the other's result or the declaration.
        only = merged(doc("shared", None, declared=False), doc("hub", bad))
        self.assertEqual(only["check_result"]["value"], "1 fail")
        self.assertTrue(only["check_declared"])
        self.assertIsNone(merged(doc("primary", None), doc("hub", None))["check_result"])

    def test_heavy_and_hub_checks_are_not_planned_on_the_active_primary(self):
        hub = Check("glancebar", "tools/ops/menubar-morning-check.sh --quick", 180, "hub", False)
        moved = plan_check(hub, here="primary", owner_active=True, heavy_host="hub",
                           active_host="primary", heavy_runtime_s=60)
        self.assertEqual(moved.argv[:2], ["remote-run", "--"])
        self.assertEqual(moved.host, "hub")
        self.assertFalse(moved.deferred)
        local = plan_check(hub, here="hub", owner_active=True, heavy_host="hub",
                           active_host="primary", heavy_runtime_s=60)
        self.assertEqual(local.argv[0], "tools/ops/menubar-morning-check.sh")
        self.assertEqual(local.host, "hub")
        self.assertNotIn("remote-run", local.argv)
        heavy = Check("selector", "deck-test --silent", 300, "any", False)
        off = plan_check(heavy, here="primary", owner_active=False, heavy_host="hub",
                         active_host="primary", heavy_runtime_s=60)
        self.assertEqual(off.host, "hub")
        self.assertEqual(off.argv[:2], ["remote-run", "--"])
        light = Check("proj", "gh run list --limit 1", 30, "any", False)
        stay = plan_check(light, here="primary", owner_active=False, heavy_host="hub",
                          active_host="primary", heavy_runtime_s=60)
        self.assertEqual(stay.host, "primary")
        self.assertEqual(stay.argv[0], "gh")
        needs_primary = Check("proj", "gh run list --limit 1", 30, "primary", False)
        deferred = plan_check(needs_primary, here="hub", owner_active=True, heavy_host="hub",
                              active_host="primary", heavy_runtime_s=60)
        self.assertTrue(deferred.deferred)
        self.assertEqual(deferred.argv, [])

    def test_shipped_example_config_is_the_threshold_source(self):
        cfg = load_config(ROOT / "docs" / "examples" / "morning.toml")
        self.assertEqual(cfg["disk"]["floor_gb"], 40)
        self.assertEqual(cfg["disk"]["trend_fail_gb"], -80)
        self.assertEqual(cfg["agents"]["unread_mail_h"], 12)
        self.assertEqual(cfg["backups"]["idrive_fail_h"], 72)
        self.assertEqual(cfg["hub"], "hub")
        self.assertEqual(cfg["notify_host"], "primary")


class CommandWiring(unittest.TestCase):
    # The command must write one digest and notify once, without a second item.
    def test_snapshot_writes_digest_files_and_notifies_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snap = root / "snap.json"
            snap.write_text(json.dumps({
                "hosts": {"shared": {"ok": False, "away_since": NOW_S - 3 * HOUR}},
                "projects": [],
            }))
            board = root / "taskhub" / "items.json"
            calls = []
            saved = {key: os.environ.get(key) for key in (
                "COWORK_STATE", "COWORK_CONFIG", "COWORK_LOCAL", "COWORK_FLUSH", "COWORK_NOW", "HOME")}
            os.environ["COWORK_STATE"] = str(root / "cowork-state")
            os.environ["COWORK_CONFIG"] = str(root / "missing-cowork.json")
            os.environ["COWORK_LOCAL"] = "1"
            os.environ["COWORK_FLUSH"] = "0"
            os.environ["COWORK_NOW"] = str(NOW_S)
            os.environ["HOME"] = str(root / "home")
            try:
                code = main([
                    "morning", "--snapshot", str(snap), "--state", str(root / "state"),
                    "--board", str(board), "--here", "hub", "--notify-host", "primary",
                    "--now", str(NOW_S), "--config", str(ROOT / "docs" / "examples" / "morning.toml"),
                ], notify=lambda host, body: calls.append((host, body)))
                self.assertEqual(code, 0)
                day = "2026-11-07"
                digest = (root / "state" / "morning" / f"{day}.md").read_text()
                summary = json.loads((root / "state" / "morning" / "summary.json").read_text())
                self.assertIn("away", digest)
                self.assertFalse(summary["ok"])
                self.assertEqual(len(calls), 1)
                # --state names a rehearsal directory: nothing goes to the synced folder.
                self.assertFalse((root / "home" / ".local" / "state" / "mac-sync" / "fleet-morning").exists())
                self.assertEqual(calls[0][0], "primary")
                self.assertNotIn("\n", calls[0][1])
                items = json.loads(board.read_text())["items"]
                self.assertEqual(len(items), 1)
                self.assertEqual(items[0]["key"], "host:shared")
                code = main([
                    "morning", "--snapshot", str(snap), "--state", str(root / "state"),
                    "--board", str(board), "--here", "hub", "--notify-host", "primary",
                    "--now", str(NOW_S), "--config", str(ROOT / "docs" / "examples" / "morning.toml"),
                ], notify=lambda host, body: calls.append((host, body)))
                self.assertEqual(code, 0)
                again = json.loads(board.read_text())["items"]
                self.assertEqual(len(again), 1)
                self.assertEqual(again[0]["opened"], items[0]["opened"])
            finally:
                for key, value in saved.items():
                    if value is None:
                        os.environ.pop(key, None)
                    else:
                        os.environ[key] = value


class SharedBoard(unittest.TestCase):
    # Findings used to land in a local inbox the hub board never reads.
    # Morning has to call cowork, which forwards to the hub and queues when it is away.
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.state = self.root / "state"
        self.cowork_state = self.root / "cowork"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.ssh_log = self.root / "ssh.log"
        ssh = self.bin / "ssh"
        ssh.write_text('#!/bin/sh\necho "$@" >> "$COWORK_SSH_LOG"\nexit 255\n')
        ssh.chmod(0o755)
        self._saved = {
            key: os.environ.get(key)
            for key in (
                "PATH", "HOME", "PYTHONPATH", "COWORK_STATE", "COWORK_CONFIG",
                "COWORK_HOST", "COWORK_LOCAL", "COWORK_FLUSH", "COWORK_NOW",
                "COWORK_SSH_LOG", "COWORK_PLAIN",
            )
        }

    def tearDown(self):
        for key, value in self._saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value

    def env(self):
        os.environ["PATH"] = str(self.bin) + os.pathsep + self._saved.get("PATH", "")
        os.environ["HOME"] = str(self.root / "home")
        os.environ["PYTHONPATH"] = str(ROOT)
        os.environ["COWORK_STATE"] = str(self.cowork_state)
        os.environ["COWORK_CONFIG"] = str(self.root / "cowork.json")
        os.environ["COWORK_HOST"] = "primary"
        os.environ["COWORK_FLUSH"] = "0"
        os.environ["COWORK_NOW"] = str(NOW_S)
        os.environ["COWORK_SSH_LOG"] = str(self.ssh_log)
        os.environ["COWORK_PLAIN"] = "1"
        os.environ.pop("COWORK_LOCAL", None)
        (self.root / "cowork.json").write_text(json.dumps({
            "hub": "hub", "hub_aliases": ["hub", "hub-lan"], "host": "primary",
        }))

    def morning(self, snapshot, board):
        return main([
            "morning", "--snapshot", str(snapshot), "--state", str(self.state),
            "--board", str(board), "--here", "primary", "--notify-host", "primary",
            "--now", str(NOW_S), "--config", str(ROOT / "docs" / "examples" / "morning.toml"),
        ], notify=lambda host, body: None)

    def test_findings_reach_the_hub_board_and_queue_when_it_is_away(self):
        self.env()
        warn = self.root / "warn.json"
        warn.write_text(json.dumps({
            "hosts": {"hub": {"ok": True, "idrive": {"last_epoch": NOW_S - 40 * HOUR}}},
        }))
        board = self.root / "inbox.json"
        code = self.morning(warn, board)
        self.assertEqual(code, 0)
        blob = (self.cowork_state / "queue.jsonl").read_text()
        self.assertIn("item", blob)
        self.assertIn("add", blob)
        self.assertIn("backup:hub:idrive", blob)
        self.assertIn("COWORK_LOCAL=1", self.ssh_log.read_text())
        listed = subprocess.run(
            [sys.executable, "-m", "cowork", "item", "list", "--json"],
            cwd=str(ROOT), env={**os.environ, "COWORK_LOCAL": "1"},
            text=True, capture_output=True,
        )
        self.assertEqual(listed.returncode, 0, listed.stderr)
        items = json.loads(listed.stdout)["items"]
        self.assertEqual([item["key"] for item in items], ["backup:hub:idrive"])
        self.assertEqual(items[0]["state"], "open")
        shown = subprocess.run(
            [sys.executable, "-m", "cowork", "board"],
            cwd=str(ROOT), env={**os.environ, "COWORK_LOCAL": "1"},
            text=True, capture_output=True,
        )
        self.assertIn("OPEN", shown.stdout)
        self.assertIn("backup:hub:idrive", shown.stdout)
        self.assertIn("item", self.ssh_log.read_text())
        self.assertIn("add", self.ssh_log.read_text())

        healthy = self.root / "ok.json"
        healthy.write_text(json.dumps({
            "hosts": {"hub": {"ok": True, "idrive": {"last_epoch": NOW_S - HOUR}}},
        }))
        code = self.morning(healthy, board)
        self.assertEqual(code, 0)
        queued = (self.cowork_state / "queue.jsonl").read_text()
        self.assertIn("close", queued)
        self.assertIn("backup:hub:idrive", queued)
        closed = subprocess.run(
            [sys.executable, "-m", "cowork", "item", "list", "--json"],
            cwd=str(ROOT), env={**os.environ, "COWORK_LOCAL": "1"},
            text=True, capture_output=True,
        )
        self.assertEqual(closed.returncode, 0, closed.stderr)
        self.assertEqual(
            [item["key"] for item in json.loads(closed.stdout)["items"]], [])


class FleetReadings(unittest.TestCase):
    # The first dry run labelled this Mac "hub", counted unplugged drives as
    # empty, and called quiet launchd jobs "not run". These are the readings
    # that should survive that.
    def status(self, snapshot, key):
        found = by_key(evaluate(snapshot, config(), NOW_S))
        self.assertIn(key, found, sorted(found))
        return found[key]

    def test_one_physical_drive_is_reported_on_the_host_that_has_it(self):
        uuid = ["E89D4EFF-3695-4B5A-8894-1C42A34727D0"]
        snap = {"hosts": {
            "primary": host(disks=[{"name": "t9-1", "free_bytes": 0, "mounted": False,
                               "volume_uuids": uuid, "last_seen": 0}]),
            "hub": host(disks=[{"name": "t9-1", "free_bytes": 800e9, "trend_h24": 0,
                               "mounted": True, "volume_uuids": uuid}]),
        }}
        found = by_key(evaluate(snap, config(), NOW_S))
        self.assertNotIn("disk:primary:t9-1", found)
        self.assertEqual(found["disk:hub:t9-1"].status, "pass")
        self.assertEqual(found["disk:hub:t9-1"].value, "800 GB")

    def test_a_drive_attached_nowhere_is_away_not_a_floor_breach(self):
        seen = int(datetime(2026, 11, 4, 8, 0, tzinfo=timezone.utc).timestamp())
        uuid = ["E89D4EFF-3695-4B5A-8894-1C42A34727D0"]
        snap = {"hosts": {
            "primary": host(disks=[{"name": "t9-1", "free_bytes": 0, "mounted": False,
                               "volume_uuids": uuid, "last_seen": 0}]),
            "shared": host(disks=[{"name": "t9-1", "free_bytes": 0, "mounted": False,
                               "volume_uuids": uuid, "last_seen": seen}]),
        }}
        row = self.status(snap, "disk:t9-1")
        self.assertEqual(row.status, "info")
        self.assertEqual(row.value, "away")
        self.assertEqual(row.datum, "since 4 Nov")
        self.assertNotIn("floor", row.datum)

    def test_interval_job_launchd_is_running_is_not_judged_from_a_quiet_log(self):
        job = {"label": "com.example.diskmap", "loaded": True, "exit": 0,
               "last_epoch": NOW_S - 3 * HOUR, "when": "every 1m",
               "runs": 102, "state": "not running"}
        row = self.status({"hosts": {"primary": host(jobs=[job])}}, "job:primary:diskmap")
        self.assertEqual(row.status, "pass")
        self.assertEqual(row.value, "ran")

    def test_interval_job_launchd_never_started_has_not_run(self):
        job = {"label": "com.example.diskmap", "loaded": True, "exit": 0,
               "last_epoch": None, "when": "every 1m", "runs": 0, "state": "not running"}
        row = self.status({"hosts": {"primary": host(jobs=[job])}}, "job:primary:diskmap")
        self.assertEqual(row.status, "fail")
        self.assertEqual(row.value, "not run")

    def test_killed_job_and_backup_pull_stay_failures(self):
        pull = {"label": "com.example.onto-tv-backup-pull", "loaded": True, "exit": 255,
                "last_epoch": NOW_S - HOUR, "when": "daily 09:05", "runs": 5, "state": "not running"}
        row = self.status({"hosts": {"primary": host(jobs=[pull])}}, "job:primary:onto-tv-backup-pull")
        self.assertEqual(row.status, "fail")
        self.assertEqual(row.value, "exit 255")
        finder = {"label": "com.example.finder-up", "loaded": True, "exit": -15,
                  "when": "always on", "runs": 2, "state": "running"}
        # Running now; -15 is the previous instance being restarted by launchctl.
        row = self.status({"hosts": {"primary": host(jobs=[finder])}}, "job:primary:finder-up")
        self.assertEqual(row.status, "pass")
        finder["exit"] = -9
        row = self.status({"hosts": {"primary": host(jobs=[finder])}}, "job:primary:finder-up")
        self.assertEqual(row.status, "fail")
        self.assertIn("-9", row.value)

    def test_glancebar_state_ok_is_current_even_without_a_release_field(self):
        snap = {"hosts": {"primary": host(glancebar={
            "behind": 0, "state": "ok", "installed": "abc", "origin": "abc", "release": None,
        })}}
        row = self.status(snap, "update:primary")
        self.assertEqual(row.status, "pass")
        self.assertNotEqual(row.datum, "no release")

    def test_glancebar_behind_count_is_the_reading(self):
        snap = {"hosts": {"hub": host(glancebar={
            "behind": 1, "state": "no-release", "installed": "abc", "origin": "def",
            "detail": "matching signed release unavailable",
        })}}
        row = self.status(snap, "update:hub")
        self.assertEqual(row.status, "warn")
        self.assertEqual(row.value, "1")
        self.assertIn("behind", row.datum)

    def test_digest_leads_with_failures_and_keeps_full_names(self):
        long = "nightly-render-failure-watch"
        snap = {"hosts": {"primary": host(
            disks=[{"name": "internal", "free_bytes": 200e9, "trend_h24": 0}],
            jobs=[
                {"label": f"com.example.{long}", "loaded": True, "exit": 255,
                 "last_epoch": NOW_S, "when": "every 15m", "runs": 4},
                {"label": "com.example.diskmap", "loaded": True, "exit": 0,
                 "last_epoch": NOW_S, "when": "every 1m", "runs": 10},
            ],
        )}}
        findings = evaluate(snap, config(), NOW_S)
        text, doc = render(findings, generated=NOW_S, date="2026-11-07")
        self.assertNotIn("…", text)
        self.assertIn(long, text)
        self.assertNotIn("diskmap", "\n".join(
            line for line in text.splitlines() if line.startswith("* job") or line.startswith("! job") or line.startswith("~ job")))
        body = [line for line in text.splitlines() if line[:1] in "*~!?"]
        self.assertTrue(body[0].startswith("! "))
        self.assertTrue(any(line.startswith("* ") and line.endswith("checks passed") for line in body))
        passed = next(line for line in body if line.endswith("checks passed"))
        self.assertLess(body.index(next(line for line in body if line.startswith("! "))), body.index(passed))
        self.assertEqual(doc["counts"]["pass"] + doc["counts"]["warn"] + doc["counts"]["fail"], len(findings))
        self.assertIn(long, json.dumps(doc))

    def test_mounted_volumes_roll_up_to_the_physical_drive(self):
        from fleet.morning.probe import disks_from_mounts, parse_launchctl_print
        drives = {"drives": [{"name": "wd-2tb", "volumes": {
            "Media": {"uuid": "AAA"},
            "Archive2": {"uuid": "BBB", "bundle": "/Volumes/Media/Archive.sparsebundle"},
        }}]}
        mounts = [
            {"name": "Media", "uuid": "aaa", "free_bytes": 200 * 1e9},
            {"name": "Archive2", "uuid": "BBB", "free_bytes": 900 * 1e9},
            {"name": "internal", "uuid": "INT", "free_bytes": 50 * 1e9, "internal": True},
        ]
        rows = {row["name"]: row for row in disks_from_mounts(drives, mounts, NOW_S)}
        self.assertEqual(rows["wd-2tb"]["free_bytes"], 200 * 1e9)
        self.assertTrue(rows["wd-2tb"]["mounted"])
        self.assertEqual(set(rows["wd-2tb"]["volume_uuids"]), {"AAA", "BBB"})
        self.assertEqual(rows["internal"]["name"], "internal")
        self.assertNotIn("Archive2", rows)
        printed = parse_launchctl_print(
            "gui/501/com.example.diskmap = {\n"
            "\tstate = not running\n"
            "\truns = 102\n"
            "\tlast exit code = 0\n"
            "\tpid = -\n"
            "\tstdout path = /tmp/diskmap.out\n"
            "}\n"
            "gui/501/com.example.finder-up = {\n"
            "\tstate = running\n"
            "\truns = 2\n"
            "\tpid = 61070\n"
            "\tlast exit code = (never exited)\n"
            "}\n"
        )
        self.assertEqual(printed["com.example.diskmap"]["runs"], 102)
        self.assertEqual(printed["com.example.diskmap"]["exit"], 0)
        self.assertNotIn("pid", printed["com.example.diskmap"])
        self.assertIsNone(printed["com.example.finder-up"]["exit"])
        self.assertEqual(printed["com.example.finder-up"]["state"], "running")


class HostIdentity(unittest.TestCase):
    def test_this_mac_is_the_primary_and_the_list_is_the_fleet_ids(self):
        from fleet.morning.collect import local_host_id, _host_list
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            state = home / ".local/state/diskmap"
            state.mkdir(parents=True)
            (state / "summary.json").write_text(json.dumps({"host_id": "primary"}))
            mail = home / ".config/agent-mail"
            mail.mkdir(parents=True)
            (mail / "hosts").write_text("hub\nshared\n")
            self.assertEqual(local_host_id(home), "primary")
            names = _host_list({"hosts": []}, home, local_host_id(home))
            self.assertEqual(names, ["primary", "hub", "shared"])

    def test_launchd_run_count_is_kept_when_the_registry_is_merged(self):
        from fleet.morning.collect import _merge_jobs
        merged = _merge_jobs(
            {"com.example.diskmap": {"loaded": True, "status": "0", "when": "every 1m", "last_epoch": 1}},
            [{"label": "com.example.diskmap", "loaded": True, "exit": 0, "runs": 102,
              "state": "not running", "last_epoch": NOW_S - 3 * HOUR, "when": "every 1m"}],
        )
        self.assertEqual(merged[0]["runs"], 102)
        row = by_key(evaluate({"hosts": {"primary": host(jobs=merged)}}, config(), NOW_S))["job:primary:diskmap"]
        self.assertEqual(row.status, "pass")

class SnapshotPush(unittest.TestCase):
    # A sleeping laptop used to vanish, or show up as an SSH failure, because
    # the hub pulled readings. Each Mac now writes its own snapshot; the digest
    # only reads those files plus the live readings of the Mac it is running on.
    # The hourly collector must carry a repo's check result into this Mac's snapshot without
    # starting the check command, and must judge its age against the run's own clock.
    def test_collect_local_carries_the_repo_check_result_and_starts_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            repo = home / "Projects" / "media"
            (repo / ".fleet").mkdir(parents=True)
            subprocess.run(["git", "-C", str(repo), "init", "-q"], check=True)
            (repo / ".fleet" / "check").write_text(
                'command = "tools/never-started"\nresult = "~/.local/state/media/check.json"\n')
            state = home / ".local" / "state" / "media"
            state.mkdir(parents=True)
            (state / "check.json").write_text(json.dumps({
                "version": 1, "generated": NOW_S - HOUR, "status": "warn", "value": "1 warn",
                "datum": "scout", "detail": "scout: taste step timed out"}))
            registry = home / ".local" / "state" / "mac-sync" / "registry"
            registry.mkdir(parents=True)
            (registry / "descriptions.tsv").write_text("media\tapp\tactive\tmusic\n")
            cfg = home / "morning.toml"
            cfg.write_text('tz = "UTC"\n')
            seen = []

            def run(cmd, **kwargs):
                seen.append(list(cmd))
                if cmd and cmd[0] == "git":
                    return subprocess.run(cmd, **kwargs)
                return SimpleNamespace(returncode=0, stdout="{}\n", stderr="")

            def collect_at(now):
                code = main(["morning", "collect", "--local", "--home", str(home), "--here", "primary",
                             "--now", str(now), "--config", str(cfg)], gather=lambda: {}, run=run)
                self.assertEqual(code, 0)
                folder = home / ".local" / "state" / "mac-sync" / "fleet-snapshots"
                return json.loads((folder / "primary.json").read_text())["projects"][0]

            project = collect_at(NOW_S)
            self.assertTrue(project["check_declared"])
            self.assertEqual(project["check_result"]["status"], "warn")
            self.assertEqual(project["check_result"]["datum"], "scout")
            self.assertEqual(project["check_result"]["host"], "primary")
            self.assertEqual(collect_at(NOW_S + 40 * HOUR)["check_result"]["value"], "stale")
            self.assertFalse(any("never-started" in " ".join(map(str, cmd)) for cmd in seen), seen)

    def test_collect_local_round_trips_this_mac_into_the_snapshot_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            repo = home / "Projects" / "selector"
            repo.mkdir(parents=True)
            readme = repo / "README"
            subprocess.run(["git", "-C", str(repo), "init", "-q"], check=True)
            readme.write_text("a\n")
            subprocess.run(["git", "-C", str(repo), "add", "README"], check=True)
            registry = home / ".local" / "state" / "mac-sync" / "registry"
            registry.mkdir(parents=True)
            (registry / "descriptions.tsv").write_text("selector\tapp\tactive\tdeck\n")
            cfg = home / "morning.toml"
            cfg.write_text('tz = "UTC"\n')
            seen = []

            def run(cmd, **kwargs):
                seen.append(list(cmd))
                if cmd and cmd[0] == "ssh":
                    raise AssertionError(cmd)
                if cmd and cmd[0] == "git":
                    return subprocess.run(cmd, **kwargs)
                return SimpleNamespace(returncode=0, stdout="{}\n", stderr="")

            def gather():
                return {
                    "disks": [{"name": "internal", "free_bytes": 250e9, "trend_h24": 0}],
                    "glancebar": {"behind": 0, "state": "ok", "installed": "1", "release": "1"},
                    "jobs": [{"label": "com.example.diskmap", "loaded": True, "exit": 0, "runs": 4,
                              "state": "not running", "last_epoch": NOW_S, "when": "every 1m"}],
                    "syncthing": {"errors": 0, "out_of_sync": 0, "receive_only_local": 0},
                    "mail": [{"at": NOW_S - 60, "read": True}],
                    "heavy": [{"label": "sim", "rc": 0, "waited": 1, "end": NOW_S}],
                    "samples": [],
                    "idrive_epoch": NOW_S - 2 * HOUR,
                    "hdd_backup_epoch": NOW_S - HOUR,
                }

            code = main([
                "morning", "collect", "--local", "--home", str(home), "--here", "hub",
                "--now", str(NOW_S), "--config", str(cfg),
            ], gather=gather, run=run)
            self.assertEqual(code, 0)
            folder = home / ".local" / "state" / "mac-sync" / "fleet-snapshots"
            self.assertEqual(sorted(p.name for p in folder.iterdir()), ["hub.json"])
            written = json.loads((folder / "hub.json").read_text())
            self.assertEqual(written["host"], "hub")
            self.assertEqual(written["collected_at"], NOW_S)
            self.assertEqual(written["reading"]["disks"][0]["free_bytes"], 250e9)
            self.assertEqual(written["reading"]["heavy"][0]["label"], "sim")
            self.assertEqual(written["reading"]["jobs"][0]["label"], "com.example.diskmap")
            self.assertEqual(written["reading"]["syncthing"]["errors"], 0)
            self.assertEqual(written["reading"]["mail"][0]["at"], NOW_S - 60)
            self.assertEqual(written["reading"]["idrive"]["last_epoch"], NOW_S - 2 * HOUR)
            project = written["projects"][0]
            self.assertEqual(project["name"], "selector")
            self.assertTrue(project["git"]["dirty"])
            self.assertFalse(any(cmd and cmd[0] == "ssh" for cmd in seen), seen)

            def gather_again():
                fresh = gather()
                fresh["disks"] = [{"name": "internal", "free_bytes": 240e9, "trend_h24": 0}]
                return fresh

            code = main([
                "morning", "collect", "--local", "--home", str(home), "--here", "hub",
                "--now", str(NOW_S + 5), "--config", str(cfg),
            ], gather=gather_again, run=run)
            self.assertEqual(code, 0)
            self.assertEqual(sorted(p.name for p in folder.iterdir()), ["hub.json"])
            again = json.loads((folder / "hub.json").read_text())
            self.assertEqual(again["collected_at"], NOW_S + 5)
            self.assertEqual(again["reading"]["disks"][0]["free_bytes"], 240e9)
            self.assertNotIn("250", (folder / "hub.json").read_text())

    def test_snapshot_age_is_info_after_three_hours_and_warn_after_forty_eight(self):
        # age seconds, status or None when the host row must be absent, datum
        cases = [
            (3 * HOUR, None, None),
            (3 * HOUR + 60, "info", "since 05:59"),
            (48 * HOUR, "info", "since 5 Nov"),
            (48 * HOUR + 1, "warn", "since 5 Nov"),
        ]
        for age, status, datum in cases:
            snap = {"hosts": {"shared": host(
                collected_at=NOW_S - age,
                disks=[{"name": "internal", "free_bytes": 200e9, "trend_h24": 0}],
            )}}
            found = by_key(evaluate(snap, config(), NOW_S))
            self.assertEqual(found["disk:shared:internal"].status, "pass", age)
            self.assertEqual(found["disk:shared:internal"].value, "200 GB", age)
            if status is None:
                self.assertNotIn("host:shared", found, age)
                continue
            row = found["host:shared"]
            self.assertEqual(row.status, status, age)
            self.assertEqual(row.value, "stale", age)
            self.assertEqual(row.datum, datum, age)
            self.assertEqual(row.symbol, "?" if status == "info" else "~", age)
            findings = list(found.values())
            text, doc = render(findings, NOW_S, "2026-11-07")
            visible = [line for line in text.splitlines() if line[:1] in "*~!?" and " stale " in f" {line} "]
            self.assertEqual(len(visible), 1, text)
            self.assertIn("stale", visible[0])
            self.assertIn(datum, visible[0])
            self.assertNotIn("\n", visible[0])
            if status == "info":
                self.assertTrue(doc["ok"], age)
                self.assertEqual(file_items(findings, [], NOW_S), [])
                self.assertIsNone(notification_text(findings))
            else:
                self.assertFalse(doc["ok"], age)
                filed = file_items(findings, [], NOW_S)
                self.assertEqual([item["key"] for item in filed], ["host:shared"])
                self.assertIn("warn", notification_text(findings))

    def test_digest_merges_three_host_snapshots_and_does_not_ssh(self):
        from fleet.morning.collect import collect
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            folder = home / ".local" / "state" / "mac-sync" / "fleet-snapshots"
            folder.mkdir(parents=True)
            docs = {
                "primary": {
                    "version": 1, "host": "primary", "collected_at": NOW_S - 10 * HOUR,
                    "reading": {"ok": True, "disks": [{"name": "internal", "free_bytes": 1, "trend_h24": 0}]},
                    "projects": [],
                },
                "hub": {
                    "version": 1, "host": "hub", "collected_at": NOW_S - 49 * HOUR,
                    "reading": {"ok": True, "disks": [{"name": "internal", "free_bytes": 10e9, "trend_h24": 0}]},
                    "projects": [{
                        "name": "selector", "host": "hub",
                        "git": {"dirty": False, "ahead": 5, "behind": 0, "unpushed": 5, "remote": ""},
                        "ci": {"conclusion": "success", "status": "completed"},
                        "jobs": [],
                    }],
                },
                "shared": {
                    "version": 1, "host": "shared", "collected_at": NOW_S - 4 * HOUR,
                    "reading": {"ok": True, "disks": [{"name": "internal", "free_bytes": 200e9, "trend_h24": 0}]},
                    "projects": [{
                        "name": "selector", "host": "shared",
                        "git": {"dirty": True, "ahead": 1, "behind": 0, "unpushed": 1, "remote": ""},
                        "ci": {"conclusion": "failure", "status": "completed"},
                        "jobs": [],
                    }],
                },
            }
            for name, doc in docs.items():
                (folder / f"{name}.json").write_text(json.dumps(doc))
            (folder / "broken.json").write_text("{not json")
            (folder / "notes.txt").write_text("ignore")
            seen = []

            def run(cmd, **kwargs):
                seen.append(list(cmd))
                if cmd and cmd[0] == "ssh":
                    raise AssertionError(cmd)
                return SimpleNamespace(returncode=0, stdout="{}\n", stderr="")

            def gather():
                return {
                    "disks": [{"name": "internal", "free_bytes": 250e9, "trend_h24": 0}],
                    "glancebar": {"behind": 0, "state": "ok", "installed": "1", "release": "1"},
                    "jobs": [],
                    "syncthing": {"errors": 0, "out_of_sync": 0, "receive_only_local": 0},
                    "mail": [],
                    "heavy": [],
                    "samples": [],
                }

            snap = collect(
                {"hosts": ["primary", "hub", "shared"], "tz": "UTC", "projects": {"heavy_runtime_s": 60}},
                here="primary", now=NOW_S, state=home / "state", run=run, home=home, gather=gather,
            )
            self.assertFalse(any(cmd and cmd[0] == "ssh" for cmd in seen), seen)
            self.assertEqual(set(snap["hosts"]), {"primary", "hub", "shared"})
            self.assertEqual(snap["hosts"]["primary"]["disks"][0]["free_bytes"], 250e9)
            self.assertEqual(snap["hosts"]["primary"]["collected_at"], NOW_S)
            self.assertEqual(snap["hosts"]["shared"]["collected_at"], NOW_S - 4 * HOUR)
            self.assertEqual(snap["hosts"]["hub"]["collected_at"], NOW_S - 49 * HOUR)
            folded = snap["projects"][0]
            self.assertEqual(folded["name"], "selector")
            self.assertTrue(folded["git"]["dirty"])
            self.assertEqual(folded["git"]["ahead"], 5)
            self.assertEqual(folded["ci"]["conclusion"], "failure")
            found = by_key(evaluate(snap, config(), NOW_S))
            self.assertEqual(found["disk:primary:internal"].value, "250 GB")
            self.assertEqual(found["disk:hub:internal"].status, "fail")
            self.assertEqual(found["disk:shared:internal"].status, "pass")
            self.assertNotIn("host:primary", found)
            self.assertEqual(found["host:shared"].status, "info")
            self.assertEqual(found["host:shared"].value, "stale")
            self.assertEqual(found["host:shared"].datum, "since 05:00")
            self.assertEqual(found["host:hub"].status, "warn")
            self.assertEqual(found["host:hub"].value, "stale")
            self.assertEqual(found["host:hub"].datum, "since 5 Nov")
            text, doc = render(list(found.values()), NOW_S, "2026-11-07")
            self.assertIn("disk:primary:internal", {row["key"] for row in doc["rows"]})
            self.assertIn("disk:hub:internal", {row["key"] for row in doc["rows"]})
            self.assertIn("disk:shared:internal", {row["key"] for row in doc["rows"]})
            host_lines = [line for line in text.splitlines() if line[:1] in "*~!?" and " stale " in f" {line} "]
            self.assertEqual(len(host_lines), 2, text)
            self.assertTrue(host_lines[0].startswith("~ "))
            self.assertTrue(any(line.startswith("? ") and "shared" in line for line in host_lines), text)


class ProbeJobsResilience(unittest.TestCase):
    def test_malformed_xml_plist_is_skipped_not_fatal(self):
        from unittest import mock
        from fleet.morning import probe

        with tempfile.TemporaryDirectory() as td:
            home = Path(td)
            agents = home / "Library/LaunchAgents"
            agents.mkdir(parents=True)
            (agents / "com.example.broken.plist").write_text(
                '<?xml version="1.0" encoding="UTF-8"?>\n'
                '<plist version="1.0"><dict>'
                '<key>Label</key><string>com.example.broken</string>'
                '<key>ProgramArguments</key><array><string>cmd 2>&1</string></array>'
                '</dict></plist>\n'
            )
            (agents / "com.example.good.plist").write_text(
                '<?xml version="1.0" encoding="UTF-8"?>\n'
                '<plist version="1.0"><dict>'
                '<key>Label</key><string>com.example.good</string>'
                '<key>ProgramArguments</key><array><string>/usr/bin/true</string></array>'
                '</dict></plist>\n'
            )
            with mock.patch.object(probe, "HOME", home), \
                 mock.patch.object(probe, "_launchctl", return_value={"com.example.good": 0}), \
                 mock.patch.object(probe, "_launchctl_print", return_value={"com.example.good": {"state": "not running"}}):
                rows = probe._jobs()
            self.assertEqual([r["label"] for r in rows], ["com.example.good"])
            from fleet.morning.cli import _cadence
            import plistlib
            (agents / "com.example.morning.plist").write_bytes(plistlib.dumps({
                "Label": "com.example.morning", "ProgramArguments": ["fleet", "morning"],
                "StartInterval": 3600,
            }))
            self.assertEqual(_cadence(home, 1000, {})["label"], "com.example.morning")

    def test_security_evidence_rows_and_staleness(self):
        now = 1_800_000_000
        self.assertEqual(evaluate({"hosts": {}}, {}, now), [])
        fresh = {
            "hosts": {},
            "security_evidence": {
                "version": 1,
                "generated": now - 120,
                "status": "pass",
                "rows": [
                    {"key": "sec:lap:integrity", "name": "lap chain", "status": "pass", "value": "intact", "datum": "12 links", "detail": ""},
                    {"key": "sec:audit", "name": "daily audit", "status": "fail", "value": "missing", "datum": "", "detail": "no audit result"},
                ],
            },
        }
        rows = evaluate(fresh, {}, now)
        by_key = {r.key: r for r in rows}
        self.assertEqual(by_key["sec:lap:integrity"].status, "pass")
        self.assertEqual(by_key["sec:lap:integrity"].area, "security")
        self.assertEqual(by_key["sec:audit"].status, "fail")

        for stamp in (None, 0, now + 600):
            fresh["security_evidence"]["generated"] = stamp
            flagged = evaluate(fresh, {}, now)
            self.assertEqual([(r.key, r.status) for r in flagged], [("sec:verify", "warn")])
        self.assertEqual(evaluate({"security_evidence": {"generated": now, "rows": []}}, {}, now)[0].value, "empty")

        stale = {"hosts": {}, "security_evidence": {"version": 1, "generated": now - 3 * 3600, "rows": []}}
        stale_rows = evaluate(stale, {}, now)
        self.assertEqual(len(stale_rows), 1)
        self.assertEqual(stale_rows[0].key, "sec:verify")
        self.assertEqual(stale_rows[0].status, "warn")
        self.assertEqual(stale_rows[0].value, "stale")

    def test_readonly_disk_images_are_skipped_in_mount_row_and_summary(self):
        from unittest import mock
        from fleet.morning import probe

        ro_dmg = {"BusProtocol": "Disk Image", "WritableVolume": False, "VolumeUUID": "DMG-1"}
        rw_usb = {"BusProtocol": "USB", "WritableVolume": True, "VolumeUUID": "USB-1"}
        ro_internal = {"BusProtocol": "Disk Image", "WritableVolume": False, "VolumeUUID": "INT-1"}
        with mock.patch.object(probe.shutil, "disk_usage", return_value=SimpleNamespace(free=100, total=200)):
            with mock.patch.object(probe, "_diskutil", return_value=ro_dmg):
                self.assertIsNone(probe._mount_row(Path("/Volumes/Example Installer"), "Example Installer", internal=False))
            with mock.patch.object(probe, "_diskutil", return_value=rw_usb):
                row = probe._mount_row(Path("/Volumes/Example USB"), "Example USB", internal=False)
                self.assertEqual(row["name"], "Example USB")
            with mock.patch.object(probe, "_diskutil", return_value=ro_internal):
                row = probe._mount_row(Path("/"), "internal", internal=True)
                self.assertEqual(row["name"], "internal")

        summary = {
            "drives": [{"name": "Example USB", "volume_uuids": ["USB-1"]}],
            "locations": [
                {"name": "internal", "mounted": True, "free_bytes": 200e9},
                {"name": "Example Installer", "mounted": True, "free_bytes": 6e8},
                {"name": "Example USB", "mounted": True, "free_bytes": 300e9},
            ],
        }
        orig_exists = Path.exists
        with mock.patch.object(probe, "_diskutil", return_value=ro_dmg), \
             mock.patch.object(Path, "exists", lambda self: True if str(self) == "/Volumes/Example Installer" else orig_exists(self)):
            rows = probe._disks_from_summary(summary)
        self.assertEqual([r["name"] for r in rows], ["internal", "Example USB"])

    def test_running_job_with_wrapper_sigterm_exit_15_passes(self):
        snap = {"hosts": {"primary": host(jobs=[{
            "label": "com.example.agy-control-api",
            "when": "always on",
            "loaded": True,
            "state": "running",
            "exit": 15,
            "runs": 16,
        }])}}
        self.assertEqual(by_key(evaluate(snap, config(), NOW_S))["job:primary:agy-control-api"].status, "pass")


if __name__ == "__main__":
    unittest.main()
