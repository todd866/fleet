import importlib.machinery
import importlib.util
import os
import pathlib
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent


def load():
    loader = importlib.machinery.SourceFileLoader("git_stale_locks", str(ROOT / "bin" / "git-stale-locks"))
    spec = importlib.util.spec_from_loader("git_stale_locks", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def lock(root, rel, mtime):
    path = pathlib.Path(root) / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("")
    os.utime(path, (mtime, mtime))
    return path


class StaleLocks(unittest.TestCase):
    def setUp(self):
        self.m = load()
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name
        self.now, self.boot = 100_000.0, 90_000.0

    def tearDown(self):
        self.tmp.cleanup()

    def sweep(self, **kw):
        args = dict(now=self.now, boot=self.boot, git_is_running=True, log=False, dry_run=True)
        args.update(kw)
        return {os.path.relpath(d["path"], self.root): d for d in self.m.sweep([self.root], **args)}

    def test_a_lock_older_than_the_boot_is_stale_even_while_git_runs(self):
        lock(self.root, "repo/.git/index.lock", self.boot - 60)
        lock(self.root, "repo2/.git/index.lock", self.boot + 60)
        found = self.sweep()
        self.assertEqual(list(found), ["repo/.git/index.lock"])
        self.assertEqual(found["repo/.git/index.lock"]["why"], "older than this boot")

    def test_an_old_lock_since_boot_is_stale_only_when_no_git_runs(self):
        lock(self.root, "repo/.git/index.lock", self.now - 50 * 60)        # after the boot, 50 minutes old
        lock(self.root, "repo/.git/HEAD.lock", self.now - 2 * 60)          # two minutes old: could be an IDE mid-write
        self.boot = self.now - 8 * 3600
        self.assertEqual(self.sweep(git_is_running=True), {})
        found = self.sweep(git_is_running=False)
        self.assertEqual(list(found), ["repo/.git/index.lock"])
        self.assertIn("no git process", found["repo/.git/index.lock"]["why"])

    def test_worktree_and_submodule_locks_are_found_and_build_trees_are_skipped(self):
        old = self.boot - 5
        lock(self.root, "repo/.git/worktrees/feature/index.lock", old)
        lock(self.root, "repo/.git/modules/sub/deep/index.lock", old)
        lock(self.root, "repo/node_modules/pkg/.git/index.lock", old)
        lock(self.root, "a/b/nested/.git/index.lock", old)
        found = set(self.sweep())
        self.assertEqual(found, {"repo/.git/worktrees/feature/index.lock", "repo/.git/modules/sub/deep/index.lock",
                                 "a/b/nested/.git/index.lock"})

    def test_a_cleared_lock_is_moved_not_left_and_a_fresh_one_is_untouched(self):
        stale = lock(self.root, "repo/.git/index.lock", self.boot - 60)
        fresh = lock(self.root, "repo2/.git/index.lock", self.now - 5)
        orig_which, orig_exists = self.m.shutil.which, self.m.os.path.exists
        self.m.shutil.which = lambda name: None                                # no `trash`: rename aside
        self.m.os.path.exists = lambda p: False if p == "/usr/bin/trash" else orig_exists(p)
        try:
            done = self.m.sweep([self.root], now=self.now, boot=self.boot, git_is_running=True, log=False)
        finally:
            self.m.shutil.which, self.m.os.path.exists = orig_which, orig_exists   # shutil is shared by every test
        self.assertEqual([d["action"] for d in done], ["renamed aside"])
        self.assertFalse(stale.exists())
        self.assertTrue((stale.parent / f"index.lock.stale-{int(self.now)}").exists())
        self.assertTrue(fresh.exists())

    def test_one_lock_that_cannot_be_moved_does_not_stop_the_sweep(self):
        lock(self.root, "a/.git/index.lock", self.boot - 60)
        lock(self.root, "b/.git/index.lock", self.boot - 60)
        real = self.m.put_away

        def put_away(path, now):
            if "/a/" in path:
                raise PermissionError(1, "Operation not permitted")
            return "moved"

        self.m.put_away = put_away
        try:
            found = self.sweep(dry_run=False)
        finally:
            self.m.put_away = real
        self.assertEqual(found["a/.git/index.lock"]["action"], "failed: Operation not permitted")
        self.assertEqual(found["b/.git/index.lock"]["action"], "moved")


if __name__ == "__main__":
    unittest.main()
