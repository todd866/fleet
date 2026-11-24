"""Fresh health evidence and honest delivery receipts; no real notifications or backups."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from fleet.morning import probe
from fleet.morning.cli import _deliver, main
from fleet.morning.collect import _host_snapshot, _surface_probes, _surface_probes_local
from fleet.morning.config import _merge, DEFAULTS
from fleet.morning.evaluate import evaluate


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)
        self.home_patch = patch.object(probe, 'HOME', self.home)
        self.home_patch.start()
        self.addCleanup(self.home_patch.stop)
        hold_patch = patch.object(probe, '_idrive_hold', return_value=None)   # not this Mac's real hold
        hold_patch.start()
        self.addCleanup(hold_patch.stop)
        self.now = int(time.time())

    def summary(self, name='latest', failed=0, age=3600, text=None):
        path = self.home / 'Library/Application Support/IDriveforMac/SessionLogsNew/Backup' / (name + '.summary')
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text if text is not None else f'End Time: 11-07-2026 18:12:28\nFiles failed to backup: {failed}\n')
        os.utime(path, (self.now-age, self.now-age))
        return path

    def test_local_attempt_wins_over_stale_diskmap(self):
        self.summary()
        with patch.object(probe, '_json', return_value={'locations': [{'kind': 'idrive', 'detail': 'ok 54d'}]}):
            self.assertEqual(probe._idrive_epoch(), self.now-3600)

    def test_success_can_omit_failed_count_when_file_counts_reconcile(self):
        self.summary(text='End Time: 11-07-2026 21:16:06\nFiles considered for backup: 228723\nFiles already present in your account: 227533\nFiles backed up now: 1190 files (7.39 GB)\n')
        self.assertEqual(probe._idrive_reading()['failed'], 0)

    def test_missing_failed_count_without_reconciled_counts_is_unknown(self):
        self.summary(text='End Time: 11-07-2026 21:16:06\nFiles considered for backup: 100\nFiles already present in your account: 50\nFiles backed up now: 49 files\n')
        self.assertIsNone(probe._idrive_epoch())

    def test_latest_partial_backup_is_not_green(self):
        self.summary('older', age=7200)
        self.summary(failed=5)
        reading = probe._idrive_reading()
        host = _host_snapshot('primary', {'idrive': reading, 'idrive_epoch': reading['last_epoch']}, {}, {}, {}, self.now)
        found = next(r for r in evaluate({'hosts': {'primary': host}}, {}, self.now) if r.key == 'backup:primary:idrive')
        self.assertEqual(found.status, 'fail')
        self.assertIn('5 files failed', found.detail)

    def test_explicit_unknown_never_uses_old_health(self):
        host = _host_snapshot('primary', {'idrive': probe._idrive_reading()}, {}, {}, {'hosts': {'primary': {'idrive': {'latest': {'observed_mtime': self.now}}}}}, self.now)
        self.assertIsNone(host['idrive']['last_epoch'])

    def test_incomplete_and_future_summary_fail_closed(self):
        self.summary(text='Files failed to backup: 0\n')
        self.assertIsNone(probe._idrive_epoch())
        self.summary(age=-3600)
        self.assertIsNone(probe._idrive_epoch())

    def test_physical_internal_space_replaces_cached_free(self):
        summary = {'locations': [{'name': 'internal', 'free_bytes': 999e9, 'last_seen': 1}]}
        with patch.object(probe, '_json', return_value=summary), patch.object(probe.shutil, 'disk_usage', return_value=SimpleNamespace(free=12e9)):
            rows = probe._disks()
        self.assertEqual(rows[0]['free_bytes'], 12e9)
        self.assertGreaterEqual(rows[0]['last_seen'], self.now)

    def test_unreadable_live_disk_does_not_use_cached_green(self):
        with patch.object(probe, '_json', return_value={'locations': [{'name': 'internal', 'free_bytes': 999e9}]}), patch.object(probe.shutil, 'disk_usage', side_effect=OSError):
            self.assertEqual(probe._disks(), [])

    def test_failed_live_disk_probe_cannot_fall_back_to_health_cache(self):
        health = {"hosts": {"primary": {"diskmap": {"locations": [{"name": "internal", "free_bytes": 999e9}]}}}}
        host = _host_snapshot('primary', {'disks': [], 'disks_authoritative': True}, {}, {}, health, self.now)
        self.assertEqual(host['disks'], [])

    def test_wrong_notification_host_is_explicit(self):
        with patch('fleet.morning.cli.subprocess.run') as run:
            result = _deliver('primary', 'hub', 'problem')
        run.assert_not_called()
        self.assertEqual(result['status'], 'wrong-host')
        self.assertFalse(result['assistant_woken'])

    def test_notification_exit_and_timeout_are_not_success(self):
        for outcome in (SimpleNamespace(returncode=1), subprocess.TimeoutExpired('osascript', 15), OSError()):
            kwargs = {'side_effect': outcome} if isinstance(outcome, Exception) else {'return_value': outcome}
            with patch('fleet.morning.cli.subprocess.run', **kwargs):
                self.assertEqual(_deliver('hub', 'hub', 'problem')['status'], 'failed')

    def test_submission_does_not_claim_user_saw_it(self):
        with patch('fleet.morning.cli.subprocess.run', return_value=SimpleNamespace(returncode=0)):
            result = _deliver('hub', 'hub', 'problem')
        self.assertEqual(result['status'], 'submitted')
        self.assertIsNone(result['user_seen'])
        self.assertFalse(result['assistant_woken'])

    def test_cli_persists_actionable_failed_delivery(self):
        snapshot = self.home/'input.json'
        snapshot.write_text(json.dumps({'hosts': {'hub': {'ok': True, 'idrive': None}}}))
        with patch('fleet.morning.cli._sync_cowork'), patch('fleet.morning.cli.subprocess.run') as run:
            main(['--snapshot', str(snapshot), '--home', str(self.home), '--here', 'hub', '--notify-host', 'primary'])
        receipt = json.loads((self.home/'.local/state/fleet/morning/delivery.json').read_text())
        self.assertEqual(receipt['status'], 'wrong-host')
        self.assertIn('backup:hub:idrive', receipt['actionable_keys'])
        run.assert_not_called()

    def test_surface_probes_ignores_observe_lines_and_flags_ip_bursts(self):
        from datetime import datetime, timezone
        iso_now = datetime.fromtimestamp(self.now - 120, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        iso_old = datetime.fromtimestamp(self.now - 36 * 3600, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        app_log = self.home / "logs/web.log"
        api_log = self.home / "logs/api.log"
        mcp_log = self.home / "logs/mcp.log"
        app_log.parent.mkdir(parents=True, exist_ok=True)
        # Observe lines (non-enforced) and old lines should not trigger bursts
        app_log.write_text(
            f"{iso_now} auth observe: would reject GET /api/draw from 192.0.2.99 ua=\"Browser\"\n"
            f"{iso_old} auth enforce: rejected GET /secret from 198.51.100.2 ua=\"Scanner\" token=absent\n"
            f"{iso_now} auth enforce: rejected GET /api/shows from 198.51.100.3 ua=\"Scanner\" token=wrong\n"
        )
        # Single-IP burst of 25 404s -> warn (>= ip_warn 20)
        api_lines = [f'[{iso_now}] ip=198.51.100.55 "GET /bad/{i}?t=REDACTED HTTP/1.1" 404 - ua="dirsearch"\n' for i in range(25)]
        api_lines.append(f'[{iso_now}] ip=127.0.0.1 "GET /v1/health HTTP/1.1" 401 - ua="curl/8.0"\n')
        api_log.write_text("".join(api_lines))
        # MCP log with >= 100 rejections from one IP -> fail (>= ip_fail 100)
        mcp_log.write_text("".join(f"{iso_now} reject 404 GET /probe/{i} from=203.0.113.88 ua=\"zgrab\"\n" for i in range(105)))

        cfg = _merge(DEFAULTS, {
            "surface_probes": {
                "logs": [
                    {"name": "app", "path": "~/logs/web.log"},
                    {"name": "api", "path": "~/logs/api.log"},
                    {"name": "mcp", "path": "~/logs/mcp.log"},
                ],
                "window_h": 24, "ip_warn": 20, "ip_fail": 100, "total_warn": 50, "total_fail": 300,
            },
        })
        local = _surface_probes_local(self.home, cfg, self.now)
        merged = _surface_probes({"hub": {"surface_probes": local}}, self.home, cfg, self.now)
        rows = {r.key: r for r in evaluate({"hosts": {}, "surface_probes": merged}, cfg, self.now)}

        self.assertEqual(rows["sec:probes:app"].status, "pass")
        self.assertEqual(rows["sec:probes:app"].value, "1 err (1 IP)")
        self.assertEqual(rows["sec:probes:api"].status, "warn")
        self.assertEqual(rows["sec:probes:api"].datum, "top 198.51.100.55 x25")
        self.assertEqual(rows["sec:probes:mcp"].status, "fail")
        self.assertEqual(rows["sec:probes:mcp"].datum, "top 203.0.113.88 x105")

    def test_vpn_gateway_evaluation_pass_warn_fail_and_absent(self):
        hosts = {
            "green": {
                "ok": True,
                "vpn_gateway": {
                    "pac_ok": True,
                    "domains": 5,
                    "vpn_ok": True,
                    "vpn_ip": "86.104.22.87",
                    "vpn_country": "Netherlands",
                    "transmission_ok": True,
                },
            },
            "txdown": {
                "ok": True,
                "vpn_gateway": {
                    "pac_ok": True,
                    "domains": 5,
                    "vpn_ok": True,
                    "vpn_ip": "86.104.22.87",
                    "vpn_country": "Netherlands",
                    "transmission_ok": False,
                },
            },
            "vpndown": {
                "ok": True,
                "vpn_gateway": {
                    "pac_ok": False,
                    "domains": 0,
                    "vpn_ok": False,
                    "vpn_ip": "",
                    "vpn_country": "",
                    "transmission_ok": False,
                },
            },
            "hub": {"ok": True},
        }
        rows = {r.key: r for r in evaluate({"hosts": hosts}, DEFAULTS, self.now)}
        self.assertEqual(rows["sec:vpn:green"].status, "pass")
        self.assertEqual(rows["sec:vpn:green"].value, "up (86.104.22.87)")
        self.assertEqual(rows["sec:vpn:green"].datum, "5 domains")
        self.assertEqual(rows["sec:vpn:txdown"].status, "warn")
        self.assertEqual(rows["sec:vpn:txdown"].datum, "transmission down")
        self.assertEqual(rows["sec:vpn:vpndown"].status, "fail")
        self.assertEqual(rows["sec:vpn:vpndown"].datum, "pac,gluetun")
        self.assertNotIn("sec:vpn:hub", rows)

    def test_vpn_gateway_probe_reads_pac_domains_count_and_gluetun_and_transmission(self):
        self.assertIsNone(probe._vpn_gateway())
        domains_file = self.home / ".config/fleet/vpn-domains.txt"
        domains_file.parent.mkdir(parents=True, exist_ok=True)
        domains_file.write_text("thepiratebay.org\n")

        class FakeResp:
            def __init__(self, status, payload=b""):
                self.status = status
                self._payload = payload

            def read(self):
                return self._payload

            def __enter__(self):
                return self

            def __exit__(self, exc_type, exc, tb):
                return False

        class FakeOpener:
            def open(self, url, timeout=2):
                if ":8899/health" in url:
                    return FakeResp(200, json.dumps({
                        "ok": True,
                        "domains_count": 5,
                        "domains": ["a", "b", "c", "d", "e"],
                    }).encode())
                if ":8890/v1/publicip/ip" in url:
                    return FakeResp(200, json.dumps({"public_ip": "86.104.22.87", "country": "Netherlands"}).encode())
                if ":9091/transmission/web/" in url:
                    raise probe.urllib.error.HTTPError(url, 409, "Conflict", {}, None)
                raise AssertionError(url)

        with patch.object(probe.urllib.request, "build_opener", return_value=FakeOpener()):
            got = probe._vpn_gateway()
        self.assertEqual(got, {
            "pac_ok": True,
            "domains": 5,
            "vpn_ok": True,
            "vpn_ip": "86.104.22.87",
            "vpn_country": "Netherlands",
            "transmission_ok": True,
        })

if __name__ == '__main__':
    unittest.main()
