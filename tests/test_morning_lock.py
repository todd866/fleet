import fcntl
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from fleet.morning.cli import main

class MorningLockTests(unittest.TestCase):
    def test_overlap_defers_without_collecting(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp) / 'morning'
            directory.mkdir()
            with (directory / 'run.lock').open('a+') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                with patch('fleet.morning.cli._main') as execute:
                    self.assertEqual(main(['--state', tmp]), 75)
                    execute.assert_not_called()

    def test_recent_completed_only_is_reused(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp) / 'morning'
            directory.mkdir()
            (directory / 'summary.json').write_text(json.dumps({'generated': 100}))
            (directory / 'completed.json').write_text(json.dumps({'generated': 100, 'completed_at': 110}))
            with patch('fleet.morning.cli._main', return_value=0) as execute:
                self.assertEqual(main(['--state', tmp, '--now', '120', '--reuse-within', '30']), 0)
                execute.assert_not_called()
                self.assertEqual(main(['--state', tmp, '--now', '150', '--reuse-within', '30']), 0)
                execute.assert_called_once()

    def test_incomplete_digest_is_not_reused(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp) / 'morning'
            directory.mkdir()
            (directory / 'summary.json').write_text(json.dumps({'generated': 100}))
            with patch('fleet.morning.cli._main', return_value=1) as execute:
                self.assertEqual(main(['--state', tmp, '--now', '120', '--reuse-within', '30']), 1)
                execute.assert_called_once()
                self.assertFalse((directory / 'completed.json').exists())

    def test_dry_run_does_not_write_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch('fleet.morning.cli._main', return_value=0) as execute:
                self.assertEqual(main(['--state', tmp, '--dry-run']), 0)
                execute.assert_called_once()
                self.assertFalse((Path(tmp) / 'morning').exists())
