#!/usr/bin/env python3
"""Small fixture tests for package-python-runtime.py (no torch download)."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("package-python-runtime.py")

class PackageRuntimeTests(unittest.TestCase):
    def fixture(self, root):
        prefix = root / "prefix"
        interpreter = prefix / "bin/python3"
        interpreter.parent.mkdir(parents=True)
        interpreter.write_bytes(b"fixture interpreter")
        interpreter.chmod(0o700)
        module = prefix / ("a" * 120) / "module.py"
        module.parent.mkdir()
        module.write_text("fixture")
        return prefix

    def package(self, prefix, output):
        return subprocess.run([sys.executable, SCRIPT, prefix, output, "--target", "fixture-target", "--skip-dependency-check"], text=True, capture_output=True)

    def test_fixture_is_deterministic(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = self.fixture(root)
            first, second = root / "one.tar.gz", root / "two.tar.gz"
            self.assertEqual(self.package(prefix, first).returncode, 0)
            self.assertEqual(self.package(prefix, second).returncode, 0)
            self.assertEqual(first.read_bytes(), second.read_bytes())

    @unittest.skipUnless(hasattr(os, "symlink"), "symlinks unavailable")
    def test_rejects_escape_symlink(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = self.fixture(root)
            (prefix / "bad").symlink_to("/etc/passwd")
            result = self.package(prefix, root / "runtime.tar.gz")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("escapes Python prefix", result.stderr)

if __name__ == "__main__":
    unittest.main()
