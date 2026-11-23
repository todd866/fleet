#!/usr/bin/env python3
"""Tests for bin/gemini-usage quota parsing and timeout retry."""
import importlib.machinery
import importlib.util
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "bin" / "gemini-usage"


def load_gemini_usage():
    loader = importlib.machinery.SourceFileLoader("gemini_usage_mod", str(BIN))
    spec = importlib.util.spec_from_loader("gemini_usage_mod", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


class GeminiUsageTests(unittest.TestCase):
    def test_parse_usage_output_extracts_primary_and_extra_pools(self):
        mod = load_gemini_usage()
        sample = (
            "Gemini Models\tWeekly Limit Remaining\t100%\t2026-11-30T21:32:27Z\n"
            "Gemini Models\tFive Hour Limit Remaining\t98%\t2026-11-24T02:32:27Z\n"
            "Claude and GPT models\tFive Hour Limit Remaining\t100%\t2026-11-24T03:33:28Z\n"
        )
        rows = mod.parse_usage_output(sample)
        self.assertEqual([r["pool"] for r in rows], ["week", "5h", "claude-gpt-5h"])
        self.assertEqual(rows[1]["remainingPct"], 98.0)

    def test_main_retries_once_on_timeout(self):
        mod = load_gemini_usage()
        sample = "Gemini Models\tFive Hour Limit Remaining\t99%\t2026-11-24T02:32:27Z\n"
        ok_proc = subprocess.CompletedProcess(["agy"], 0, stdout=sample, stderr="")
        calls = []

        def fake_run(*args, **kwargs):
            calls.append(kwargs.get("timeout"))
            if len(calls) == 1:
                raise subprocess.TimeoutExpired(["agy"], 65)
            return ok_proc

        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            fake_agy = tmp_path / "agy"
            fake_agy.write_text("#!/bin/sh\n")
            with mock.patch.object(mod, "AGY", fake_agy), \
                 mock.patch.object(mod, "STATE", tmp_path / "state"), \
                 mock.patch.object(mod, "scan_transcript_tokens", return_value={
                     "todayTokensAll": 0, "todayTokens": 0, "weekTokensAll": 0,
                     "weekTokens": 0, "turnsToday": 0, "poolTokensUsed": {},
                 }), \
                 mock.patch.object(mod.subprocess, "run", side_effect=fake_run):
                rc = mod.main()
            self.assertEqual(rc, 0)
            self.assertEqual(len(calls), 2)
            self.assertTrue((tmp_path / "state" / "quota.json").is_file())


if __name__ == "__main__":
    unittest.main()
