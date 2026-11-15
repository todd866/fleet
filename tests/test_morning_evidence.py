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
from fleet.morning.collect import _host_snapshot
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

if __name__ == '__main__':
    unittest.main()
