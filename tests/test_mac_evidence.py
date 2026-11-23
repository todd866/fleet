#!/usr/bin/env python3
"""Tests for tools/security-evidence/mac_evidence.py unified log chunking."""
import copy
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools" / "security-evidence"))

import mac_evidence as me  # noqa: E402


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


if __name__ == "__main__":
    unittest.main()
