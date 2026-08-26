#!/usr/bin/env python3
"""Unified build, test, run, package, and cleanup entry point for Rust Workbench."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import time
import tomllib
from typing import Iterable, Sequence


ROOT = Path(__file__).resolve().parents[1]
CONFIG_PATH = ROOT / "workbench.toml"
CONFIG = tomllib.loads(CONFIG_PATH.read_text(encoding="utf-8"))


def command_text(command: Sequence[object]) -> str:
    return shlex.join(str(part) for part in command)


def run(
    command: Sequence[object],
    *,
    cwd: Path = ROOT,
    env: dict[str, str] | None = None,
    check: bool = True,
    capture: bool = False,
    verbose: bool = True,
) -> subprocess.CompletedProcess[str]:
    rendered = [str(part) for part in command]
    if verbose:
        print(f"+ ({cwd}) {command_text(rendered)}", flush=True)
    return subprocess.run(
        rendered,
        cwd=cwd,
        env=env,
        check=check,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
    )


def output(command: Sequence[object], *, cwd: Path = ROOT, env: dict[str, str] | None = None) -> str:
    completed = run(command, cwd=cwd, env=env, capture=True)
    return completed.stdout.strip()


def workspace_path(key: str) -> Path:
    return ROOT / CONFIG["workspace"][key]


RUST = workspace_path("rust")
ZED = workspace_path("zed")
ANALYZER = workspace_path("analyzer")
STRESS_PROJECT = workspace_path("stress_project")
HOST = "x86_64-unknown-linux-gnu"
STAGE1 = RUST / "build" / HOST / "stage1"
STAGE0 = RUST / "build" / HOST / "stage0"
RUSTC = STAGE1 / "bin" / "rustc"
RUSTDOC = STAGE1 / "bin" / "rustdoc"
CARGO = STAGE0 / "bin" / "cargo"
ANALYZER_BINARY = ANALYZER / "target" / "release" / "rust-analyzer"
EDITOR_BINARY = ZED / "target" / "release" / "rust-workbench"
PROC_MACRO_SERVER = STAGE1 / "libexec" / "rust-analyzer-proc-macro-srv"
STAGE1_TARGET_LIB = STAGE1 / "lib" / "rustlib" / HOST / "lib"
BUNDLE_TEMPLATES = ROOT / "tools" / "bundle"
DIST = ROOT / "dist"
BUILD_RECEIPT = ZED / ".build-deps" / "rust-workbench-build-receipt.json"
RELEASE_VERSION = tomllib.loads(
    (ZED / "crates" / "rust_workbench" / "Cargo.toml").read_text(encoding="utf-8")
)["package"]["version"]

CORE_GLIBC_LIBRARIES = {
    "ld-linux-x86-64.so.2",
    "libanl.so.1",
    "libBrokenLocale.so.1",
    "libc.so.6",
    "libcrypt.so.1",
    "libdl.so.2",
    "libm.so.6",
    "libmemusage.so",
    "libnsl.so.1",
    "libnss_compat.so.2",
    "libnss_dns.so.2",
    "libnss_files.so.2",
    "libnss_hesiod.so.2",
    "libpcprofile.so",
    "libpthread.so.0",
    "libresolv.so.2",
    "librt.so.1",
    "libthread_db.so.1",
    "libutil.so.1",
}


def human_size(byte_count: int) -> str:
    value = float(byte_count)
    for suffix in ("B", "KiB", "MiB", "GiB", "TiB"):
        if value < 1024.0 or suffix == "TiB":
            return f"{value:.1f} {suffix}"
        value /= 1024.0
    raise AssertionError("unreachable")


def directory_bytes(path: Path) -> int:
    if not path.exists():
        return 0
    completed = run(["du", "-sb", path], capture=True)
    return int(completed.stdout.split()[0])


def first_line(command: Sequence[object], *, env: dict[str, str] | None = None) -> str:
    completed = run(command, env=env, capture=True, check=False)
    combined = completed.stdout or completed.stderr
    return combined.splitlines()[0] if combined else "unavailable"


def current_glibc() -> str | None:
    completed = run(["getconf", "GNU_LIBC_VERSION"], capture=True, check=False)
    match = re.search(r"glibc\s+([0-9.]+)", completed.stdout)
    return match.group(1) if match else None


def version_tuple(value: str) -> tuple[int, ...]:
    return tuple(int(part) for part in value.split("."))


def source_commit() -> str:
    return output(["git", "rev-parse", "HEAD"])


def source_epoch() -> int:
    configured = os.environ.get("SOURCE_DATE_EPOCH")
    value = configured or output(["git", "show", "-s", "--format=%ct", "HEAD"])
    try:
        epoch = int(value)
    except ValueError as error:
        raise RuntimeError(f"invalid SOURCE_DATE_EPOCH: {value!r}") from error
    if epoch < 0:
        raise RuntimeError(f"SOURCE_DATE_EPOCH must be non-negative: {epoch}")
    return epoch


def source_changes() -> str:
    return output(["git", "status", "--porcelain=v1", "--untracked-files=all"])


def require_clean_source() -> None:
    changes = source_changes()
    if changes:
        raise SystemExit(
            "packaging requires a clean, committed source tree; commit or stash these paths:\n"
            + changes
        )


def record_build(component: str, artifacts: Sequence[Path], **details: object) -> None:
    commit = source_commit()
    dirty = bool(source_changes())
    receipt: dict[str, object] = {}
    if BUILD_RECEIPT.is_file():
        try:
            loaded = json.loads(BUILD_RECEIPT.read_text(encoding="utf-8"))
            if (
                isinstance(loaded, dict)
                and loaded.get("source_commit") == commit
                and loaded.get("source_dirty") == dirty
            ):
                receipt = loaded
        except (OSError, json.JSONDecodeError):
            pass
    components = receipt.setdefault("components", {})
    if not isinstance(components, dict):
        components = {}
        receipt["components"] = components
    artifact_records = []
    for artifact in artifacts:
        if not artifact.is_file():
            raise RuntimeError(f"build did not produce required artifact: {artifact}")
        artifact_records.append(
            {"path": str(artifact.relative_to(ROOT)), "sha256": sha256(artifact)}
        )
    components[component] = {"artifacts": artifact_records, **details}
    receipt.update(
        {
            "format_version": 1,
            "source_commit": commit,
            "source_dirty": dirty,
            "built_at_epoch": int(time.time()),
        }
    )
    BUILD_RECEIPT.parent.mkdir(parents=True, exist_ok=True)
    BUILD_RECEIPT.write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def validate_build_receipt() -> None:
    try:
        receipt = json.loads(BUILD_RECEIPT.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(
            "missing or invalid release build receipt; rebuild without --skip-build"
        ) from error
    if receipt.get("source_commit") != source_commit() or receipt.get("source_dirty"):
        raise SystemExit(
            "release artifacts were not built from the current clean commit; "
            "rebuild without --skip-build"
        )
    components = receipt.get("components")
    if not isinstance(components, dict):
        raise SystemExit("release build receipt has no component records")
    for component in ("compiler", "editor"):
        record = components.get(component)
        if not isinstance(record, dict):
            raise SystemExit(f"release build receipt is missing {component}")
        if component == "editor" and record.get("portable") is not True:
            raise SystemExit("release editor receipt is not marked portable")
        artifacts = record.get("artifacts")
        if not isinstance(artifacts, list) or not artifacts:
            raise SystemExit(f"release build receipt has no {component} artifacts")
        for artifact in artifacts:
            if not isinstance(artifact, dict):
                raise SystemExit(f"invalid {component} artifact receipt")
            path = (ROOT / str(artifact.get("path", ""))).resolve()
            if path == ROOT or ROOT not in path.parents:
                raise SystemExit(f"unsafe {component} artifact path in build receipt: {path}")
            expected = artifact.get("sha256")
            if not path.is_file() or not isinstance(expected, str) or sha256(path) != expected:
                raise SystemExit(
                    f"stale or modified {component} artifact: {path}; "
                    "rebuild without --skip-build"
                )


def doctor() -> int:
    print("Rust Workbench workspace doctor")
    print(f"root: {ROOT}")
    print(f"git: {first_line(['git', 'describe', '--always', '--dirty'])}")
    print(f"platform: {platform.system()} {platform.machine()}")
    glibc = current_glibc()
    required_glibc = CONFIG["compatibility"]["glibc_min"]
    print(f"glibc: {glibc or 'not found'} (bundle target >= {required_glibc})")

    failed = False
    required_directories = (RUST, ZED, ANALYZER, STRESS_PROJECT)
    for path in required_directories:
        present = path.is_dir()
        print(f"{'OK' if present else 'MISSING'} source: {path.relative_to(ROOT)}")
        failed |= not present

    commands = ("python3", "cargo", "readelf", "tar", "zstd", "dpkg-query")
    for command in commands:
        resolved = shutil.which(command)
        print(f"{'OK' if resolved else 'MISSING'} command: {command}{f' -> {resolved}' if resolved else ''}")
        failed |= resolved is None

    artifacts = (
        ("custom rustc", RUSTC, ["-Vv"]),
        ("custom rustdoc", RUSTDOC, ["-V"]),
        ("bundled Cargo source", CARGO, ["-V"]),
        ("custom rust-analyzer", ANALYZER_BINARY, ["--version"]),
        ("stage1 proc-macro server", PROC_MACRO_SERVER, []),
        ("Rust Workbench editor", EDITOR_BINARY, []),
    )
    for label, path, arguments in artifacts:
        if not path.is_file():
            print(f"NOT BUILT artifact: {label} ({path.relative_to(ROOT)})")
            # A partially-created stage1 sysroot is worse than a clearly
            # unbuilt tree: the editor can start while rustdoc or proc-macro
            # expansion fails later. Treat missing siblings as a doctor error
            # once stage1 rustc itself exists.
            if RUSTC.is_file():
                failed = True
            continue
        size = human_size(path.stat().st_size)
        version = first_line([path, *arguments]) if arguments else "present"
        print(f"OK artifact: {label}: {version} ({size})")

    stage1_std = sorted(STAGE1_TARGET_LIB.glob("libstd-*.rlib"))
    if stage1_std:
        print(f"OK artifact: stage1 standard library: {stage1_std[0].name}")
    else:
        print(
            "NOT BUILT artifact: stage1 standard library "
            f"({STAGE1_TARGET_LIB.relative_to(ROOT)}/libstd-*.rlib)"
        )
        if RUSTC.is_file():
            failed = True

    if EDITOR_BINARY.is_file():
        dynamic = run(["readelf", "-d", EDITOR_BINARY], capture=True, check=False).stdout
        paths = [line.strip() for line in dynamic.splitlines() if "RPATH" in line or "RUNPATH" in line]
        for line in paths or ["no RPATH/RUNPATH"]:
            print(f"editor linkage: {line}")

    if glibc and version_tuple(glibc) < version_tuple(required_glibc):
        failed = True
    return 1 if failed else 0


def bootstrap_native() -> None:
    run([ZED / "script" / "bootstrap-rust-workbench-linux"], cwd=ZED)


def bootstrap() -> None:
    bootstrap_native()


def build_compiler() -> None:
    # rust-analyzer selects its proc-macro server from the active rustc sysroot.
    # Ask bootstrap for every promised stage1 tool in one invocation: separate
    # invocations recreate the sysroot and can prune the tool built just before
    # them. The analyzer target supplies the ABI-matched proc-macro server.
    run(
        [
            RUST / "x",
            "build",
            "--stage",
            "1",
            "library/std",
            "compiler/rustc",
            "src/tools/rust-analyzer",
            "src/tools/rustdoc",
        ],
        cwd=RUST,
    )
    standard_libraries = sorted(STAGE1_TARGET_LIB.glob("libstd-*.rlib"))
    if not standard_libraries:
        raise RuntimeError(f"compiler build did not produce {STAGE1_TARGET_LIB}/libstd-*.rlib")
    record_build(
        "compiler",
        [RUSTC, RUSTDOC, CARGO, PROC_MACRO_SERVER, standard_libraries[0]],
    )


def build_analyzer() -> None:
    cargo = shutil.which("cargo")
    if cargo is None:
        raise SystemExit("cargo is required to build rust-analyzer")
    run(
        [cargo, "build", "--manifest-path", ANALYZER / "Cargo.toml", "-p", "rust-analyzer", "--release"],
        cwd=ANALYZER,
    )
    record_build("analyzer", [ANALYZER_BINARY])


def portable_build_environment() -> dict[str, str]:
    environment = os.environ.copy()
    environment.pop("ZED_MINIDUMP_ENDPOINT", None)
    environment["RUST_WORKBENCH_PORTABLE"] = "1"
    environment["ZED_BUNDLE"] = "true"
    return environment


def build_editor(*, portable: bool = False) -> None:
    environment = portable_build_environment() if portable else os.environ.copy()
    run([ZED / "script" / "build-rust-workbench"], cwd=ZED, env=environment)
    record_build("analyzer", [ANALYZER_BINARY])
    record_build("editor", [EDITOR_BINARY, ANALYZER_BINARY], portable=portable)


def build(component: str, *, portable: bool = False) -> None:
    if component == "compiler":
        build_compiler()
    elif component == "analyzer":
        build_analyzer()
    elif component == "editor":
        build_editor(portable=portable)
    elif component == "all":
        build_compiler()
        build_editor(portable=portable)
    else:
        raise SystemExit(f"unknown component: {component}")


def test_quick_or_full(mode: str) -> None:
    environment = portable_build_environment() if mode == "full" else os.environ.copy()
    run(
        [ZED / "script" / "qualify-rust-workbench", f"--{mode}"],
        cwd=ZED,
        env=environment,
    )


def linux_cpu_totals() -> tuple[int, int] | None:
    try:
        fields = Path("/proc/stat").read_text(encoding="ascii").splitlines()[0].split()
        if fields[0] != "cpu" or len(fields) < 6:
            return None
        values = [int(value) for value in fields[1:]]
    except (OSError, ValueError, IndexError):
        return None
    idle = values[3] + (values[4] if len(values) > 4 else 0)
    return sum(values), idle


def ensure_performance_host_idle() -> None:
    before = linux_cpu_totals()
    if before is None:
        return
    time.sleep(2)
    after = linux_cpu_totals()
    if after is None:
        return
    total_delta = after[0] - before[0]
    idle_delta = after[1] - before[1]
    idle_percent = idle_delta / max(total_delta, 1) * 100.0
    print(f"Performance preflight: {idle_percent:.1f}% CPU idle", flush=True)
    if idle_percent < 50.0:
        raise SystemExit(
            "performance qualification requires at least 50% idle CPU before it starts; "
            "stop unrelated workloads and rerun without changing the baseline"
        )


def test_performance() -> None:
    ensure_performance_host_idle()
    cargo = CARGO if CARGO.is_file() else Path(shutil.which("cargo") or "cargo")
    for suite in range(1, 4):
        print(f"Compiler transport performance suite {suite}/3", flush=True)
        run(
            [
                ZED / "script" / "benchmark-rust-ownership-check",
                "--cargo",
                cargo,
                "--rustc",
                RUSTC,
                "--analyzer",
                ANALYZER_BINARY,
            ],
            cwd=ZED,
        )
    run(
        [
            ZED / "script" / "benchmark-rust-learning-context",
            "--analyzer",
            ANALYZER_BINARY,
            "--project",
            STRESS_PROJECT,
            "--runs",
            "100",
            "--shutdown-timeout",
            "60",
        ],
        cwd=ZED,
    )


def host_cargo() -> Path:
    cargo = shutil.which("cargo")
    if cargo is None:
        raise SystemExit("cargo is required to run Rust Workbench tests")
    return Path(cargo)


def test_resilience() -> None:
    cargo = host_cargo()
    run(
        [
            cargo,
            "test",
            "--manifest-path",
            ZED / "Cargo.toml",
            "-p",
            "project",
            "ownership_protocol_tests",
            "--lib",
        ],
        cwd=ZED,
    )
    run(
        [cargo, "test", "--manifest-path", ZED / "Cargo.toml", "-p", "rust_workbench", "--lib"],
        cwd=ZED,
    )


def test_ui() -> None:
    cargo = host_cargo()
    run(
        [
            cargo,
            "test",
            "--manifest-path",
            ZED / "Cargo.toml",
            "-p",
            "rust_workbench",
            "compact_scene_and_issue_navigation_meet_ui_latency_budget",
            "--lib",
            "--",
            "--nocapture",
        ],
        cwd=ZED,
    )


def test_multi_instance() -> None:
    run([ZED / "script" / "test-rust-workbench-instances"], cwd=ZED)
    cargo = host_cargo()
    run(
        [
            cargo,
            "test",
            "--manifest-path",
            ANALYZER / "Cargo.toml",
            "-p",
            "rust-analyzer",
            "--lib",
            "flycheck::tests::ownership_model_cache_is_stable_and_workspace_scoped",
        ],
        cwd=ANALYZER,
    )


def run_editor(
    paths: list[str],
    *,
    debug: bool = False,
    instance: str | None = None,
    new_instance: bool = False,
) -> None:
    arguments: list[object] = [ZED / "script" / "rust-workbench"]
    if debug:
        arguments.append("--debug")
    if instance is not None:
        arguments.extend(("--instance", instance))
    if new_instance:
        arguments.append("--new-instance")
    arguments.extend(paths or [str(STRESS_PROJECT)])
    run(arguments, cwd=ZED)


def disk_report() -> None:
    paths = [
        RUST,
        RUST / "build",
        ANALYZER / "target" / "debug",
        ANALYZER / "target" / "release",
        ZED,
        ZED / "target" / "debug",
        ZED / "target" / "release",
        ZED / "build",
        ZED / "rust-workbench-data",
        ZED / "rust-workbench-analysis-target",
        DIST,
    ]
    print("Disk usage")
    for path in paths:
        if path.exists():
            print(f"{human_size(directory_bytes(path)):>12}  {path.relative_to(ROOT)}")
    run(["df", "-h", ROOT])

    completed = run(["ps", "-eo", "pid=,rss=,comm=,args="], capture=True)
    total = 0
    rows = []
    for line in completed.stdout.splitlines():
        if "rust-workbench" not in line and "rust-analyzer" not in line:
            continue
        parts = line.split(None, 3)
        if len(parts) < 3:
            continue
        rss_kib = int(parts[1])
        total += rss_kib
        rows.append((parts[0], rss_kib, parts[2], parts[3] if len(parts) == 4 else ""))
    print("Live Rust Workbench processes")
    for pid, rss_kib, command, arguments in rows:
        print(f"{pid:>8} {human_size(rss_kib * 1024):>12} {command} {arguments[:100]}")
    print(f"combined RSS: {human_size(total * 1024)}")


def diagnostic_data_root(override: Path | None) -> Path:
    if override is not None:
        return override.expanduser().resolve()
    configured = os.environ.get("RUST_WORKBENCH_DATA_DIR")
    if configured:
        return Path(configured).expanduser().resolve()
    return (ZED / "rust-workbench-data").resolve()


def diagnostic_files(data_root: Path, *, include_minidumps: bool) -> list[Path]:
    if not data_root.is_dir():
        raise SystemExit(f"Rust Workbench data directory does not exist: {data_root}")
    result: list[Path] = []
    for path in data_root.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        relative_parts = {part.lower() for part in path.relative_to(data_root).parts}
        is_log = path.suffix.lower() == ".log" or path.name.endswith(".log.old")
        is_crash = bool(relative_parts & {"crashes", "logs"})
        is_minidump = path.suffix.lower() in {".dmp", ".mdmp"}
        if is_minidump and not include_minidumps:
            continue
        if is_log or is_crash:
            result.append(path)
    return sorted(result, key=lambda path: path.as_posix())[:200]


def redact_diagnostic_text(value: str) -> str:
    replacements = ((str(ROOT), "<workspace>"), (str(Path.home()), "<home>"))
    for original, replacement in replacements:
        if original and original != "/":
            value = value.replace(original, replacement)
    value = re.sub(
        r"(?i)(authorization|api[_-]?key|token|secret)(\s*[:=]\s*)([^\s,;]+)",
        r"\1\2<redacted>",
        value,
    )
    return value


def collect_diagnostics(
    *, data_root: Path | None, include_minidumps: bool, output_path: Path | None
) -> Path:
    source = diagnostic_data_root(data_root)
    files = diagnostic_files(source, include_minidumps=include_minidumps)
    DIST.mkdir(parents=True, exist_ok=True)
    timestamp = dt.datetime.now(dt.UTC).strftime("%Y%m%dT%H%M%SZ")
    destination = (output_path or DIST / f"rust-workbench-diagnostics-{timestamp}.tar.gz").resolve()
    maximum_file_bytes = 10 * 1024 * 1024
    maximum_total_bytes = 100 * 1024 * 1024
    copied_bytes = 0
    with tempfile.TemporaryDirectory(prefix="rust-workbench-diagnostics-") as temp:
        staging = Path(temp) / "rust-workbench-diagnostics"
        staging.mkdir()
        included: list[dict[str, object]] = []
        for source_file in files:
            size = source_file.stat().st_size
            if size > maximum_file_bytes or copied_bytes + size > maximum_total_bytes:
                continue
            relative = source_file.relative_to(source)
            target = staging / "profiles" / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            if source_file.suffix.lower() in {".log", ".txt", ".json"} or source_file.name.endswith(
                ".log.old"
            ):
                content = source_file.read_text(encoding="utf-8", errors="replace")
                target.write_text(redact_diagnostic_text(content), encoding="utf-8")
            else:
                shutil.copy2(source_file, target)
            copied_bytes += size
            included.append({"path": relative.as_posix(), "bytes": size})
        manifest = {
            "format_version": 1,
            "created_utc": dt.datetime.now(dt.UTC).isoformat(),
            "source_commit": source_commit(),
            "minidumps_included": include_minidumps,
            "files": included,
        }
        (staging / "manifest.json").write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tarfile.open(destination, "w:gz") as archive:
            archive.add(staging, arcname=staging.name, recursive=True)
    print(f"diagnostics: {destination}")
    print(f"included files: {len(included)} ({human_size(copied_bytes)})")
    if not include_minidumps:
        print("minidumps: excluded (pass --include-minidumps to opt in)")
    return destination


def prune_diagnostics(*, data_root: Path | None, max_age_days: int, dry_run: bool) -> None:
    source = diagnostic_data_root(data_root)
    cutoff = time.time() - max_age_days * 24 * 60 * 60
    candidates = [
        path
        for path in diagnostic_files(source, include_minidumps=True)
        if path.stat().st_mtime < cutoff
    ]
    for path in candidates:
        relative = path.relative_to(source)
        if dry_run:
            print(f"would remove {relative}")
        else:
            path.unlink()
            print(f"removed {relative}")
    print(f"diagnostic files matched: {len(candidates)}")


def safe_generated_path(relative: str) -> Path:
    path = (ROOT / relative).resolve()
    if path == ROOT or ROOT not in path.parents:
        raise RuntimeError(f"refusing unsafe cleanup path: {path}")
    return path


def clean(mode: str, *, dry_run: bool) -> None:
    key = "debug_caches" if mode == "debug-caches" else "all_generated"
    targets = [safe_generated_path(relative) for relative in CONFIG["cleanup"][key]]
    for path in targets:
        if not path.exists():
            print(f"absent: {path.relative_to(ROOT)}")
            continue
        size = directory_bytes(path)
        if dry_run:
            print(f"would remove {path.relative_to(ROOT)} ({human_size(size)})")
        else:
            print(f"removing {path.relative_to(ROOT)} ({human_size(size)})", flush=True)
            if path.is_dir() and not path.is_symlink():
                shutil.rmtree(path)
            else:
                path.unlink()


def copy_tree(source: Path, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    run(["cp", "-a", "--reflink=auto", f"{source}/.", destination])


def copy_executable(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    destination.chmod(destination.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def package_version(name: str) -> str:
    completed = run(
        ["dpkg-query", "-W", "-f=${binary:Package}\t${Version}", name],
        capture=True,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(f"required native SDK package is not installed: {name}\n{completed.stderr}")
    return completed.stdout.strip()


def include_native_path(path: Path) -> bool:
    value = path.as_posix()
    prefixes = (
        "/bin/",
        "/lib/",
        "/lib64/",
        "/usr/bin/",
        "/usr/include/",
        "/usr/lib/",
        "/usr/libexec/",
        "/usr/share/doc/",
    )
    return any(value.startswith(prefix) for prefix in prefixes)


def lexists(path: Path) -> bool:
    return os.path.lexists(path)


def copy_native_path(source: Path, sysroot: Path) -> None:
    relative = source.relative_to("/")
    destination = sysroot / relative
    if source.is_dir() and not source.is_symlink():
        destination.mkdir(parents=True, exist_ok=True)
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    if source.is_symlink():
        target = os.readlink(source)
        if os.path.isabs(target):
            # Preserve the link inside the copied sysroot instead of allowing
            # it to escape back to the host filesystem.
            target = os.path.relpath(sysroot / target.lstrip("/"), destination.parent)
        if lexists(destination):
            if destination.is_symlink() and os.readlink(destination) == target:
                return
            destination.unlink()
        destination.symlink_to(target)
    elif source.is_file():
        shutil.copy2(source, destination)


def copy_native_sdk(app: Path) -> dict[str, str]:
    sysroot = app / "native" / "sysroot"
    sysroot.mkdir(parents=True)
    versions: dict[str, str] = {}
    for package in CONFIG["native_sdk"]["packages"]:
        versions[package] = package_version(package)
        listing = run(["dpkg-query", "-L", package], capture=True).stdout
        for item in listing.splitlines():
            source = Path(item)
            if include_native_path(source) and lexists(source):
                copy_native_path(source, sysroot)

    # usr-merged Ubuntu diverts this compatibility link, so it is not present
    # in `dpkg-query -L libc6`, although libc.so's linker script requires it.
    loader_link = sysroot / "lib64" / "ld-linux-x86-64.so.2"
    loader_link.parent.mkdir(parents=True, exist_ok=True)
    if not lexists(loader_link):
        loader_link.symlink_to("../usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2")

    native_bin = app / "native" / "bin"
    native_bin.mkdir(parents=True)
    copy_executable(BUNDLE_TEMPLATES / "native-cc", native_bin / "cc")
    (native_bin / "c++").symlink_to("cc")
    (native_bin / "gcc").symlink_to("cc")
    (native_bin / "g++").symlink_to("cc")
    copy_executable(BUNDLE_TEMPLATES / "native-binutils", native_bin / "ar")
    for tool in ("ranlib", "nm", "ld", "as", "strip", "objcopy"):
        (native_bin / tool).symlink_to("ar")
    return versions


def is_elf(path: Path) -> bool:
    if not path.is_file():
        return False
    try:
        with path.open("rb") as handle:
            return handle.read(4) == b"\x7fELF"
    except OSError:
        return False


def ldd_dependencies(path: Path, *, environment: dict[str, str]) -> list[tuple[str, Path]]:
    completed = run(["ldd", path], env=environment, capture=True, check=False)
    text = f"{completed.stdout}\n{completed.stderr}"
    if "not found" in text:
        raise RuntimeError(f"missing shared library for {path}:\n{text}")
    dependencies: list[tuple[str, Path]] = []
    for line in text.splitlines():
        mapped = re.match(r"\s*(\S+)\s+=>\s+(/\S+)", line)
        if mapped:
            dependencies.append((mapped.group(1), Path(mapped.group(2))))
            continue
        direct = re.match(r"\s*(/\S+)\s+\(", line)
        if direct:
            resolved = Path(direct.group(1))
            dependencies.append((resolved.name, resolved))
    return dependencies


def runtime_seed_binaries(app: Path) -> list[Path]:
    sysroot = app / "native" / "sysroot"
    seeds = [
        app / "libexec" / "rust-workbench",
        app / "libexec" / "rust-analyzer",
        app / "toolchain" / "bin" / "cargo",
        sysroot / "usr" / "bin" / "x86_64-linux-gnu-gcc-15",
        sysroot / "usr" / "bin" / "x86_64-linux-gnu-g++-15",
        sysroot / "usr" / "libexec" / "gcc" / "x86_64-linux-gnu" / "15" / "cc1",
        sysroot / "usr" / "libexec" / "gcc" / "x86_64-linux-gnu" / "15" / "cc1plus",
        sysroot / "usr" / "libexec" / "gcc" / "x86_64-linux-gnu" / "15" / "collect2",
        sysroot / "usr" / "bin" / "x86_64-linux-gnu-ld.bfd",
        sysroot / "usr" / "bin" / "x86_64-linux-gnu-as",
        sysroot / "usr" / "bin" / "x86_64-linux-gnu-ar",
    ]
    return [path for path in seeds if is_elf(path)]


def collect_runtime_libraries(app: Path) -> None:
    library_dir = app / "lib"
    library_dir.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment["LD_LIBRARY_PATH"] = str(library_dir)
    queue = runtime_seed_binaries(app)
    visited: set[Path] = set()
    copied: set[str] = set()
    while queue:
        binary = queue.pop()
        real_binary = binary.resolve()
        if real_binary in visited:
            continue
        visited.add(real_binary)
        for requested_name, resolved in ldd_dependencies(binary, environment=environment):
            if requested_name in CORE_GLIBC_LIBRARIES or requested_name.startswith("libnss_"):
                continue
            if requested_name in copied:
                continue
            if not resolved.is_file():
                raise RuntimeError(f"resolved dependency does not exist: {requested_name} -> {resolved}")
            destination = library_dir / requested_name
            shutil.copy2(resolved.resolve(), destination)
            copied.add(requested_name)
            queue.append(destination)


def component_version(command: Sequence[object], *, environment: dict[str, str] | None = None) -> str:
    completed = run(command, env=environment, capture=True, check=False)
    combined = completed.stdout.strip() or completed.stderr.strip()
    return combined.splitlines()[0] if combined else "unknown"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def bundle_environment(app: Path, data_dir: Path, target_dir: Path | None = None) -> dict[str, str]:
    environment = os.environ.copy()
    environment["RUST_WORKBENCH_DATA_DIR"] = str(data_dir)
    if target_dir is not None:
        environment["CARGO_TARGET_DIR"] = str(target_dir)
    return environment


def write_smoke_project(destination: Path) -> None:
    (destination / "src").mkdir(parents=True)
    (destination / "Cargo.toml").write_text(
        textwrap.dedent(
            """\
            [package]
            name = "rust_workbench_bundle_smoke"
            version = "0.1.0"
            edition = "2024"
            """
        ),
        encoding="utf-8",
    )
    (destination / "native.c").write_text(
        "int rust_workbench_native_answer(void) { return 42; }\n", encoding="utf-8"
    )
    (destination / "build.rs").write_text(
        textwrap.dedent(
            """\
            use std::{env, path::PathBuf, process::Command};

            fn checked(command: &mut Command) {
                let status = command.status().expect("failed to start native tool");
                assert!(status.success(), "native tool failed: {command:?}");
            }

            fn main() {
                let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
                let object = output.join("native.o");
                let archive = output.join("libnative_smoke.a");
                checked(Command::new(env::var_os("CC").expect("CC"))
                    .args(["-c", "native.c", "-o"]).arg(&object));
                checked(Command::new(env::var_os("AR").expect("AR"))
                    .arg("crs").arg(&archive).arg(&object));
                println!("cargo:rustc-link-search=native={}", output.display());
                println!("cargo:rustc-link-lib=static=native_smoke");
            }
            """
        ),
        encoding="utf-8",
    )
    (destination / "src" / "main.rs").write_text(
        textwrap.dedent(
            """\
            unsafe extern "C" {
                fn rust_workbench_native_answer() -> i32;
            }

            fn main() {
                let answer = unsafe { rust_workbench_native_answer() };
                assert_eq!(answer, 42);
                println!("bundle smoke passed: {answer}");
            }
            """
        ),
        encoding="utf-8",
    )


def write_broken_project(destination: Path) -> None:
    (destination / "src").mkdir(parents=True)
    (destination / "Cargo.toml").write_text(
        textwrap.dedent(
            """\
            [package]
            name = "rust_workbench_bundle_diagnostic"
            version = "0.1.0"
            edition = "2024"
            """
        ),
        encoding="utf-8",
    )
    (destination / "src" / "main.rs").write_text(
        textwrap.dedent(
            """\
            fn main() {
                let value = String::new();
                value.push_str("hello");
            }
            """
        ),
        encoding="utf-8",
    )


def smoke_bundle(app: Path, *, label: str) -> None:
    launcher = app / "bin" / "rust-workbench"
    with tempfile.TemporaryDirectory(prefix="rust-workbench-smoke-") as temp:
        temp_path = Path(temp)
        data = temp_path / "data"
        project = temp_path / "native-project"
        write_smoke_project(project)
        environment = bundle_environment(app, data, project / "target")
        run([launcher, "--doctor"], env=environment)
        run([launcher, "--run-toolchain", "cargo", "run", "--quiet", "--manifest-path", project / "Cargo.toml"], env=environment)

        broken = temp_path / "broken-project"
        write_broken_project(broken)
        diagnostic_environment = bundle_environment(app, data, broken / "target")
        diagnostic_environment["RUSTFLAGS"] = "-Zborrowck-wrapper-suggestions"
        completed = run(
            [
                launcher,
                "--run-toolchain",
                "cargo",
                "check",
                "--manifest-path",
                broken / "Cargo.toml",
                "--message-format=json",
            ],
            env=diagnostic_environment,
            check=False,
            capture=True,
        )
        combined = f"{completed.stdout}\n{completed.stderr}"
        if completed.returncode == 0:
            raise RuntimeError("the intentionally broken bundle fixture unexpectedly compiled")
        if "borrowck_wrapper_ref_cell" not in combined:
            raise RuntimeError(f"custom compiler suggestion missing from {label} bundle smoke test")

    print(f"bundle smoke passed: {label}")


def all_elf_files(app: Path) -> Iterable[Path]:
    for path in app.rglob("*"):
        if is_elf(path):
            yield path


def validate_bundle(app: Path) -> dict[str, object]:
    forbidden_paths = [
        path.relative_to(app).as_posix()
        for path in app.rglob("*")
        if "rustic" in path.name.lower() or "rustc_codegen_c" in path.name.lower()
    ]
    if forbidden_paths:
        raise RuntimeError(
            "removed Generated C components found in bundle:\n" + "\n".join(forbidden_paths)
        )

    editor = app / "libexec" / "rust-workbench"
    dynamic = run(["readelf", "-d", editor], capture=True).stdout
    if "$ORIGIN/../lib" not in dynamic:
        raise RuntimeError("portable editor is missing its $ORIGIN/../lib RPATH")
    if str(ROOT) in dynamic or "/home/" in dynamic:
        raise RuntimeError(f"editor contains a machine-specific runtime search path:\n{dynamic}")

    maximum_glibc = (0,)
    maximum_name = "0"
    absolute_rpaths: list[str] = []
    invalid_symlinks: list[str] = []
    elf_count = 0
    for path in app.rglob("*"):
        if not path.is_symlink():
            continue
        target = os.readlink(path)
        relative = path.relative_to(app).as_posix()
        if os.path.isabs(target):
            invalid_symlinks.append(f"{relative} -> {target} (absolute)")
        elif not path.resolve(strict=False).exists():
            # Debian documentation packages intentionally cross-link their
            # copyright directories.  Those links are not executable inputs;
            # all code/toolchain links remain strict.
            if not relative.startswith("native/sysroot/usr/share/doc/"):
                invalid_symlinks.append(f"{relative} -> {target} (broken)")
    for path in all_elf_files(app):
        elf_count += 1
        versions = run(
            ["readelf", "--version-info", path], capture=True, check=False, verbose=False
        ).stdout
        for version in re.findall(r"GLIBC_([0-9]+(?:\.[0-9]+)+)", versions):
            parsed = version_tuple(version)
            if parsed > maximum_glibc:
                maximum_glibc = parsed
                maximum_name = version
        tags = run(["readelf", "-d", path], capture=True, check=False, verbose=False).stdout
        for line in tags.splitlines():
            if ("RPATH" in line or "RUNPATH" in line) and (str(ROOT) in line or "/home/" in line):
                absolute_rpaths.append(f"{path.relative_to(app)}: {line.strip()}")

    allowed_glibc = CONFIG["compatibility"]["glibc_min"]
    if maximum_glibc > version_tuple(allowed_glibc):
        raise RuntimeError(f"bundle requires GLIBC {maximum_name}, newer than allowed {allowed_glibc}")
    if absolute_rpaths:
        raise RuntimeError("absolute runtime paths found:\n" + "\n".join(absolute_rpaths))
    if invalid_symlinks:
        raise RuntimeError("non-relocatable symlinks found:\n" + "\n".join(invalid_symlinks))

    environment = os.environ.copy()
    environment["LD_LIBRARY_PATH"] = str(app / "lib")
    for path in (
        editor,
        app / "libexec" / "rust-analyzer",
        app / "toolchain" / "bin" / "cargo",
        app / "toolchain" / "bin" / "rustc",
    ):
        result = run(["ldd", path], env=environment, capture=True, check=False)
        combined = f"{result.stdout}\n{result.stderr}"
        if "not found" in combined:
            raise RuntimeError(f"missing bundle dependency for {path}:\n{combined}")

    return {"elf_files": elf_count, "maximum_required_glibc": maximum_name}


def create_manifest(app: Path, native_versions: dict[str, str], validation: dict[str, object]) -> None:
    environment = os.environ.copy()
    environment["LD_LIBRARY_PATH"] = str(app / "lib")
    commit = source_commit()
    created = dt.datetime.fromtimestamp(source_epoch(), dt.UTC).isoformat()
    manifest = {
        "format_version": 2,
        "name": "Rust Workbench",
        "version": RELEASE_VERSION,
        "created_utc": created,
        "source_commit": commit,
        "compatibility": {
            "os": CONFIG["compatibility"]["os"],
            "architecture": CONFIG["compatibility"]["arch"],
            "minimum_glibc": CONFIG["compatibility"]["glibc_min"],
            "requires_vulkan_host_driver": True,
            "requires_x11_or_wayland_for_gui": True,
        },
        "components": {
            "rustc": component_version([app / "toolchain" / "bin" / "rustc", "-Vv"], environment=environment),
            "cargo": component_version([app / "toolchain" / "bin" / "cargo", "-V"], environment=environment),
            "rust_analyzer": component_version([app / "libexec" / "rust-analyzer", "--version"], environment=environment),
            "editor_sha256": sha256(app / "libexec" / "rust-workbench"),
            "analyzer_sha256": sha256(app / "libexec" / "rust-analyzer"),
            "rustc_sha256": sha256(app / "toolchain" / "bin" / "rustc"),
            "cargo_sha256": sha256(app / "toolchain" / "bin" / "cargo"),
        },
        "native_sdk_packages": native_versions,
        "validation": validation,
    }
    (app / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    checksum_paths = (
        app / "libexec" / "rust-workbench",
        app / "libexec" / "rust-analyzer",
        app / "toolchain" / "bin" / "rustc",
        app / "toolchain" / "bin" / "cargo",
    )
    (app / "checksums.sha256").write_text(
        "".join(
            f"{sha256(path)}  {path.relative_to(app)}\n"
            for path in checksum_paths
        ),
        encoding="utf-8",
    )


def populate_bundle(app: Path) -> dict[str, str]:
    (app / "bin").mkdir(parents=True)
    (app / "libexec").mkdir(parents=True)
    (app / "share" / "applications").mkdir(parents=True)
    (app / "share" / "icons" / "hicolor" / "512x512" / "apps").mkdir(parents=True)
    (app / "share" / "licenses" / "rust-workbench").mkdir(parents=True)

    copy_executable(EDITOR_BINARY, app / "libexec" / "rust-workbench")
    copy_executable(ANALYZER_BINARY, app / "libexec" / "rust-analyzer")
    copy_executable(BUNDLE_TEMPLATES / "rust-workbench", app / "bin" / "rust-workbench")
    copy_executable(BUNDLE_TEMPLATES / "install-desktop", app / "bin" / "install-desktop")

    copy_tree(STAGE1, app / "toolchain")
    copy_executable(CARGO, app / "toolchain" / "bin" / "cargo")
    # A compiler built in-tree installs these as absolute links back to the
    # checkout.  They are useful for compiler development, but would make the
    # supposedly portable bundle depend on this machine.  Materialize only the
    # rust-src component we promise to ship and drop the compiler-source link.
    bundled_rust_source = app / "toolchain" / "lib" / "rustlib" / "src" / "rust"
    if lexists(bundled_rust_source):
        bundled_rust_source.unlink()
    bundled_rust_source.mkdir(parents=True)
    copy_tree(RUST / "library", bundled_rust_source / "library")
    bundled_rustc_source = app / "toolchain" / "lib" / "rustlib" / "rustc-src" / "rust"
    if lexists(bundled_rustc_source):
        bundled_rustc_source.unlink()
    if (STAGE0 / "share" / "doc" / "cargo").is_dir():
        copy_tree(STAGE0 / "share" / "doc" / "cargo", app / "share" / "licenses" / "cargo")

    license_dir = app / "share" / "licenses" / "rust-workbench"
    shutil.copy2(RUST / "LICENSE-APACHE", license_dir / "rust-LICENSE-APACHE")
    shutil.copy2(RUST / "LICENSE-MIT", license_dir / "rust-LICENSE-MIT")
    shutil.copy2(ZED / "LICENSE-APACHE", license_dir / "zed-LICENSE-APACHE")
    shutil.copy2(ZED / "LICENSE-GPL", license_dir / "zed-LICENSE-GPL")
    shutil.copy2(
        ZED / "crates" / "zed" / "resources" / "app-icon-rust-workbench.png",
        app / "share" / "icons" / "hicolor" / "512x512" / "apps" / "rust-workbench.png",
    )
    shutil.copy2(
        BUNDLE_TEMPLATES / "dev.rustworkbench.RustWorkbench.desktop",
        app / "share" / "applications" / "dev.rustworkbench.RustWorkbench.desktop",
    )

    native_versions = copy_native_sdk(app)
    collect_runtime_libraries(app)
    for binary in (app / "libexec" / "rust-workbench", app / "libexec" / "rust-analyzer"):
        run(["strip", "--strip-debug", binary])
    return native_versions


def ensure_package_artifacts(*, skip_build: bool) -> None:
    if platform.system() != "Linux" or platform.machine() != CONFIG["compatibility"]["arch"]:
        raise SystemExit("this package target supports only x86_64 Linux")
    if skip_build:
        validate_build_receipt()
        missing = [
            path
            for path in (EDITOR_BINARY, ANALYZER_BINARY, CARGO, RUSTDOC, PROC_MACRO_SERVER)
            if not path.is_file()
        ]
        if missing:
            raise SystemExit("missing package artifacts:\n" + "\n".join(str(path) for path in missing))
        dynamic = run(["readelf", "-d", EDITOR_BINARY], capture=True, check=False).stdout
        if "$ORIGIN/../lib" not in dynamic:
            raise SystemExit(
                "the existing editor is a development build, not a portable build; "
                "run ./workbench build editor --portable before packaging with --skip-build"
            )
        if not any(STAGE1_TARGET_LIB.glob("libstd-*.rlib")):
            raise SystemExit(
                "missing stage1 standard library: "
                f"{STAGE1_TARGET_LIB}/libstd-*.rlib"
            )
    else:
        build_compiler()
        build_editor(portable=True)


def archive_bundle(staging: Path, archive: Path, app_name: str) -> None:
    if archive.exists():
        archive.unlink()
    epoch = source_epoch()
    run(
        [
            "tar",
            "--zstd",
            "--sort=name",
            f"--mtime=@{epoch}",
            "--owner=0",
            "--group=0",
            "--numeric-owner",
            "-cf",
            archive,
            "-C",
            staging,
            app_name,
        ]
    )
    checksum = sha256(archive)
    archive.with_suffix(archive.suffix + ".sha256").write_text(
        f"{checksum}  {archive.name}\n", encoding="utf-8"
    )


def verify_archive(archive: Path, *, container: bool = False) -> None:
    if not archive.is_file():
        raise SystemExit(f"bundle archive not found: {archive}")
    verify_root = DIST / ".verify path with spaces"
    if verify_root.exists():
        shutil.rmtree(verify_root)
    verify_root.mkdir(parents=True)
    try:
        run(["tar", "--zstd", "-xf", archive, "-C", verify_root])
        apps = list(verify_root.glob("*.app"))
        if len(apps) != 1:
            raise RuntimeError(f"expected one application directory in {archive}, found {apps}")
        app = apps[0]
        validate_bundle(app)
        smoke_bundle(app, label="relocated archive")
        if container:
            verify_bundle_in_container(app)
    finally:
        shutil.rmtree(verify_root, ignore_errors=True)


def verify_bundle_in_container(app: Path) -> None:
    if shutil.which("docker") is None:
        raise RuntimeError("docker is required for the clean-container bundle test")
    image = "ubuntu:26.04"
    inspect = run(["docker", "image", "inspect", image], check=False, capture=True)
    if inspect.returncode != 0:
        run(["docker", "pull", image])
    with tempfile.TemporaryDirectory(prefix="rust-workbench-container-smoke-") as temp:
        fixture = Path(temp) / "project"
        write_smoke_project(fixture)
        launcher = "/opt/rust-workbench.app/bin/rust-workbench"
        base = [
            "docker",
            "run",
            "--rm",
            "--network",
            "none",
            "-e",
            "RUST_WORKBENCH_DATA_DIR=/tmp/rust-workbench-data",
            "-e",
            "CARGO_TARGET_DIR=/work/target",
            "-v",
            f"{app}:/opt/rust-workbench.app:ro",
            "-v",
            f"{fixture}:/work",
            image,
        ]
        run([*base, "sh", "-c", "test ! -e /usr/bin/rustc && test ! -e /usr/bin/cargo && test ! -e /usr/bin/cc"])
        run([*base, launcher, "--doctor"])
        run([*base, launcher, "--run-toolchain", "cargo", "run", "--quiet", "--manifest-path", "/work/Cargo.toml"])
    print("bundle smoke passed: clean Ubuntu 26.04 container")


def package_linux(*, skip_build: bool, keep_staging: bool, verify: bool) -> Path:
    require_clean_source()
    ensure_package_artifacts(skip_build=skip_build)
    DIST.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".rust-workbench-package-", dir=DIST))
    app_name = "rust-workbench.app"
    app = staging / app_name
    archive = DIST / CONFIG["compatibility"]["archive"]
    try:
        native_versions = populate_bundle(app)
        validation = validate_bundle(app)
        create_manifest(app, native_versions, validation)
        manifest = archive.with_suffix(archive.suffix + ".manifest.json")
        shutil.copy2(app / "manifest.json", manifest)
        smoke_bundle(app, label="staging")
        archive_bundle(staging, archive, app_name)
        if verify:
            verify_archive(archive)
        print(f"bundle: {archive}")
        print(f"archive size: {human_size(archive.stat().st_size)}")
        print(f"sha256: {sha256(archive)}")
        print(f"manifest: {manifest}")
        if keep_staging:
            kept = DIST / "rust-workbench.app"
            if kept.exists():
                shutil.rmtree(kept)
            shutil.move(app, kept)
            print(f"staging application retained at {kept}")
        return archive
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def latest_archive() -> Path:
    configured = DIST / CONFIG["compatibility"]["archive"]
    if configured.is_file():
        return configured
    candidates = sorted(
        DIST.glob("rust-workbench-*-linux-*.tar.zst"), key=lambda path: path.stat().st_mtime
    )
    if not candidates:
        raise SystemExit("no packaged bundle found; run ./workbench package linux")
    return candidates[-1]


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    subcommands = result.add_subparsers(dest="command", required=True)
    subcommands.add_parser("doctor", help="validate source, tools, artifacts, and ABI")
    subcommands.add_parser("bootstrap", help="install workspace-local native dependencies")

    build_parser = subcommands.add_parser("build", help="build compiler, analyzer, editor, or all")
    build_parser.add_argument("component", choices=("compiler", "analyzer", "editor", "all"))
    build_parser.add_argument("--portable", action="store_true", help="link the editor for bundle layout")

    test_parser = subcommands.add_parser(
        "test", help="run correctness, resilience, UI, performance, or bundle gates"
    )
    test_parser.add_argument(
        "suite",
        choices=("quick", "full", "resilience", "ui", "multi-instance", "performance", "bundle"),
    )
    test_parser.add_argument("--archive", type=Path)
    test_parser.add_argument("--container", action="store_true")

    run_parser = subcommands.add_parser("run", help="launch Rust Workbench")
    run_parser.add_argument("paths", nargs="*")
    run_parser.add_argument("--debug", action="store_true")
    run_instance = run_parser.add_mutually_exclusive_group()
    run_instance.add_argument("--instance", help="use a named isolated editor profile")
    run_instance.add_argument(
        "--new-instance", action="store_true", help="create a new isolated editor profile"
    )

    package_parser = subcommands.add_parser("package", help="create a relocatable application archive")
    package_parser.add_argument("target", choices=("linux",))
    package_parser.add_argument("--skip-build", action="store_true")
    package_parser.add_argument("--keep-staging", action="store_true")
    package_parser.add_argument("--no-verify", action="store_true")

    subcommands.add_parser("disk", help="report build storage and live editor/analyzer RSS")

    diagnostics_parser = subcommands.add_parser(
        "diagnostics", help="collect or prune local logs and crash evidence"
    )
    diagnostics_commands = diagnostics_parser.add_subparsers(dest="diagnostics_command", required=True)
    collect_parser = diagnostics_commands.add_parser("collect")
    collect_parser.add_argument("--data-root", type=Path)
    collect_parser.add_argument("--output", type=Path)
    collect_parser.add_argument("--include-minidumps", action="store_true")
    prune_parser = diagnostics_commands.add_parser("prune")
    prune_parser.add_argument("--data-root", type=Path)
    prune_parser.add_argument("--max-age-days", type=int, default=30)
    prune_parser.add_argument("--dry-run", action="store_true")

    clean_parser = subcommands.add_parser("clean", help="remove only configured generated artifacts")
    selection = clean_parser.add_mutually_exclusive_group(required=True)
    selection.add_argument("--dry-run", action="store_true")
    selection.add_argument("--debug-caches", action="store_true")
    selection.add_argument("--all", action="store_true")
    return result


def main() -> int:
    arguments = parser().parse_args()
    if arguments.command == "doctor":
        return doctor()
    if arguments.command == "bootstrap":
        bootstrap()
    elif arguments.command == "build":
        build(arguments.component, portable=arguments.portable)
    elif arguments.command == "test":
        if arguments.suite in ("quick", "full"):
            test_quick_or_full(arguments.suite)
        elif arguments.suite == "resilience":
            test_resilience()
        elif arguments.suite == "ui":
            test_ui()
        elif arguments.suite == "multi-instance":
            test_multi_instance()
        elif arguments.suite == "performance":
            test_performance()
        else:
            verify_archive((arguments.archive or latest_archive()).resolve(), container=arguments.container)
    elif arguments.command == "run":
        run_editor(
            arguments.paths,
            debug=arguments.debug,
            instance=arguments.instance,
            new_instance=arguments.new_instance,
        )
    elif arguments.command == "package":
        package_linux(
            skip_build=arguments.skip_build,
            keep_staging=arguments.keep_staging,
            verify=not arguments.no_verify,
        )
    elif arguments.command == "disk":
        disk_report()
    elif arguments.command == "diagnostics":
        if arguments.diagnostics_command == "collect":
            collect_diagnostics(
                data_root=arguments.data_root,
                include_minidumps=arguments.include_minidumps,
                output_path=arguments.output,
            )
        else:
            if arguments.max_age_days < 1:
                raise SystemExit("--max-age-days must be at least 1")
            prune_diagnostics(
                data_root=arguments.data_root,
                max_age_days=arguments.max_age_days,
                dry_run=arguments.dry_run,
            )
    elif arguments.command == "clean":
        mode = "all" if arguments.all else "debug-caches"
        clean(mode, dry_run=arguments.dry_run)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(130) from None
