from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import textwrap
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "tools" / "bundle" / "install-user"
CONFIG = tomllib.loads((ROOT / "workbench.toml").read_text(encoding="utf-8"))
APP_ID = CONFIG["application"]["app_id"]


class ManagedInstallerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="rust workbench installer ")
        self.root = Path(self.temporary.name)
        self.home = self.root / "home with spaces"
        self.data = self.root / "desktop data"
        self.prefix = self.home / ".local" / "opt" / "rust-workbench"
        self.bin_dir = self.home / ".local" / "bin"
        self.environment = os.environ.copy()
        self.environment.update({"HOME": str(self.home), "XDG_DATA_HOME": str(self.data)})

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def bundle(self, version: str, commit_digit: str, *, include_icon: bool = True) -> Path:
        bundle = self.root / f"source {version}.app"
        (bundle / "bin").mkdir(parents=True)
        (bundle / "share" / "rust-workbench").mkdir(parents=True)
        (bundle / "share" / "applications").mkdir(parents=True)
        icon_dir = bundle / "share" / "icons" / "hicolor" / "512x512" / "apps"
        icon_dir.mkdir(parents=True)
        shutil.copy2(INSTALLER, bundle / "bin" / "install-user")
        launcher = bundle / "bin" / "rust-workbench"
        launcher.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                root="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd -P)"
                case "${1:-}" in
                  --doctor)
                    [[ ! -e "$root/fail-doctor" ]]
                    [[ "$root" != *'.staging-'* || ! -e "$root/fail-staging" ]]
                    ;;
                  --print-bundle-root)
                    printf '%s\n' "$root"
                    ;;
                esac
                """
            ),
            encoding="utf-8",
        )
        launcher.chmod(0o755)
        commit = commit_digit * 40
        (bundle / "share" / "rust-workbench" / "release.env").write_text(
            textwrap.dedent(
                f"""\
                RUST_WORKBENCH_APP_NAME='Rust Workbench'
                RUST_WORKBENCH_APP_ID={APP_ID}
                RUST_WORKBENCH_RELEASE_CHANNEL=dev
                RUST_WORKBENCH_INSTALL_DIRECTORY=rust-workbench
                RUST_WORKBENCH_INSTALL_LAYOUT_VERSION=1
                RUST_WORKBENCH_VERSION={version}
                RUST_WORKBENCH_SOURCE_COMMIT={commit}
                RUST_WORKBENCH_ARCH=x86_64
                RUST_WORKBENCH_RUST_HOST=x86_64-unknown-linux-gnu
                RUST_WORKBENCH_NATIVE_TRIPLE=x86_64-linux-gnu
                RUST_WORKBENCH_GLIBC_MIN=2.43
                RUST_WORKBENCH_GCC_MAJOR=15
                """
            ),
            encoding="utf-8",
        )
        (bundle / "share" / "applications" / f"{APP_ID}.desktop").write_text(
            textwrap.dedent(
                f"""\
                [Desktop Entry]
                Type=Application
                Name=Rust Workbench
                Exec=rust-workbench %F
                Icon={APP_ID}
                StartupWMClass={APP_ID}
                """
            ),
            encoding="utf-8",
        )
        if include_icon:
            (icon_dir / f"{APP_ID}.png").write_bytes(b"not-a-real-png-for-installer-testing")
        return bundle

    def invoke(self, installer: Path, *arguments: object, check: bool = True) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(installer), *map(str, arguments), "--prefix", str(self.prefix), "--bin-dir", str(self.bin_dir)],
            env=self.environment,
            check=check,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def current_payload(self) -> str:
        return os.readlink(self.prefix / "current")

    def test_install_upgrade_rollback_failure_and_uninstall(self) -> None:
        first = self.bundle("1.0.0", "1")
        second = self.bundle("2.0.0", "2")
        broken = self.bundle("3.0.0", "3", include_icon=False)

        self.bin_dir.mkdir(parents=True)
        foreign = self.bin_dir / "rust-workbench"
        foreign.write_text("foreign command\n", encoding="utf-8")
        refused = self.invoke(first / "bin" / "install-user", "install", check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(foreign.read_text(encoding="utf-8"), "foreign command\n")
        foreign.unlink()

        self.invoke(first / "bin" / "install-user", "install")
        self.assertEqual(self.current_payload(), "versions/1.0.0-111111111111")
        self.assertTrue((self.data / "applications" / f"{APP_ID}.desktop").is_file())

        self.invoke(second / "bin" / "install-user", "install")
        self.assertEqual(self.current_payload(), "versions/2.0.0-222222222222")
        self.assertEqual(os.readlink(self.prefix / "previous"), "versions/1.0.0-111111111111")
        self.assertEqual(len(list((self.prefix / "versions").iterdir())), 2)

        self.invoke(self.bin_dir / "rust-workbench-uninstall", "rollback")
        self.assertEqual(self.current_payload(), "versions/1.0.0-111111111111")
        self.assertEqual(os.readlink(self.prefix / "previous"), "versions/2.0.0-222222222222")

        failed = self.invoke(broken / "bin" / "install-user", "install", check=False)
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual(self.current_payload(), "versions/1.0.0-111111111111")
        self.assertFalse((self.prefix / "versions" / "3.0.0-333333333333").exists())

        preserved = self.data / "rust-workbench" / "user-file"
        preserved.write_text("keep\n", encoding="utf-8")
        self.invoke(self.bin_dir / "rust-workbench-uninstall", "uninstall")
        self.assertFalse(self.prefix.exists())
        self.assertFalse((self.bin_dir / "rust-workbench").exists())
        self.assertTrue(preserved.is_file())

        self.invoke(first / "bin" / "install-user", "install")
        self.invoke(self.bin_dir / "rust-workbench-uninstall", "uninstall", "--purge-data")
        self.assertFalse((self.data / "rust-workbench").exists())

    def test_failed_first_install_leaves_no_dangling_integration(self) -> None:
        broken = self.bundle("3.0.0", "3", include_icon=False)
        failed = self.invoke(broken / "bin" / "install-user", "install", check=False)
        self.assertNotEqual(failed.returncode, 0)
        self.assertFalse((self.prefix / "current").exists())
        self.assertFalse((self.prefix / "versions" / "3.0.0-333333333333").exists())
        self.assertFalse((self.bin_dir / "rust-workbench").exists())
        self.assertFalse((self.bin_dir / "rust-workbench-uninstall").exists())
        self.assertFalse((self.data / "applications" / f"{APP_ID}.desktop").exists())
        self.assertFalse((self.data / "icons" / "hicolor" / "512x512" / "apps" / f"{APP_ID}.png").exists())

    def test_invalid_purge_marker_does_not_remove_application(self) -> None:
        first = self.bundle("1.0.0", "1")
        self.invoke(first / "bin" / "install-user", "install")
        marker = self.data / "rust-workbench" / ".rust-workbench-managed-data"
        marker.write_text("not-this-application\n", encoding="utf-8")

        refused = self.invoke(
            self.bin_dir / "rust-workbench-uninstall", "uninstall", "--purge-data", check=False
        )
        self.assertNotEqual(refused.returncode, 0)
        self.assertTrue((self.prefix / "current").is_symlink())
        self.assertTrue((self.bin_dir / "rust-workbench").is_symlink())


if __name__ == "__main__":
    unittest.main()
