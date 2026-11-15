#!/usr/bin/env python3
"""fleet-join: steps run in order, a passing check is left alone, an owner step is never applied,
and a job step runs once per registry job for this host.

Breaks this guards against: a recipe that "fixes" what was already fine, an agent typing a
password because a step said apply, or a job missed because its host list said `all`.
"""
import importlib.machinery
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load():
    loader = importlib.machinery.SourceFileLoader("fleet_join", str(ROOT / "bin/fleet-join"))
    spec = importlib.util.spec_from_loader("fleet_join", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


M = load()


class Join(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def recipe(self, steps, **extra):
        path = self.dir / "join.json"
        path.write_text(json.dumps(dict({"host_id": "lap", "steps": steps}, **extra)))
        return str(path)

    def test_check_only_changes_nothing_and_apply_fixes(self):
        flag = self.dir / "flag"
        steps = [{"name": "flag", "check": f"test -e {flag}", "apply": f"touch {flag}"}]
        self.assertEqual(M.main(["--recipe", self.recipe(steps)]), 1)
        self.assertFalse(flag.exists())
        self.assertEqual(M.main(["--recipe", self.recipe(steps), "--apply"]), 0)
        self.assertTrue(flag.exists())

    def test_an_owner_step_is_never_applied(self):
        flag = self.dir / "typed-a-password"
        steps = [{"name": "sign in", "owner": True, "check": "false", "apply": f"touch {flag}", "say": "you"}]
        self.assertEqual(M.main(["--recipe", self.recipe(steps), "--apply"]), 1)
        self.assertFalse(flag.exists())

    def test_job_steps_follow_the_registry_and_host_steps_their_host(self):
        registry = self.dir / "jobs.tsv"
        registry.write_text("# label\thosts\n"
                            "com.example.a\tlap\tx\n"
                            "com.example.b\tall\tx\n"
                            "com.example.c\thub\tx\n"
                            "com.example.d\tall\tRETIRED 1 Nov: replaced\n"
                            "com.example.e\tlap\tPAUSED 2 Nov: waits for a disk\n")
        seen = self.dir / "seen"
        steps = [{"name": "job", "for_each_job": True, "check": f"echo {{label}} >> {seen}; true"},
                 {"name": "hub only", "hosts": ["hub"], "check": "false"}]
        rc = M.main(["--recipe", self.recipe(steps, registry=str(registry), plists_dir=str(self.dir))])
        self.assertEqual(rc, 0)
        self.assertEqual(seen.read_text().split(), ["com.example.a", "com.example.b"])

    def test_as_takes_a_lost_macs_place(self):
        steps = [{"name": "who", "check": "test {host} = old-laptop"}]
        self.assertEqual(M.main(["--recipe", self.recipe(steps), "--as", "old-laptop"]), 0)


if __name__ == "__main__":
    unittest.main()
