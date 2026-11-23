#!/usr/bin/env python3
"""Unit and HTTP integration tests for fleet-api."""
from __future__ import annotations

import importlib.util
import json
import os
import plistlib
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("fleet_api_server", str(ROOT / "fleet-api" / "server.py"))
assert SPEC and SPEC.loader
srv = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(srv)


class FleetAPITests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.home = root / "home"
        self.mac_sync = self.home / ".local/state/mac-sync"
        self.evidence = root / "evidence"
        self.ota = self.home / ".local/state/fleet-api/ota"

        (self.mac_sync / "fleet-morning").mkdir(parents=True)
        (self.mac_sync / "fleet-snapshots").mkdir(parents=True)
        (self.mac_sync / "storage-audit").mkdir(parents=True)
        (self.home / ".local/state/ai-capacity").mkdir(parents=True)
        (self.home / ".local/state/idrive-ingest").mkdir(parents=True)
        (self.home / ".local/state/gdrive-backup").mkdir(parents=True)
        (self.home / ".local/state/agent-mail").mkdir(parents=True)
        (self.home / ".local/state/cowork").mkdir(parents=True)
        (self.home / ".config/agentkit").mkdir(parents=True)
        (self.evidence / "audit/primary").mkdir(parents=True)
        self.ota.mkdir(parents=True)

        (self.mac_sync / "storage-audit/free-targets.tsv").write_text("primary\t500\t500\t100\n")
        (self.mac_sync / "fleet-morning/summary.json").write_text(json.dumps({
            "version": 1,
            "generated": 1795473000,
            "date": "2026-11-24",
            "ok": False,
            "counts": {"pass": 291, "warn": 42, "fail": 25},
            "cadence": {"text": "daily, next at 22:30Z"},
            "rows": [
                {"key": "disk:primary:internal", "status": "pass", "area": "disk", "name": "primary internal", "value": "250 GB", "datum": "ok", "detail": "ok"},
                {"key": "job:hub:demo", "status": "fail", "area": "job", "name": "hub demo", "value": "exit 1", "datum": "demo", "detail": "failed"},
            ],
        }))
        (self.mac_sync / "fleet-morning/agy-morning.md").write_text("1. Top item: test\n")
        (self.mac_sync / "fleet-snapshots/primary.json").write_text(json.dumps({
            "host": "primary",
            "collected_at": 1795472528,
            "reading": {
                "ok": True,
                "owner_active": False,
                "samples": [{"t": 1795472500, "chipC": 58.2, "thermal": "nominal", "cpu": 24.5, "memPressure": "Low", "load": [2.1, 2.0, 1.9], "batt": 80, "ac": True, "diskFreeGB": 251.0, "diskTotalGB": 1995.2}],
                "disks": [{"name": "internal", "mounted": True, "free_bytes": 251_000_000_000, "total_bytes": 1_995_000_000_000}],
                "syncthing": {"errors": 0, "out_of_sync": 0},
                "jobs": [{"label": "com.example.ok", "loaded": True, "exit": 0, "when": "every 5m", "state": "not running"}],
                "mail": [{"id": "1", "read": False}],
                "placement": {"generated": 1795473000, "ok": False, "violations": [{"host": "hub", "dataset": "Photos", "code": "offsite", "detail": "no offsite"}]},
            },
        }))
        (self.home / ".local/state/ai-capacity/shared.json").write_text(json.dumps({
            "generatedAt": "2026-11-23T23:01:00Z",
            "services": {"gemini": {"pools": [{"pool": "5h", "remainingPct": 98.0, "freshness": "fresh", "resetAt": "2026-11-24T02:32:27Z"}]}},
        }))
        (self.home / ".config/agentkit/paused-providers").write_text("# comment\nclaude\n")
        (self.home / ".local/state/idrive-ingest/status.json").write_text(json.dumps({"running": True, "progress": {"percent": 17.5, "sent_bytes": 100, "total_bytes": 500}}))
        (self.home / ".local/state/gdrive-backup/receipts.json").write_text(json.dumps({"home": {"running": True, "last_ok": 1795470000}}))
        (self.evidence / "check.json").write_text(json.dumps({"status": "fail", "value": "1 fail", "rows": []}))
        (self.evidence / "status.json").write_text(json.dumps({"status": "pass", "namespaces": {}}))
        (self.evidence / "audit/latest.json").write_text(json.dumps({"status": "fail"}))
        (self.evidence / "audit/primary/2026-11-24.json").write_text(json.dumps({"status": "fail", "counts": {"runs": 10}, "findings": [{"level": "fail", "rule": "silence", "text": "gap"}]}))
        (self.home / ".local/state/agent-mail/inbox.jsonl").write_text(json.dumps({"id": "msg-1", "at": "2026-11-24 07:00:00", "from": "primary", "project": "fleet", "text": "hello"}) + "\n")
        (self.home / ".local/state/cowork/board.json").write_text(json.dumps({"claims": []}))

        self.token = "a" * 64
        self.cfg = dict(srv.DEFAULT_CONFIG)
        self.cfg.update({
            "bind": "127.0.0.1",
            "port": 0,
            "public_base": "https://hub.example.ts.net:8444",
            "home_dir": str(self.home),
            "mac_sync_dir": str(self.mac_sync),
            "evidence_dir": str(self.evidence),
            "ota_dir": str(self.ota),
            "audit_log": str(self.home / ".local/state/fleet-api/actions.jsonl"),
            "bundle_id": "org.example.fleet",
            "app_title": "Fleet",
        })
        store = srv.FleetDataStore(self.cfg)
        self.server = srv.FleetHTTPServer(("127.0.0.1", 0), store, self.token, self.cfg)
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.tmp.cleanup()

    def _req(self, path: str, method: str = "GET", body: dict | None = None, token: str | None = None):
        data = json.dumps(body).encode("utf-8") if body is not None else None
        headers = {}
        if token is not None:
            headers["Authorization"] = f"Bearer {token}"
        if data is not None:
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.read()
        except urllib.error.HTTPError as e:
            with e:
                return e.code, e.read()

    def test_auth_required_and_ota_silent_404(self) -> None:
        code, _ = self._req("/v1/overview")
        self.assertEqual(code, 401)
        code, body = self._req("/v1/ota/manifest.plist?t=wrong")
        self.assertEqual(code, 404)
        self.assertEqual(body, b"")

    def test_read_endpoints_return_expected_shapes(self) -> None:
        for ep in ("/v1/health", "/v1/overview", "/v1/hosts", "/v1/morning", "/v1/capacity", "/v1/backups", "/v1/security", "/v1/mail"):
            code, raw = self._req(ep, token=self.token)
            self.assertEqual(code, 200, ep)
            doc = json.loads(raw.decode("utf-8"))
            self.assertIsInstance(doc, dict)

        _, raw = self._req("/v1/overview", token=self.token)
        ov = json.loads(raw.decode("utf-8"))
        self.assertEqual(ov["counts"]["fail"], 25)
        self.assertEqual(ov["hosts"][0]["disk_free_gb"], 251.0)
        self.assertEqual(ov["backups"]["idrive"]["percent"], 17.5)
        self.assertEqual(ov["unread_mail"], 1)

    def test_ota_manifest_and_ipa_download(self) -> None:
        (self.ota / "Fleet.ipa").write_bytes(b"PK-FAKE-IPA")
        code, xml_bytes = self._req(f"/v1/ota/manifest.plist?t={self.token}")
        self.assertEqual(code, 200)
        pl = plistlib.loads(xml_bytes)
        self.assertEqual(pl["items"][0]["metadata"]["bundle-identifier"], "org.example.fleet")
        code, ipa = self._req(f"/v1/ota/Fleet.ipa?t={self.token}")
        self.assertEqual(code, 200)
        self.assertEqual(ipa, b"PK-FAKE-IPA")

    def test_actions_voice_note_and_provider_pause(self) -> None:
        code, raw = self._req("/v1/actions/voice-note", method="POST", body={"text": "test dictation"}, token=self.token)
        self.assertEqual(code, 200)
        self.assertTrue(json.loads(raw)["recorded"])
        self.assertIn("test dictation", (self.mac_sync / "VOICE-INBOX.md").read_text())

        code, raw = self._req("/v1/actions/provider-pause", method="POST", body={"provider": "zai", "paused": True}, token=self.token)
        self.assertEqual(code, 200)
        self.assertIn("zai", json.loads(raw)["list"])

        code, _ = self._req("/v1/actions/provider-pause", method="POST", body={"provider": "gemini", "paused": True}, token=self.token)
        self.assertEqual(code, 400)


if __name__ == "__main__":
    unittest.main()
