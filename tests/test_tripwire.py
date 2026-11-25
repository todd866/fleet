import importlib.machinery
import importlib.util
import json
import os
import pathlib
import tempfile
import subprocess
import sys
import unittest
from unittest import mock
from concurrent.futures import ThreadPoolExecutor

ROOT = pathlib.Path(__file__).resolve().parent.parent


def load(state, home):
    os.environ["TRIPWIRE_STATE"] = str(state)
    os.environ["TRIPWIRE_CONFIG"] = str(home / "tripwire.json")
    os.environ["HOME"] = str(home)
    loader = importlib.machinery.SourceFileLoader("tripwire", str(ROOT / "bin" / "tripwire"))
    spec = importlib.util.spec_from_loader("tripwire", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class Spawned:
    def __init__(self):
        self.calls = []

    def __call__(self, argv, **kwargs):
        self.calls.append(argv)


class Ran:
    def __init__(self):
        self.calls = []

    def __call__(self, argv, **kwargs):
        self.calls.append(argv)


class TripwireTest(unittest.TestCase):
    def setUp(self):
        self._env = dict(os.environ)
        self.tmp = tempfile.TemporaryDirectory()
        self.home = pathlib.Path(self.tmp.name) / "home"
        self.home.mkdir()
        self.state = pathlib.Path(self.tmp.name) / "state"
        self.tw = load(self.state, self.home)
        self.keys = self.home / "authorized_keys"
        self.keys.write_text("ssh-ed25519 AAAA one\n")
        self.config = {"files": [str(self.keys)], "summon": ["/bin/echo", "{brief}"], "mail_to": "hub:fleet"}
        self.wires = {"files": self.tw.wire_files}
        self.spawn, self.runner = Spawned(), Ran()

    def tearDown(self):
        os.environ.clear()
        os.environ.update(self._env)
        try:
            self.tmp.cleanup()
        except OSError:
            # A process a test started can still be writing into the folder; Python 3.9's rmtree
            # then fails on "directory not empty" (newer Pythons retry). Not a tripwire fault.
            import shutil
            shutil.rmtree(self.tmp.name, ignore_errors=True)

    def check(self, now=1000.0, **extra):
        return self.tw.check(config=self.config, now=now, host="testmac", wires=self.wires,
                             spawn=self.spawn, runner=self.runner, **extra)

    def incidents(self):
        return sorted((self.state / "incidents").glob("*/incident.json"))

    def test_default_wires_restore_each_provider_from_its_own_copy(self):
        self.wires = {"guards": self.tw.wire_guards}
        originals = {}
        for path in (".codex/hooks.json", ".cursor/hooks.json", ".gemini/config/hooks.json", ".claude/settings.json"):
            target = self.home / path
            target.parent.mkdir(parents=True)
            originals[target] = f"{path}: agentkit hook"
            target.write_text(originals[target])
        self.check()
        for target in originals:
            target.write_text(f"new settings for {target.parent.name}")
        self.check(now=1300)
        self.assertEqual(len(self.incidents()), 1)
        for target, content in originals.items():
            self.assertEqual(target.read_text(), content)
        incident = self.incidents()[0].parent
        (incident / "report.md").write_text("VERDICT: BENIGN\nFixture approval confirmed.\n")
        self.assertEqual(self.tw.release(incident.name, self.config), 0)
        for target in originals:
            self.assertEqual(target.read_text(), f"new settings for {target.parent.name}")

    def test_investigator_saves_only_valid_stdout_and_tripwire_releases(self):
        self.config["summon"] = [str(ROOT / "bin/tripwire-investigate")]
        incident = self.hold_keys()
        fake_bin = self.home / "bin"
        fake_bin.mkdir()
        agy = fake_bin / "agy"
        agy.write_text("""#!/bin/sh
for arg in "$@"; do printf '%s\\n' "$arg" >> args.txt; done
printf '%s\\n' "$TEST_REPORT"
""")
        agy.chmod(0o700)
        timeout = fake_bin / "timeout"
        timeout.write_text('#!/bin/sh\nshift\nexec "$@"\n')
        timeout.chmod(0o700)
        env = dict(os.environ, PATH=str(fake_bin) + ":/usr/bin:/bin")
        command = [sys.executable, str(ROOT / 'bin/tripwire-investigate'), '--engine', 'gemini']
        def run(report):
            return subprocess.run(command, cwd=incident, env=dict(env, TEST_REPORT=report),
                                  capture_output=True).returncode
        self.assertNotEqual(run("inconclusive"), 0)
        self.assertFalse((incident / "report.md").exists())
        self.assertEqual(run("VERDICT: BENIGN\nfixture evidence"), 0)
        args = (incident / "args.txt").read_text().splitlines()
        self.assertIn("plan", args)
        self.assertIn("--sandbox", args)
        record = json.loads((incident / "incident.json").read_text())
        self.assertIn(json.dumps({key: record[key] for key in ("host", "raised", "changes")}), args)
        self.assertNotIn("--dangerously-skip-permissions", args)
        self.assertEqual(run("VERDICT: SUSPICIOUS\nsecond response"), 0)
        self.assertEqual((incident / "report.md").read_text(), "VERDICT: BENIGN\nfixture evidence\n")
        self.assertNotIn("stranger", self.keys.read_text())
        self.check(now=1600)
        self.assertIn("stranger", self.keys.read_text())

    def test_no_hold_failed_attempts_get_one_fallback_and_a_local_report(self):
        self.config["summon_fallback"] = ["/bin/echo", "fallback"]
        self.check()
        self.keys.write_text("change")
        self.check(now=1010)
        incident = self.incidents()[0].parent
        (incident / "primary-result.json").write_text('{"gemini":"provider exited 1"}')
        with ThreadPoolExecutor(2) as pool:
            list(pool.map(lambda _: self.check(now=1011), range(2)))
        self.assertEqual(len(self.spawn.calls), 2)
        (incident / "fallback-result.json").write_text('{"zai":"provider HTTP 429"}')
        self.check(now=1012)
        report = (incident / "report.md").read_text()
        self.assertEqual(self.tw.reports.verdict(report), self.tw.reports.NEEDS_REVIEW)
        self.assertIn("observed_at_review", report)
        self.assertIn("provider HTTP 429", report)
        n = len(self.runner.calls)
        self.check(now=1100)
        self.assertEqual(len(self.runner.calls), n)
        self.assertEqual(len(self.spawn.calls), 2)

    def test_partial_verdict_at_deadline_cannot_release_the_file_or_extend_the_hold(self):
        self.config.update(hold_timeout_min=1, summon_fallback=["/bin/echo", "fallback"])
        incident = self.hold_keys()
        (incident / "report.md").write_text("VERDICT: BENIGN\n")
        (incident / "primary-result.json").write_text("{}")
        self.assertEqual(self.tw.release(incident.name, self.config), 1)
        self.check(now=1360)
        self.assertEqual(len(self.spawn.calls), 1)
        self.assertEqual(self.tw.reports.saved_verdict(incident), self.tw.reports.NEEDS_REVIEW)
        preserved, = incident.glob("report.incomplete-*.md")
        self.assertEqual(preserved.read_text(), "VERDICT: BENIGN\n")
        self.assertNotIn("stranger", self.keys.read_text())

    def test_hostile_report_stays_held_past_deadline_and_late_escalation_is_reported(self):
        self.config.update(hold_timeout_min=1)
        incident = self.hold_keys()
        self.check(now=1360)
        self.tw.reports.publish(incident, "VERDICT: HOSTILE\nConfirmed by the reviewer.")
        self.check(now=1500)
        self.assertEqual((incident / "verdict.seen").read_text().strip(), "VERDICT: HOSTILE")
        self.assertNotIn("stranger", self.keys.read_text())
        self.assertTrue(list(incident.glob("report.local-*.md")))
        self.assertEqual(self.tw.reports.publish(incident, "VERDICT: BENIGN\nLate reply."), "VERDICT: HOSTILE")
        self.assertEqual(self.tw.release(incident.name, self.config), 1)

    def test_concurrent_incidents_get_distinct_evidence_directories(self):
        changes = [{"wire": "test", "what": "test", "before": None, "after": "run"}]
        with ThreadPoolExecutor(2) as pool:
            results = list(pool.map(lambda _: self.tw.raise_incident({}, changes, 1000, "fixture",
                                do_summon=False, runner=self.runner), range(2)))
        self.assertNotEqual(results[0], results[1])
        self.assertEqual(len(self.incidents()), 2)

    def test_text_provider_route_is_bounded_and_never_auto_clears_metadata_only_incidents(self):
        loader = importlib.machinery.SourceFileLoader("investigator", str(ROOT / "bin/tripwire-investigate"))
        module = importlib.util.module_from_spec(importlib.util.spec_from_loader("investigator", loader))
        loader.exec_module(module)
        record = {"changes": [{"wire": "files", "what": "private-fixture", "before": "private-before",
                              "after": "private-after", "other": "private-extra"}] * 100}
        reply = {"stop_reason": "end_turn", "content": [{"type": "text", "text": "VERDICT: BENIGN\nOrdinary change."}]}
        response = subprocess.CompletedProcess([], 0, stdout=json.dumps(reply) + "\n200")
        with mock.patch.object(module.subprocess, "run", return_value=response) as send:
            report, status = module.text_review("deepseek", record, 3)
            self.assertEqual(status, "complete")
            self.assertEqual(module.reports.verdict(report), module.reports.NEEDS_REVIEW)
            command, = send.call_args.args
            payload = json.loads(send.call_args.kwargs["input"])
            self.assertEqual(command[-1], "http://127.0.0.1:8797/v1/messages")
            self.assertIn("--max-filesize", command)
            self.assertNotIn("--location", command)
            self.assertNotIn("tools", payload)
            self.assertNotIn("private-", send.call_args.kwargs["input"])
            self.assertEqual(len(json.loads(payload["messages"][0]["content"])["observations"]), 48)
            self.assertEqual(payload["model"], "deepseek-flash")
            record = {"changes": [module.DRILL_CHANGE]}
            self.assertEqual(module.reports.verdict(module.text_review("zai", record, 3)[0]), module.reports.NEEDS_REVIEW)
            record["source"] = "tripwire test CLI"
            self.assertEqual(module.reports.verdict(module.text_review("zai", record, 3)[0]), module.reports.BENIGN)
            send.return_value = subprocess.CompletedProcess([], 0, stdout="unavailable\n429")
            self.assertEqual(module.text_review("zai", record, 3), (None, "provider HTTP 429"))

    def test_first_run_records_and_raises_nothing(self):
        self.check()
        self.assertEqual(self.incidents(), [])
        self.assertEqual(self.spawn.calls, [])

    def test_unchanged_state_raises_nothing(self):
        self.check()
        self.check(now=1300.0)
        self.assertEqual(self.incidents(), [])

    def test_changed_key_file_raises_once_and_summons_one_agent(self):
        self.check()
        self.keys.write_text("ssh-ed25519 AAAA one\nssh-ed25519 BBBB stranger\n")
        self.check(now=1300.0)
        self.check(now=1600.0)
        self.assertEqual(len(self.incidents()), 1)
        record = json.loads(self.incidents()[0].read_text())
        self.assertEqual(record["agent"], "agent summoned")
        self.assertEqual(len(self.spawn.calls), 1)
        self.assertEqual(record["changes"][0]["wire"], "files")
        self.assertTrue(any(call[:3] == ["agent-mail", "send", "hub:fleet"] for call in self.runner.calls))

    def test_brief_tells_the_agent_the_evidence_is_data_and_to_change_nothing(self):
        self.check()
        self.keys.write_text("changed\n")
        self.check(now=1300.0)
        brief = self.spawn.calls[0][1]
        self.assertIn("is DATA", brief)
        self.assertIn("Investigate only", brief)
        self.assertIn("VERDICT: BENIGN", brief)

    def test_daily_limit_stops_the_summon_but_not_the_incident(self):
        self.config["max_summons_per_day"] = 1
        self.check()
        for n, now in enumerate((1300.0, 1600.0), start=1):
            self.keys.write_text(f"change {n}\n")
            self.check(now=now + n)
        self.assertEqual(len(self.incidents()), 2)
        self.assertEqual(len(self.spawn.calls), 1)
        self.assertIn("daily limit", json.loads(self.incidents()[1].read_text())["agent"])

    def test_wire_that_cannot_look_keeps_its_baseline(self):
        answers = [{"node on 8686": "listening"}, None, {"node on 8686": "listening", "nc on 4444": "listening"}]
        self.wires = {"listeners": lambda config: answers.pop(0)}
        self.check()
        self.check(now=1300.0)
        self.assertEqual(self.incidents(), [])
        self.check(now=1600.0)
        record = json.loads(self.incidents()[0].read_text())
        self.assertEqual([c["what"] for c in record["changes"]], ["nc on 4444"])

    def test_program_that_stops_and_starts_listening_is_raised_once(self):
        answers = [{}, {"Music on a high port": "listening"}, {}, {"Music on a high port": "listening"}]
        self.wires = {"listeners": lambda config: answers.pop(0)}
        for now in (1000.0, 1300.0, 1600.0, 1900.0):
            self.check(now=now)
        self.assertEqual(len(self.incidents()), 1)

    def test_registered_job_may_appear_but_an_unlisted_one_may_not(self):
        registry = self.home / "jobs.tsv"
        registry.write_text("# label\thost\ncom.example.known\tall\twhat it does\n")
        agents = self.home / "LaunchAgents"
        agents.mkdir()
        self.config.update(registry=str(registry), launchd_dirs=[str(agents)])
        self.wires = {"launchd": self.tw.wire_launchd}
        self.check()
        (agents / "com.example.known.plist").write_text("<plist/>")
        self.check(now=1300.0)
        self.assertEqual(self.incidents(), [])
        (agents / "com.example.stranger.plist").write_text("<plist/>")
        self.check(now=1600.0)
        self.assertEqual(len(self.incidents()), 1)
        (agents / "com.example.known.plist").write_text("<plist>other program</plist>")
        self.check(now=1900.0)
        self.assertEqual(len(self.incidents()), 2)

    def test_guards_raise_only_when_the_hook_disappears(self):
        settings = self.home / "settings.json"
        self.config["must_contain"] = [[str(settings), "agentkit hook"]]
        self.wires = {"guards": self.tw.wire_guards}
        self.check()
        settings.write_text('{"hooks": "agentkit hook claude all"}')
        self.check(now=1300.0)
        self.assertEqual(self.incidents(), [])
        settings.write_text('{"hooks": ""}')
        self.check(now=1600.0)
        self.assertEqual(len(self.incidents()), 1)

    def test_listeners_ignore_loopback_and_name_random_ports_by_program(self):
        text = ("COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n"
                "node 1 u 12u IPv4 0x1 0t0 TCP *:8686 (LISTEN)\n"
                "ollama 2 u 3u IPv4 0x2 0t0 TCP 127.0.0.1:11434 (LISTEN)\n"
                "rapportd 3 u 4u IPv4 0x3 0t0 TCP *:55774 (LISTEN)\n"
                "rapportd 3 u 5u IPv6 0x4 0t0 TCP *:61000 (LISTEN)\n")
        self.assertEqual(self.tw.wire_listeners({}, text),
                         {"node on 8686": "listening", "rapportd on a high port": "listening"})

    def test_accounts_see_a_new_user_and_a_new_admin(self):
        before = self.tw.wire_accounts({}, "_www 70\nowner 501\n", "GroupMembership: root owner\n")
        after = self.tw.wire_accounts({}, "_www 70\nowner 501\nguesty 502\n", "GroupMembership: root owner guesty\n")
        found = self.tw.differences("accounts", before, after)
        self.assertEqual(sorted(c["what"] for c in found), ["admin guesty", "user guesty"])

    def test_repository_turning_public_is_raised_and_being_offline_is_not(self):
        self.assertEqual(self.tw.wire_public_repos({}, "[]"), {})          # off unless the site turns it on
        answers = ['[{"nameWithOwner": "me/site"}]', "", '[{"nameWithOwner": "me/site"}, {"nameWithOwner": "me/notes"}]']
        self.config["github"] = True
        self.wires = {"public_repos": lambda config: self.tw.wire_public_repos(config, answers.pop(0))}
        self.check()
        self.check(now=1300.0)
        self.assertEqual(self.incidents(), [])
        self.check(now=1600.0)
        record = json.loads(self.incidents()[0].read_text())
        self.assertEqual([c["what"] for c in record["changes"]], ["public repository me/notes"])
        self.assertNotIn("holds", record)

    # ---- hold first, then think ----

    def hold_keys(self):
        self.config["hold_files"] = [str(self.keys)]
        self.check()
        self.keys.write_text("ssh-ed25519 AAAA one\nssh-ed25519 BBBB stranger\n")
        self.check(now=1300.0)
        return self.incidents()[0].parent

    def test_changed_login_keys_are_put_back_at_once_and_the_change_is_kept(self):
        incident = self.hold_keys()
        self.assertEqual(self.keys.read_text(), "ssh-ed25519 AAAA one\n")
        self.assertIn("stranger", (incident / "held" / "authorized_keys").read_text())
        record = json.loads((incident / "incident.json").read_text())
        self.assertEqual(record["holds"][0]["kind"], "file")
        self.assertIn("1 thing(s) held", record["agent"])
        self.check(now=1600.0)                      # the put-back file is the baseline: no second incident
        self.assertEqual(len(self.incidents()), 1)

    def test_release_needs_a_benign_report_then_returns_the_change_without_raising_again(self):
        incident = self.hold_keys()
        self.assertEqual(self.tw.release(incident.name, self.config), 1)
        (incident / "report.md").write_text("VERDICT: SUSPICIOUS\n")
        self.assertEqual(self.tw.release(incident.name, self.config), 1)
        (incident / "report.md").write_text("VERDICT: BENIGN pending confirmation\n")
        self.assertEqual(self.tw.release(incident.name, self.config), 1)
        self.assertNotIn("stranger", self.keys.read_text())
        (incident / "report.md").write_text("VERDICT: BENIGN\nthe owner added a key\n")
        self.assertEqual(self.tw.release(incident.name, self.config), 0)
        self.assertIn("stranger", self.keys.read_text())
        self.check(now=1600.0)
        self.assertEqual(len(self.incidents()), 1)
        self.assertEqual(self.tw.release(incident.name, self.config), 0)   # twice is harmless

    def test_owner_can_release_without_a_report(self):
        incident = self.hold_keys()
        self.assertEqual(self.tw.release(incident.name, self.config, owner=True), 0)
        self.assertIn("stranger", self.keys.read_text())

    def test_release_leaves_a_file_that_changed_again_while_held(self):
        incident = self.hold_keys()
        self.keys.write_text("someone edited it meanwhile\n")
        (incident / "report.md").write_text("VERDICT: BENIGN\nFixture approval confirmed.\n")
        self.tw.release(incident.name, self.config)
        self.assertEqual(self.keys.read_text(), "someone edited it meanwhile\n")

    def test_nothing_is_held_when_no_agent_can_be_started(self):
        self.config.update(hold_files=[str(self.keys)], max_summons_per_day=0)
        self.check()
        self.keys.write_text("changed\n")
        self.check(now=1300.0)
        self.assertEqual(self.keys.read_text(), "changed\n")
        self.assertNotIn("holds", json.loads(self.incidents()[0].read_text()))

    def test_login_keys_stay_put_back_when_no_report_comes_and_the_owner_is_told_once(self):
        self.config["hold_timeout_min"] = 10
        incident = self.hold_keys()
        self.check(now=1300.0 + 11 * 60)
        self.check(now=1300.0 + 30 * 60)
        self.assertNotIn("stranger", self.keys.read_text())
        told = [call for call in self.runner.calls if "stay put back" in " ".join(call)]
        self.assertEqual(len(told), 2)  # one notification and one mail
        self.assertEqual(self.tw.release(incident.name, self.config, owner=True), 0)
        self.assertIn("stranger", self.keys.read_text())

    def dead_agent_setup(self):
        """A held listener whose agent has already exited without writing a report."""
        import subprocess
        import sys
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(lambda: (child.kill(), child.wait()))
        gone = subprocess.Popen([sys.executable, "-c", "pass"])
        gone.wait()
        self.sent = []
        self.kill = lambda pid, sig: self.sent.append((pid, sig))
        self.tw.listener_rows = lambda config, text: [("nc on 4444", child.pid)]
        answers = [{}] + [{"nc on 4444": "listening"}] * 4
        self.wires = {"listeners": lambda config: answers.pop(0)}
        self.check()
        self.check(now=1300.0, kill=self.kill)
        path = self.incidents()[0]
        record = json.loads(path.read_text())
        record.update(agent_pid=gone.pid, agent_started="Thu  1 Jan 00:00:00 1970")
        path.write_text(json.dumps(record))
        return child

    def test_agent_that_exits_without_a_report_does_not_leave_the_hold_for_the_hour(self):
        import signal
        child = self.dead_agent_setup()
        self.check(now=1300.0 + 30, kill=self.kill)           # too soon to call it dead
        self.assertEqual(len(self.sent), 1)
        self.check(now=1300.0 + 120, kill=self.kill)
        self.assertEqual(self.sent[-1], (child.pid, signal.SIGCONT))
        self.assertTrue(any("exited without a report" in " ".join(call) for call in self.runner.calls))

    def test_agent_whose_state_is_unknown_is_never_taken_for_dead(self):
        import os
        self.assertFalse(self.tw.agent_is_gone(None, "x"))
        self.assertFalse(self.tw.agent_is_gone(os.getpid(), None))              # no start time recorded
        self.tw.process_start = lambda pid: None                                # `ps` failed
        self.assertFalse(self.tw.agent_is_gone(os.getpid(), "Thu  1 Jan 00:00:00 1970"))
        self.tw.process_start = lambda pid: "Sat 14 Nov 20:00:00 2026"
        self.assertFalse(self.tw.agent_is_gone(os.getpid(), "Sat 14 Nov 20:00:00 2026"))
        self.assertTrue(self.tw.agent_is_gone(os.getpid(), "Thu  1 Jan 00:00:00 1970"))   # pid reused

    def test_fallback_agent_is_tried_once_before_the_hold_is_given_up(self):
        import signal
        child = self.dead_agent_setup()
        self.config["summon_fallback"] = ["/bin/echo", "fallback", "{brief}"]
        self.check(now=1300.0 + 120, kill=self.kill)
        self.assertEqual(self.spawn.calls[-1][:2], ["/bin/echo", "fallback"])
        self.assertEqual(len(self.sent), 1)                    # still held: someone is looking again
        record = json.loads(self.incidents()[0].read_text())
        self.assertTrue(record["fallback_tried"])
        self.assertNotIn("released", record)

    def test_stopped_process_goes_on_again_when_no_report_comes(self):
        import signal
        import subprocess
        import sys
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(lambda: (child.kill(), child.wait()))
        sent = []
        kill = lambda pid, sig: sent.append((pid, sig))
        self.tw.listener_rows = lambda config, text: [("nc on 4444", child.pid)]
        answers = [{}, {"nc on 4444": "listening"}, {"nc on 4444": "listening"}, {"nc on 4444": "listening"}]
        self.wires = {"listeners": lambda config: answers.pop(0)}
        self.config["hold_timeout_min"] = 10
        self.check()
        self.check(now=1300.0, kill=kill)
        self.check(now=1300.0 + 9 * 60, kill=kill)
        self.assertEqual(sent, [(child.pid, signal.SIGSTOP)])
        self.check(now=1300.0 + 11 * 60, kill=kill)
        self.assertEqual(sent[-1], (child.pid, signal.SIGCONT))
        self.assertIn("timeout", json.loads(self.incidents()[0].read_text())["released"])

    def test_new_listener_is_stopped_then_continued(self):
        import signal
        import subprocess
        import sys
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(lambda: (child.kill(), child.wait()))
        sent = []
        rows = [("nc on 4444", child.pid)]
        self.tw.listener_rows = lambda config, text: rows
        answers = [{}, {"nc on 4444": "listening"}]
        self.wires = {"listeners": lambda config: answers.pop(0)}
        self.check()
        self.check(now=1300.0, kill=lambda pid, sig: sent.append((pid, sig)))
        incident = self.incidents()[0].parent
        self.assertEqual(sent, [(child.pid, signal.SIGSTOP)])
        (incident / "report.md").write_text("VERDICT: BENIGN\nFixture approval confirmed.\n")
        self.tw.release(incident.name, self.config, kill=lambda pid, sig: sent.append((pid, sig)))
        self.assertEqual(sent[-1], (child.pid, signal.SIGCONT))

    def test_changed_job_is_disabled_and_unloaded_then_restored_but_never_the_tripwire_job(self):
        agents = self.home / "Library" / "LaunchAgents"
        agents.mkdir(parents=True)
        self.config["launchd_dirs"] = [str(agents)]
        self.wires = {"launchd": self.tw.wire_launchd}
        calls = []
        loaded = True
        def launchctl(args):
            nonlocal loaded
            calls.append(args)
            if args[0] == "print":
                return 0 if loaded else 1
            if args[0] == "bootout":
                loaded = False
            if args[0] == "bootstrap":
                loaded = True
            return 0
        self.tw.launchctl = launchctl
        self.check()
        (agents / "com.example.stranger.plist").write_text(
            '<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>com.example.stranger</string></dict></plist>')
        (agents / "com.example.watch.plist").write_text("<plist>runs tripwire check</plist>")
        self.check(now=1300.0)
        incident = self.incidents()[0].parent
        self.assertEqual([c[0] for c in calls], ["print", "disable", "bootout"])
        self.assertTrue(all(c[1].endswith("/com.example.stranger") for c in calls))
        (incident / "report.md").write_text("VERDICT: BENIGN\nFixture approval confirmed.\n")
        self.tw.release(incident.name, self.config)
        self.assertEqual([c[0] for c in calls[3:]], ["enable", "print", "bootstrap", "print"])

    def test_failed_job_restoration_retains_only_unrestored_holds_and_bounds_retries(self):
        incident = self.state / "incidents" / "fixture"
        incident.mkdir(parents=True)
        jobs = [{"kind": "job", "label": label, "was_loaded": True, "plist": str(self.keys)}
                for label in ("recovering", "offline")]
        record = {"host": "fixture", "raised_epoch": 100, "changes": [], "holds": jobs, "stdout_report": True}
        self.tw.write_json(incident / "incident.json", record)
        report = incident / "report.md"
        report.write_text("VERDICT: BENIGN\nConfirmed cause in the fixture.")
        loaded = {"recovering"}
        bootstraps = []
        def launch(args):
            if args[0] == "print":
                return 0 if args[1].rsplit("/", 1)[-1] in loaded else 1
            if args[0] == "bootstrap":
                bootstraps.append(args)
                return 1
            return 0
        self.tw.launchctl = launch
        for i in range(5):
            self.check(now=1000+i)
        record = json.loads((incident / "incident.json").read_text())
        self.assertEqual([h["label"] for h in record["holds"]], ["offline"])
        self.assertEqual(record["restore_attempts"], 3)
        self.assertEqual(len(bootstraps), 3)
        self.assertNotIn("released", record)
        self.assertTrue(record["restore_notified"])
        loaded.add("offline")
        self.assertEqual(self.tw.release(incident.name, self.config, owner=True), 0)
        self.assertIn("released", json.loads((incident / "incident.json").read_text()))
        self.assertEqual(len(bootstraps), 3)

    def test_timeout_keeps_failed_operational_restoration_separate_from_file_holds(self):
        incident = self.hold_keys()
        path = incident / "incident.json"
        record = json.loads(path.read_text())
        record["holds"].append({"kind": "job", "label": "job", "was_loaded": False, "plist": str(self.keys)})
        self.tw.write_json(path, record)
        self.tw.launchctl = lambda _: 1
        self.check(now=4900)
        self.assertNotIn("released", json.loads(path.read_text()))
        self.assertEqual({h["kind"] for h in json.loads(path.read_text())["holds"]}, {"job", "file"})
        self.tw.launchctl = lambda _: 0
        self.check(now=5000)
        self.assertEqual([h["kind"] for h in json.loads(path.read_text())["holds"]], ["file"])
        self.assertNotIn("stranger", self.keys.read_text())

    def test_primary_reserves_time_for_a_configured_fallback(self):
        self.config.update(hold_timeout_min=2, summon_fallback=["/bin/echo"])
        with mock.patch.object(self.tw, "start_review") as start:
            self.hold_keys()
        self.assertEqual(start.call_args.args[1], "primary")
        self.assertEqual(start.call_args.args[6], 60)

    def test_non_benign_verdict_reaches_the_owner_once(self):
        incident = self.state / "incidents" / "20261114T000000Z-files"
        incident.mkdir(parents=True)
        (incident / "report.md").write_text("VERDICT: SUSPICIOUS\nno cause found\n")
        self.check()
        self.check(now=1300.0)
        told = [call for call in self.runner.calls if "SUSPICIOUS" in " ".join(call)]
        self.assertEqual(len(told), 2)  # one notification and one mail, from the first check only

    def test_benign_verdict_is_quiet(self):
        incident = self.state / "incidents" / "20261114T000000Z-files"
        incident.mkdir(parents=True)
        (incident / "report.md").write_text("VERDICT: BENIGN\nagentkit sync\n")
        self.check()
        self.assertEqual(self.runner.calls, [])


if __name__ == "__main__":
    unittest.main()
