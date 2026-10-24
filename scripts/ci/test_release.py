"""Exercise release packaging against disposable repositories and build outputs."""

import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile


SCRIPT = Path(__file__).with_name("release.py")
ASSETS = (
    "codeg-relay-x86_64-unknown-linux-musl",
    "codeg-relay-aarch64-unknown-linux-musl",
    "codeg-relay-deploy.tar.gz",
    "codeg-relay-agent-x86_64-pc-windows-msvc.zip",
    "codeg-relay-agent-aarch64-apple-darwin.tar.gz",
    "codeg-relay-agent-x86_64-apple-darwin.tar.gz",
    "codeg-relay-agent-x86_64-unknown-linux-musl.tar.gz",
    "codeg-relay-agent-aarch64-unknown-linux-musl.tar.gz",
)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="release-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo with spaces"
        self.target = self.root / "fake target"
        self.output = self.root / "dist"
        self.repo.mkdir()
        (self.repo / "LICENSE").write_text("Example license\n", encoding="utf-8")
        (self.repo / "NOTICE").write_text("Example notice\n", encoding="utf-8")

    def run_cli(self, *arguments, success=True):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), *arguments, "--output-dir", str(self.output)],
            capture_output=True,
            text=True,
            check=False,
        )
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def binary(self, target, name, data=b"fake executable\n"):
        path = self.target / target / "release" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def package(self, component, target=None, success=True):
        arguments = [
            "package", component, "--repo-root", str(self.repo),
            "--target-dir", str(self.target),
        ]
        if target:
            arguments.extend(["--target", target])
        return self.run_cli(*arguments, success=success)

    def check_sidecar(self, name):
        asset = self.output / name
        expected = f"{hashlib.sha256(asset.read_bytes()).hexdigest()}  {name}\n"
        self.assertEqual((self.output / f"{name}.sha256").read_bytes(), expected.encode())

    def seed_assets(self):
        self.output.mkdir()
        for name in ASSETS:
            data = f"fixture for {name}\n".encode()
            (self.output / name).write_bytes(data)
            (self.output / f"{name}.sha256").write_bytes(
                f"{hashlib.sha256(data).hexdigest()}  {name}\n".encode()
            )

    def test_windows_zip_collects_all_top_level_executables(self):
        target = "x86_64-pc-windows-msvc"
        self.binary(target, "codeg-relay-agent.exe", b"console")
        self.binary(target, "future-launcher.exe", b"gui")
        self.binary(target, "unrelated.pdb")
        self.binary(target, "deps/test-harness.exe")
        self.package("agent", target)
        name = "codeg-relay-agent-x86_64-pc-windows-msvc.zip"
        with zipfile.ZipFile(self.output / name) as archive:
            self.assertEqual(set(archive.namelist()), {
                "codeg-relay-agent.exe", "future-launcher.exe", "LICENSE", "NOTICE",
            })
            self.assertEqual(archive.read("future-launcher.exe"), b"gui")
            self.assertEqual(archive.read("NOTICE"), b"Example notice\n")
        self.check_sidecar(name)

    def test_unix_archives_preserve_runnable_agent_and_notices(self):
        for target in (
            "aarch64-apple-darwin", "x86_64-apple-darwin",
            "x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
        ):
            with self.subTest(target=target):
                self.binary(target, "codeg-relay-agent", b"unix agent")
                self.package("agent", target)
                name = f"codeg-relay-agent-{target}.tar.gz"
                with tarfile.open(self.output / name) as archive:
                    self.assertEqual(set(archive.getnames()), {
                        "codeg-relay-agent", "LICENSE", "NOTICE",
                    })
                    self.assertEqual(archive.extractfile("codeg-relay-agent").read(), b"unix agent")
                    self.assertEqual(archive.getmember("codeg-relay-agent").mode, 0o755)
                self.check_sidecar(name)

    def test_relay_assets_are_raw_executables(self):
        for target in ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"):
            with self.subTest(target=target):
                self.binary(target, "codeg-relay", b"relay")
                self.package("relay", target)
                name = f"codeg-relay-{target}"
                self.assertEqual((self.output / name).read_bytes(), b"relay")
                if os.name != "nt":
                    self.assertEqual((self.output / name).stat().st_mode & 0o777, 0o755)
                self.check_sidecar(name)

    def test_deploy_bundle_keeps_adjacent_files_and_nested_content(self):
        deploy = self.repo / "crates/codeg-relay/deploy"
        (deploy / "nested").mkdir(parents=True)
        for name in ("install.sh", "uninstall.sh", "codeg-relay.service", "nested/README.md"):
            (deploy / name).write_text(name, encoding="utf-8")
        self.package("deploy")
        with tarfile.open(self.output / "codeg-relay-deploy.tar.gz") as archive:
            self.assertEqual({item.name for item in archive if item.isfile()}, {
                "deploy/install.sh", "deploy/uninstall.sh", "deploy/codeg-relay.service",
                "deploy/nested/README.md", "LICENSE", "NOTICE",
            })
        self.check_sidecar("codeg-relay-deploy.tar.gz")

    def test_missing_windows_binaries_fail_before_creating_archive(self):
        result = self.package("agent", "x86_64-pc-windows-msvc", success=False)
        self.assertIn("No .exe files", result.stderr)
        self.assertFalse(list(self.output.glob("*.zip")))

    def test_missing_unix_binary_is_an_error(self):
        result = self.package("agent", "aarch64-apple-darwin", success=False)
        self.assertIn("codeg-relay-agent", result.stderr)

    def test_missing_notice_is_an_error(self):
        (self.repo / "NOTICE").unlink()
        self.binary("x86_64-pc-windows-msvc", "console.exe")
        result = self.package("agent", "x86_64-pc-windows-msvc", success=False)
        self.assertIn("NOTICE", result.stderr)
        self.assertFalse(list(self.output.glob("*.zip")))

    def test_unsupported_relay_target_is_rejected(self):
        result = self.package("relay", "x86_64-apple-darwin", success=False)
        self.assertIn("Unsupported", result.stderr)

    def test_sha256sums_covers_every_asset_and_sidecar(self):
        self.seed_assets()
        self.run_cli("checksums")
        lines = (self.output / "SHA256SUMS").read_text().splitlines()
        self.assertEqual(len(lines), 16)
        self.assertEqual({line.split("  ", 1)[1] for line in lines},
                         set(ASSETS) | {f"{name}.sha256" for name in ASSETS})
        for line in lines:
            digest, name = line.split("  ", 1)
            self.assertEqual(digest, hashlib.sha256((self.output / name).read_bytes()).hexdigest())
        before = (self.output / "SHA256SUMS").read_bytes()
        self.run_cli("checksums")
        self.assertEqual((self.output / "SHA256SUMS").read_bytes(), before)

    def test_incomplete_release_is_rejected(self):
        self.seed_assets()
        (self.output / ASSETS[0]).unlink()
        result = self.run_cli("checksums", success=False)
        self.assertIn("Missing", result.stderr)
        self.assertFalse((self.output / "SHA256SUMS").exists())

    def test_unexpected_release_file_is_rejected(self):
        self.seed_assets()
        (self.output / "debug.pdb").write_bytes(b"debug")
        result = self.run_cli("checksums", success=False)
        self.assertIn("Unexpected", result.stderr)

    def test_corrupt_asset_or_wrong_sidecar_filename_is_rejected(self):
        self.seed_assets()
        asset = self.output / ASSETS[0]
        original = asset.read_bytes()
        asset.write_bytes(b"corrupt")
        result = self.run_cli("checksums", success=False)
        self.assertIn("Checksum mismatch", result.stderr)
        asset.write_bytes(original)
        (self.output / f"{ASSETS[0]}.sha256").write_text(
            f"{hashlib.sha256(original).hexdigest()}  wrong-name\n", encoding="utf-8"
        )
        result = self.run_cli("checksums", success=False)
        self.assertIn("Checksum mismatch", result.stderr)


if __name__ == "__main__":
    unittest.main()
