#!/usr/bin/env python3
"""Tests for tools/security-evidence/mac_evidence.py and evidence-audit."""
import copy
import importlib.machinery
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SEC_DIR = ROOT / "tools" / "security-evidence"
sys.path.insert(0, str(SEC_DIR))

import evidence_chain as ec  # noqa: E402
import mac_evidence as me  # noqa: E402


def load_audit():
    loader = importlib.machinery.SourceFileLoader("evidence_audit_mod", str(SEC_DIR / "evidence-audit"))
    mod = importlib.util.module_from_spec(importlib.util.spec_from_loader("evidence_audit_mod", loader))
    loader.exec_module(mod)
    return mod


ea = load_audit()


class UnifiedLogChunkingTests(unittest.TestCase):
    def _ctx(self, now, run, **cfg_over):
        cfg = copy.deepcopy(me.DEFAULTS)
        cfg.update(cfg_over)
        return {"now": now, "cfg": cfg, "run": run}

    def test_chunks_long_lookback_and_advances_cursor(self):
        calls = []
        now = 1_795_445_000.0
        st = {"end": now - 3000}

        def fake_run(argv, timeout):
            calls.append((argv[argv.index("--start") + 1], argv[argv.index("--end") + 1], timeout))
            ev = {
                "timestamp": "2026-11-23 13:15:00.000000+0000",
                "machTimestamp": len(calls),
                "processID": 10 + len(calls),
                "processImagePath": "/kernel",
                "eventMessage": "PMRD: System Sleep",
            }
            return 0, json.dumps(ev) + "\n"

        rows = me.src_unified_log(
            self._ctx(now, fake_run, log_chunk_s=900, log_max_chunks=4, log_slice_timeout_s=60),
            st,
        )
        self.assertEqual(len(calls), 4)
        self.assertTrue(all(c[2] == 60 for c in calls))
        # Start = prev_end - 60 = now - 3060; 4 * 900s = +3600 -> reaches `now`.
        self.assertEqual(st["end"], now)
        power_rows = [r for stream, r in rows if stream == "power"]
        self.assertEqual(len(power_rows), 4)
        self.assertEqual(rows[-1][0], "keychain")

    def test_timeout_slice_records_gap_and_advances_without_wedging(self):
        now = 1_795_445_000.0
        st = {"end": now - 1500}
        step = 0

        def fake_run(argv, timeout):
            nonlocal step
            step += 1
            if step == 1:
                return 124, ""
            ev = {
                "timestamp": "2026-11-23 13:40:00.000000+0000",
                "machTimestamp": 99,
                "processID": 42,
                "processImagePath": "/kernel",
                "eventMessage": "PMRD: System Wake",
            }
            return 0, json.dumps(ev) + "\n"

        rows = me.src_unified_log(
            self._ctx(now, fake_run, log_chunk_s=900, log_max_chunks=4, log_slice_timeout_s=60),
            st,
        )
        gaps = [r for stream, r in rows if stream == "collector" and r.get("ev") == "gap"]
        wakes = [r for stream, r in rows if stream == "power" and r.get("ev") == "wake"]
        self.assertEqual(len(gaps), 1)
        self.assertEqual(gaps[0]["why"], "log show timed out")
        self.assertEqual(len(wakes), 1)
        self.assertEqual(st["end"], now)

    def test_powerd_sleep_and_wake_parsed(self):
        now = 1_795_445_000.0
        st = {"end": now - 300}

        def fake_run(_argv, _timeout):
            evs = [
                {
                    "timestamp": "2026-11-23 13:40:00.000000+0000",
                    "machTimestamp": 1,
                    "processID": 368,
                    "processImagePath": "/System/Library/CoreServices/powerd.bundle/powerd",
                    "eventMessage": "Entering Sleep state due to 'Idle Sleep'",
                },
                {
                    "timestamp": "2026-11-23 13:42:00.000000+0000",
                    "machTimestamp": 2,
                    "processID": 368,
                    "processImagePath": "/System/Library/CoreServices/powerd.bundle/powerd",
                    "eventMessage": "Wake from Normal Sleep [CDNVA] : due to EC.LidOpen/Lid Open",
                },
            ]
            return 0, "\n".join(json.dumps(e) for e in evs) + "\n"

        rows = me.src_unified_log(self._ctx(now, fake_run), st)
        power = [r for s, r in rows if s == "power"]
        self.assertEqual([r["ev"] for r in power], ["sleep", "wake"])
        self.assertEqual([r["src"] for r in power], ["powerd", "powerd"])


class CollectorResilienceTests(unittest.TestCase):
    def test_enforce_cap_ignores_vanished_acked_batch(self):
        with tempfile.TemporaryDirectory() as tmp:
            state = Path(tmp)
            sd = me.spool_dir(state)
            b1 = sd / "acked" / "0000000001.batch"
            b2 = sd / "acked" / "0000000002.batch"
            b1.write_bytes(b"a" * 100)
            b2.write_bytes(b"b" * 100)

            orig_stat = Path.stat

            def flaky_stat(self, *a, **kw):
                if self.name == "0000000001.batch":
                    raise FileNotFoundError(self)
                return orig_stat(self, *a, **kw)

            with mock.patch.object(Path, "stat", flaky_stat):
                dropped = me.enforce_cap(state, 50)
            self.assertEqual(dropped, [])
            self.assertFalse(b2.exists())

    def test_quick_continues_shipping_when_lsof_times_out(self):
        with tempfile.TemporaryDirectory() as tmp:
            state = Path(tmp)
            cfg = copy.deepcopy(me.DEFAULTS)
            ledger = state / "guard.jsonl"
            ledger.write_text('{"t":"2026-11-24T01:00:00Z","decision":"allow"}\n')
            cfg["ledgers"] = {"guard": str(ledger), "cred": str(state / "cred.jsonl"), "net": str(state / "net.jsonl")}

            def fake_run(argv, _timeout):
                if argv[0] == "/usr/sbin/lsof":
                    return 124, ""
                return 0, ""

            res = me.quick(cfg, state, run=fake_run)
            self.assertTrue(res["net"]["timeout"])
            self.assertGreaterEqual(res["collect"]["batches"], 1)


class EvidenceAuditRegressionTests(unittest.TestCase):
    def _write_archive(self, root: Path, ns: str, stream: str, day: str, rows: list[dict]):
        p = root / "archive" / ns / stream / f"{day}.jsonl"
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("".join(json.dumps(r) + "\n" for r in rows))

    def test_overlapping_window_seq_replay_no_false_positive_but_real_rewrite_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            day = "2026-11-24"
            t1 = ec.parse_iso("2026-11-24T01:00:00Z")
            t2 = ec.parse_iso("2026-11-24T02:00:00Z")
            t3 = ec.parse_iso("2026-11-24T03:00:00Z")
            self._write_archive(
                root, "laptop", "collector", day,
                [{"t": "2026-11-24T02:00:00Z", "ev": "run"}],
            )
            self._write_archive(
                root, "laptop", "guard", day,
                [
                    {"t": "2026-11-24T01:00:00Z", "decision": "allow", "_seq": [101, 500, 10]},
                    {"t": "2026-11-24T02:00:00Z", "decision": "allow", "_seq": [101, 2000, 20]},
                ],
            )
            acfg = copy.deepcopy(ea.DEFAULT_AUDIT)
            settings = {"kind": "host"}
            # Previous audit stopped at t2 (offset 2000). Replaying [t1..t2] must NOT fail.
            profile = {"seq": {"guard": [101, 2000, t2]}}
            with mock.patch.object(ea.ec, "verify_archive", return_value={"links": 1, "problems": []}):
                res1 = ea.audit_ns(root, "laptop", settings, acfg, t1 - 60, t2 + 60, profile)
            self.assertFalse(any(f["rule"] == "ledgers" for f in res1["findings"]))

            # A later row at t3 with offset < 2000 IS a real backward rewrite and MUST fail.
            self._write_archive(
                root, "laptop", "guard", day,
                [
                    {"t": "2026-11-24T01:00:00Z", "decision": "allow", "_seq": [101, 500, 10]},
                    {"t": "2026-11-24T02:00:00Z", "decision": "allow", "_seq": [101, 2000, 20]},
                    {"t": "2026-11-24T03:00:00Z", "decision": "allow", "_seq": [101, 1500, 21]},
                ],
            )
            with mock.patch.object(ea.ec, "verify_archive", return_value={"links": 1, "problems": []}):
                res2 = ea.audit_ns(root, "laptop", settings, acfg, t1 - 60, t3 + 60, profile)
            self.assertTrue(any(f["rule"] == "ledgers" and "went back" in f["text"] for f in res2["findings"]))

    def test_guard_fallback_and_dedup_plus_cred_dedup(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            day = "2026-11-24"
            t1 = ec.parse_iso("2026-11-24T01:00:00Z")
            t2 = ec.parse_iso("2026-11-24T02:00:00Z")
            self._write_archive(
                root, "laptop", "collector", day,
                [{"t": "2026-11-24T01:30:00Z", "ev": "run"}],
            )
            self._write_archive(
                root, "laptop", "guard", day,
                [
                    {"t": "2026-11-24T01:10:00Z", "provider": "agy", "ppid": 444, "session": "", "decision": "deny", "rule": "guard-config", "cwd": "/var/tmp/example/project"},
                    {"t": "2026-11-24T01:11:00Z", "provider": "agy", "ppid": 444, "session": "", "decision": "deny", "rule": "guard-config", "cwd": "/var/tmp/example/project"},
                ],
            )
            self._write_archive(
                root, "laptop", "cred", day,
                [
                    {"t": "2026-11-24T01:10:00Z", "op": "_values", "chain": ["/usr/bin/python3"]},
                    {"t": "2026-11-24T01:11:00Z", "op": "_values", "chain": ["/usr/bin/python3"]},
                    {"t": "2026-11-24T01:12:00Z", "op": "_values", "chain": ["/usr/bin/python3"]},
                ],
            )
            acfg = copy.deepcopy(ea.DEFAULT_AUDIT)
            acfg["cred_values_callers"] = ["/var/tmp/example/.local/bin/pw-mcp-tabsafe"]
            with mock.patch.object(ea.ec, "verify_archive", return_value={"links": 1, "problems": []}):
                res = ea.audit_ns(root, "laptop", {"kind": "host"}, acfg, t1, t2, {})
            guard_fails = [f for f in res["findings"] if f["rule"] == "guard" and f["level"] == "fail"]
            cred_warns = [f for f in res["findings"] if f["rule"] == "cred"]
            self.assertEqual(len(guard_fails), 1)
            self.assertIn("agy:444", guard_fails[0]["text"])
            self.assertIn("/var/tmp/example/project", guard_fails[0]["text"])
            self.assertIn("(x2)", guard_fails[0]["text"])
            self.assertEqual(len(cred_warns), 1)
            self.assertIn("(x3)", cred_warns[0]["text"])


class PrivilegeAuditSourceTests(unittest.TestCase):
    def test_privilege_audit_baseline_unchanged_and_drift(self):
        cfg = copy.deepcopy(me.DEFAULTS)
        sample_one = {
            "system_extensions": [{"bundle_id": "com.objective-see.lulu.extension", "state": "activated enabled"}],
            "privileged_helper_tools": ["com.docker.socket"],
            "launch_daemons": ["com.docker.socket.plist"],
            "launch_agents": [],
            "tcc": {"grant_count": 41, "orphaned": [{"service": "Accessibility", "client": "com.logmein.Rescue"}]},
            "chrome": {"force_installed_ids": ["mmlmfjhmonkocbjadbfplnigmagldckm"]},
        }
        sample_two = copy.deepcopy(sample_one)
        sample_two["privileged_helper_tools"] = []

        responses = [
            (0, json.dumps(sample_one)),
            (0, json.dumps(sample_one)),
            (0, json.dumps(sample_two)),
        ]
        st = {}
        ctx = {"now": 1_795_500_000.0, "cfg": cfg, "run": lambda argv, timeout: responses.pop(0)}

        r1 = me.src_privilege_audit(ctx, st)
        self.assertEqual(len(r1), 1)
        self.assertEqual(r1[0][0], "manifest")
        self.assertEqual(r1[0][1]["set"], "privilege_audit")
        self.assertEqual(r1[0][1]["ev"], "baseline")

        r2 = me.src_privilege_audit(ctx, st)
        self.assertEqual(r2, [])

        r3 = me.src_privilege_audit(ctx, st)
        self.assertEqual(len(r3), 1)
        self.assertEqual(r3[0][0], "manifest")
        self.assertEqual(r3[0][1]["ev"], "changed")
        self.assertEqual(r3[0][1]["prev_sha256"], r1[0][1]["sha256"])

    def test_evidence_audit_flags_privilege_audit_change_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            ns_dir = root / "archive" / "laptop"
            (ns_dir / "collector").mkdir(parents=True)
            (ns_dir / "manifest").mkdir(parents=True)
            now = ec.parse_iso("2026-11-24T12:00:00Z")
            t_col = ec.iso_now(now - 60)
            (ns_dir / "collector" / "2026-11-24.jsonl").write_text(
                json.dumps({"t": t_col, "ev": "run"}) + "\n"
            )
            posture = {
                "system_extensions": [["com.objective-see.lulu.extension", "activated enabled"]],
                "privileged_helper_tools": ["com.microsoft.autoupdate.helper"],
                "launch_daemons": ["com.microsoft.autoupdate.helper.plist"],
                "launch_agents": [],
                "tcc_grant_count": 41,
                "tcc_orphaned": [["Accessibility", "com.logmein.Rescue"]],
                "chrome_force_installed": ["mmlmfjhmonkocbjadbfplnigmagldckm"],
            }
            manifest_rows = [
                {"t": ec.iso_now(now - 120), "set": "privilege_audit", "ev": "baseline", "sha256": "a" * 64, "posture": posture},
                {"t": ec.iso_now(now - 30), "set": "privilege_audit", "ev": "changed", "prev_sha256": "a" * 64, "sha256": "b" * 64, "posture": posture},
            ]
            (ns_dir / "manifest" / "2026-11-24.jsonl").write_text(
                "".join(json.dumps(r) + "\n" for r in manifest_rows)
            )
            acfg = dict(ea.DEFAULT_AUDIT, gap_min=10_000)
            with mock.patch.object(ea.ec, "verify_archive", return_value={"problems": [], "links": 1}):
                res = ea.audit_ns(root, "laptop", {"kind": "host"}, acfg, now - 3600, now, {})
            priv = [f for f in res["findings"] if f["rule"] == "privilege"]
            self.assertEqual(len(priv), 1)
            self.assertEqual(priv[0]["level"], "warn")
            self.assertIn("helpers=com.microsoft.autoupdate.helper", priv[0]["text"])
            self.assertIn("sysexts=com.objective-see.lulu.extension", priv[0]["text"])


if __name__ == "__main__":
    unittest.main()
