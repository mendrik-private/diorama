#!/usr/bin/env python3
"""Offline regression tests for Diorama's first-use FLUX provisioning."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("setup_line_art", Path(__file__).with_name("setup-line-art.py"))
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class ProvisioningTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.model = self.root / "model with spaces"
        self.gguf = self.root / "custom transformer.gguf"
        self.weights = b"verified test transformer"
        self.enterContext(patch.object(setup, "GGUF_SIZE", len(self.weights)))
        self.enterContext(patch.object(setup, "GGUF_SHA256", hashlib.sha256(self.weights).hexdigest()))

    def pipeline_download(self, repository, revision, destination, patterns, **kwargs):
        self.assertEqual(repository, setup.REPOSITORY)
        self.assertEqual(revision, setup.REVISION)
        self.assertEqual(patterns, setup.ALLOW_PATTERNS)
        self.assertFalse((destination / setup.REVISION_MARKER).exists())
        (destination / "model_index.json").write_text("{}")

    def gguf_download(self, repository, revision, destination, patterns, **kwargs):
        self.assertEqual(repository, setup.GGUF_REPOSITORY)
        self.assertEqual(revision, setup.GGUF_REVISION)
        self.assertEqual(patterns, [setup.GGUF_FILE])
        (destination / setup.GGUF_FILE).write_bytes(self.weights)

    def test_missing_pipeline_downloads_pinned_revision_then_reuses_it_offline(self):
        with patch.object(setup, "download", side_effect=self.pipeline_download) as download:
            setup.ensure_model(self.model)
            setup.ensure_model(self.model)
        download.assert_called_once()
        self.assertTrue(setup.model_complete(self.model))

    def test_incomplete_pipeline_retries_without_a_stale_success_marker(self):
        self.model.mkdir()
        marker = self.model / setup.REVISION_MARKER
        marker.write_text(setup.REVISION)
        with patch.object(setup, "download", side_effect=OSError("network unavailable")):
            with self.assertRaises(OSError):
                setup.ensure_model(self.model)
        self.assertFalse(marker.exists())
        with patch.object(setup, "download", side_effect=self.pipeline_download):
            setup.ensure_model(self.model)
        self.assertTrue(setup.model_complete(self.model))

    def test_success_without_pipeline_files_is_rejected(self):
        with patch.object(setup, "download"):
            with self.assertRaisesRegex(SystemExit, "without model_index.json"):
                setup.ensure_model(self.model)
        self.assertFalse((self.model / setup.REVISION_MARKER).exists())

    def test_missing_and_truncated_gguf_download_verify_and_reuse_custom_path(self):
        for initial in (None, b"partial"):
            with self.subTest(initial=initial):
                self.gguf.unlink(missing_ok=True)
                if initial:
                    self.gguf.write_bytes(initial)
                with patch.object(setup, "download", side_effect=self.gguf_download) as download:
                    setup.ensure_gguf(self.gguf)
                    setup.ensure_gguf(self.gguf)
                download.assert_called_once()
                self.assertTrue(setup.gguf_complete(self.gguf))

    def test_same_size_corruption_forces_a_fresh_download(self):
        self.gguf.write_bytes(b"x" * len(self.weights))
        with patch.object(setup, "download", side_effect=self.gguf_download) as download:
            setup.ensure_gguf(self.gguf)
        self.assertTrue(download.call_args.kwargs["force"])
        self.assertTrue(setup.gguf_complete(self.gguf))

    def test_failed_hash_never_creates_a_verified_marker(self):
        self.gguf.write_bytes(b"x" * len(self.weights))
        with patch.object(setup, "download", side_effect=lambda *a, **kw: (self.root / setup.GGUF_FILE).write_bytes(b"bad")):
            with self.assertRaisesRegex(SystemExit, "not the pinned"):
                setup.ensure_gguf(self.gguf)
        self.assertFalse(self.gguf.with_name(setup.GGUF_MARKER).exists())

    def test_models_only_does_not_install_packages_or_record_runtime(self):
        with patch.object(setup, "ensure_environment") as environment, \
             patch.object(setup, "ensure_model") as model, \
             patch.object(setup, "ensure_gguf") as gguf, \
             patch.object(setup, "write_atomically") as write:
            setup.main(["--models-only", "--model", str(self.model), "--gguf", str(self.gguf)])
        environment.assert_not_called()
        write.assert_not_called()
        model.assert_called_once_with(self.model)
        gguf.assert_called_once_with(self.gguf)

    def test_environment_only_records_pins_without_downloading_models(self):
        python = self.root / "venv/bin/python"
        config = self.root / "runtime.conf"
        with patch.object(setup, "ensure_environment", return_value=(python, {})), \
             patch.object(setup, "report_torch"), \
             patch.object(setup, "download") as download:
            setup.main(["--environment-only", "--runtime-config", str(config)])
        download.assert_not_called()
        self.assertIn(f"python={python}\n", config.read_text())

    def test_download_errors_explain_access_or_retry_without_marking_success(self):
        class HTTPError(Exception):
            pass

        class HubError(HTTPError):
            def __init__(self, status):
                self.response = SimpleNamespace(status_code=status)

        class GatedError(HubError):
            pass

        for error, expected in [
            (GatedError(403), "auth login"),
            (HubError(401), "auth login"),
            (HubError(503), "retry in Diorama"),
            (OSError("disk full"), "disk space"),
        ]:
            with self.subTest(error=error):
                snapshot = Mock(side_effect=error)
                modules = {
                    "httpx": SimpleNamespace(HTTPError=HTTPError),
                    "huggingface_hub": SimpleNamespace(snapshot_download=snapshot),
                    "huggingface_hub.errors": SimpleNamespace(GatedRepoError=GatedError, HfHubHTTPError=HubError),
                }
                with patch.dict(sys.modules, modules):
                    with self.assertRaisesRegex(SystemExit, expected):
                        setup.ensure_model(self.model)
                snapshot.assert_called_once_with(
                    setup.REPOSITORY, revision=setup.REVISION, local_dir=str(self.model),
                    allow_patterns=setup.ALLOW_PATTERNS, force_download=False,
                )
                self.assertFalse((self.model / setup.REVISION_MARKER).exists())


if __name__ == "__main__":
    unittest.main()
