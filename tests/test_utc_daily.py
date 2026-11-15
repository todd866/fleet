import calendar
import importlib.machinery
import importlib.util
import os
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
PREFIXES = ("com.example.",)
WRAPPER = ROOT / "bin" / "utc-daily"


def load_schedule():
    loader = importlib.machinery.SourceFileLoader("utc_schedule", str(ROOT / "bin" / "utc-schedule"))
    spec = importlib.util.spec_from_loader("utc_schedule", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def epoch(text):
    """'2026-11-14 22:31' in UTC."""
    date, clock = text.split()
    y, m, d = (int(x) for x in date.split("-"))
    hh, mm = (int(x) for x in clock.split(":"))
    return calendar.timegm((y, m, d, hh, mm, 0))


class Wrapper(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.state = pathlib.Path(self.tmp.name) / "state"
        self.ran = pathlib.Path(self.tmp.name) / "ran"

    def tearDown(self):
        self.tmp.cleanup()

    def tick(self, when, *options, command=None):
        env = dict(os.environ, UTC_DAILY_STATE=str(self.state), UTC_DAILY_NOW=str(epoch(when)))
        command = command or ["/bin/sh", "-c", f"echo x >> {self.ran}"]
        return subprocess.run(["/bin/zsh", str(WRAPPER), "--label", "job", *options, "--", *command],
                              env=env, capture_output=True, text=True)

    def runs(self):
        return len(self.ran.read_text().splitlines()) if self.ran.exists() else 0

    def test_runs_once_per_utc_day_after_the_slot(self):
        self.assertEqual(self.tick("2026-11-14 22:29", "--at", "22:30").returncode, 0)
        self.assertEqual(self.runs(), 0)
        self.tick("2026-11-14 22:31", "--at", "22:30")
        self.assertEqual(self.runs(), 1)
        self.tick("2026-11-14 23:55", "--at", "22:30")
        self.assertEqual(self.runs(), 1)
        # midnight UTC starts a new day, but the slot has not come round yet
        self.tick("2026-11-15 00:05", "--at", "22:30")
        self.assertEqual(self.runs(), 1)
        self.tick("2026-11-15 22:40", "--at", "22:30")
        self.assertEqual(self.runs(), 2)

    def test_a_mac_asleep_at_the_slot_runs_at_its_next_tick_and_only_once_for_two_slots(self):
        self.tick("2026-11-14 20:00", "--at", "03:15", "--at", "15:15")
        self.assertEqual(self.runs(), 1)
        self.tick("2026-11-14 20:05", "--at", "03:15", "--at", "15:15")
        self.assertEqual(self.runs(), 1)

    def test_two_slots_each_run_on_their_own(self):
        self.tick("2026-11-14 03:20", "--at", "03:15", "--at", "15:15")
        self.tick("2026-11-14 15:20", "--at", "03:15", "--at", "15:15")
        self.assertEqual(self.runs(), 2)

    def test_weekday_is_counted_in_utc(self):
        # 14 Nov 2026 is a Saturday (6), 15 Nov a Sunday (0)
        self.tick("2026-11-14 23:30", "--at", "23:00", "--weekday", "0")
        self.assertEqual(self.runs(), 0)
        self.tick("2026-11-15 23:30", "--at", "23:00", "--weekday", "0")
        self.assertEqual(self.runs(), 1)

    def test_exit_code_and_last_record(self):
        out = self.tick("2026-11-14 10:00", "--at", "09:00", command=["/bin/sh", "-c", "exit 7"])
        self.assertEqual(out.returncode, 7)
        start, end, slot, rc = (self.state / "job" / "last").read_text().split()
        self.assertEqual((int(start), slot, rc), (epoch("2026-11-14 10:00"), "09:00", "7"))
        # the failed run is not retried the same day, and a quiet tick exits 0
        self.assertEqual(self.tick("2026-11-14 10:05", "--at", "09:00", command=["/bin/sh", "-c", "exit 7"]).returncode, 0)

    def test_retry_leaves_a_failed_slot_open_until_a_run_succeeds(self):
        fail = ["/bin/sh", "-c", f"echo x >> {self.ran}; exit 3"]
        self.assertEqual(self.tick("2026-11-14 10:00", "--at", "09:00", "--retry", command=fail).returncode, 3)
        self.assertEqual(self.tick("2026-11-14 10:05", "--at", "09:00", "--retry", command=fail).returncode, 3)
        self.assertEqual(self.runs(), 2)
        self.assertEqual(self.tick("2026-11-14 10:10", "--at", "09:00", "--retry").returncode, 0)
        self.tick("2026-11-14 10:15", "--at", "09:00", "--retry")
        self.assertEqual(self.runs(), 3)

    def test_a_mac_asleep_across_utc_midnight_still_runs_last_nights_slot(self):
        # a tick before the slot, the lid closes, and it opens after 00:00Z (08:00 in Perth)
        self.tick("2026-11-14 22:00", "--at", "22:30")
        self.tick("2026-11-15 00:05", "--at", "22:30")
        self.assertEqual(self.runs(), 1)
        start, end, slot, rc = (self.state / "job" / "last").read_text().split()
        self.assertEqual(slot, "22:30")
        self.tick("2026-11-15 00:10", "--at", "22:30")
        self.assertEqual(self.runs(), 1)
        # and the next night's slot is its own run
        self.tick("2026-11-15 22:35", "--at", "22:30")
        self.assertEqual(self.runs(), 2)

    def test_a_slot_missed_by_more_than_the_catch_up_window_is_skipped(self):
        self.tick("2026-11-14 22:00", "--at", "22:30")
        self.tick("2026-11-15 17:00", "--at", "22:30")   # 18.5 h late
        self.assertEqual(self.runs(), 0)
        self.tick("2026-11-15 22:31", "--at", "22:30")
        self.assertEqual(self.runs(), 1)

    def test_a_job_first_seen_after_utc_midnight_does_not_run_yesterdays_slot(self):
        # installing or converting a job must not fire every overnight slot at once
        self.tick("2026-11-15 00:05", "--at", "22:30")
        self.assertEqual(self.runs(), 0)
        self.tick("2026-11-15 22:31", "--at", "22:30")
        self.assertEqual(self.runs(), 1)

    def test_weekday_follows_the_slot_into_the_next_utc_day(self):
        # Sunday 23:00Z slept through, woken on Monday 00:10Z
        self.tick("2026-11-15 22:00", "--at", "23:00", "--weekday", "0")
        self.tick("2026-11-16 00:10", "--at", "23:00", "--weekday", "0")
        self.assertEqual(self.runs(), 1)
        # Monday's own 23:00 is not a Sunday slot
        self.tick("2026-11-16 23:10", "--at", "23:00", "--weekday", "0")
        self.assertEqual(self.runs(), 1)

    def running(self, key, date, pid, boot, attempt):
        folder = self.state / "job"
        folder.mkdir(parents=True, exist_ok=True)
        (folder / f"done-{key}").write_text(date + "\n")
        (folder / "running").write_text(f"{key} {date} {pid} {boot} {attempt}\n")

    def test_a_run_cut_short_by_a_restart_is_run_again_once(self):
        # the slot was marked started, then the Mac lost power: no 'last', a 'running' from another boot
        self.running("0900", "2026-11-14", 99999, "1111", 1)
        self.tick("2026-11-14 10:00", "--at", "09:00")
        self.assertEqual(self.runs(), 1)
        self.assertEqual((self.state / "job" / "running").read_text().strip(), "")
        # a second cut is the last: two attempts, then the slot stays done
        self.running("0900", "2026-11-14", 99999, "1111", 2)
        self.tick("2026-11-14 10:05", "--at", "09:00")
        self.assertEqual(self.runs(), 1)

    def test_a_tick_while_the_run_is_still_going_does_nothing(self):
        boot = subprocess.run(["/usr/sbin/sysctl", "-n", "kern.boottime"], capture_output=True, text=True).stdout
        boot = boot.split("sec = ")[1].split(",")[0]
        self.running("0900", "2026-11-14", os.getpid(), boot, 1)
        (self.state / "job" / "done-0900").write_text("")   # even with the slot open
        self.assertEqual(self.tick("2026-11-14 10:00", "--at", "09:00").returncode, 0)
        self.assertEqual(self.runs(), 0)

    def test_a_run_ended_by_a_signal_is_tried_again_at_the_next_tick(self):
        # launchd ends a job with TERM at shutdown; that is not the job's own failure
        script = f"echo x >> {self.ran}; kill -TERM $PPID; sleep 5"
        self.tick("2026-11-14 10:00", "--at", "09:00", command=["/bin/sh", "-c", script])
        self.assertEqual(self.runs(), 1)
        self.tick("2026-11-14 10:05", "--at", "09:00")
        self.assertEqual(self.runs(), 2)

    def test_status_lists_jobs_only(self):
        self.tick("2026-11-14 10:00", "--at", "09:00")
        (self.state / "plist-backups").mkdir()
        env = dict(os.environ, UTC_DAILY_STATE=str(self.state))
        out = subprocess.run(["/bin/zsh", str(WRAPPER), "status"], env=env, capture_output=True, text=True).stdout
        self.assertIn("job ", out)
        self.assertNotIn("plist-backups", out)

    def test_bad_arguments_are_refused(self):
        self.assertEqual(self.tick("2026-11-14 10:00", "--at", "25:00").returncode, 64)
        self.assertEqual(self.tick("2026-11-14 10:00").returncode, 64)
        self.assertEqual(self.runs(), 0)


class Conversion(unittest.TestCase):
    def setUp(self):
        self.mod = load_schedule()

    def test_local_slots_become_the_same_instant_in_utc(self):
        self.assertEqual(self.mod.to_utc(6, 30, None, 480), ("22:30", None))
        self.assertEqual(self.mod.to_utc(9, 5, None, 480), ("01:05", None))
        # Sunday 07:00 at UTC+8 is Saturday 23:00 UTC; launchd also writes Sunday as 7
        self.assertEqual(self.mod.to_utc(7, 0, 0, 480), ("23:00", 6))
        self.assertEqual(self.mod.to_utc(7, 0, 7, 480), ("23:00", 6))
        self.assertEqual(self.mod.to_utc(22, 0, 6, -300), ("03:00", 0))

    def test_plan_wraps_the_command_and_leaves_special_jobs_alone(self):
        job = {"Label": "com.example.job", "Program": "/bin/echo", "ProgramArguments": ["/bin/echo", "hi"],
               "StartCalendarInterval": [{"Hour": 3, "Minute": 15}, {"Hour": 15, "Minute": 15}]}
        new, slots, weekday = self.mod.plan(job, 480, PREFIXES)
        self.assertEqual(slots, ["07:15", "19:15"])
        self.assertIsNone(weekday)
        self.assertNotIn("StartCalendarInterval", new)
        self.assertNotIn("Program", new)
        self.assertEqual(new["StartInterval"], 300)
        self.assertEqual(new["ProgramArguments"][:2], ["/bin/zsh", self.mod.WRAPPER])
        self.assertEqual(new["ProgramArguments"][-3:], ["--", "/bin/echo", "hi"])
        self.assertIsInstance(self.mod.plan(new, 480, PREFIXES), str)  # second pass is a no-op
        self.assertIsInstance(self.mod.plan({**job, "KeepAlive": True}, 480, PREFIXES), str)
        retried, _slots, _day = self.mod.plan({**job, "KeepAlive": {"SuccessfulExit": False}}, 480, PREFIXES)
        self.assertIn("--retry", retried["ProgramArguments"])
        self.assertEqual(retried["KeepAlive"], {"SuccessfulExit": False})
        mounted, _slots, _day = self.mod.plan({**job, "StartOnMount": True}, 480, PREFIXES)
        self.assertTrue(mounted["StartOnMount"])
        hourly = {**job, "StartCalendarInterval": {"Minute": 23}}
        self.assertIsInstance(self.mod.plan(hourly, 480, PREFIXES), str)
        vendor = {**job, "Label": "com.google.keystone"}
        self.assertIsInstance(self.mod.plan(vendor, 480, PREFIXES), str)


if __name__ == "__main__":
    unittest.main()
