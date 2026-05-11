#!/usr/bin/env python3
"""
scripts/build_release.py — QMvir Cross-Platform Release Builder
================================================================
Builds all 4 components for the current platform:
  1. qm_engine   (Rust/PyO3)   → wheel via maturin
  2. qm_native   (Rust/PyO3)   → wheel via maturin
  3. qm_native_c (C/SIMD)      → wheel via setuptools
  4. qmvir       (Python)      → wheel via build

Usage:
    python scripts/build_release.py                    # full build
    python scripts/build_release.py --component engine # single component
    python scripts/build_release.py --target aarch64-apple-darwin  # cross
    python scripts/build_release.py --install          # build + install
"""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

QM_ROOT = Path(__file__).resolve().parent.parent
DIST_DIR = QM_ROOT / "dist"

COMPONENTS = ["engine", "native", "native_c", "python"]


def detect_target() -> str:
    """Auto-detect the Rust target triple for the current platform."""
    system = platform.system()
    machine = platform.machine().lower()

    if machine in ("x86_64", "amd64"):
        arch = "x86_64"
    elif machine in ("arm64", "aarch64"):
        arch = "aarch64"
    else:
        arch = machine

    if system == "Darwin":
        return f"{arch}-apple-darwin"
    elif system == "Linux":
        return f"{arch}-unknown-linux-gnu"
    elif system == "Windows":
        return f"{arch}-pc-windows-msvc"
    return f"{arch}-unknown-{system.lower()}"


def run(cmd: list[str], cwd: Path | None = None) -> None:
    print(f"  → {' '.join(cmd)}")
    subprocess.run(cmd, cwd=cwd, check=True)


def ensure_tool(name: str, install_cmd: list[str] | None = None) -> bool:
    """Check if a tool is available, optionally install it."""
    if shutil.which(name):
        return True
    if install_cmd:
        print(f"  Installing {name}...")
        run(install_cmd)
        return True
    print(f"  ✗ {name} not found")
    return False


def build_engine(target: str | None = None) -> Path:
    """Build qm_engine wheel via maturin."""
    print("\n═══ [1/4] qm_engine (Rust/PyO3) ═══")
    ensure_tool("maturin", [sys.executable, "-m", "pip", "install", "maturin"])

    cmd = ["maturin", "build", "--release", "--out", str(DIST_DIR)]
    if target:
        cmd.extend(["--target", target])

    run(cmd, cwd=QM_ROOT / "qm_engine")
    return DIST_DIR


def build_native(target: str | None = None) -> Path:
    """Build qm_native wheel via maturin."""
    print("\n═══ [2/4] qm_native (Rust/Maturin) ═══")
    ensure_tool("maturin", [sys.executable, "-m", "pip", "install", "maturin"])

    cmd = ["maturin", "build", "--release", "--out", str(DIST_DIR)]
    if target:
        cmd.extend(["--target", target])

    run(cmd, cwd=QM_ROOT / "qm_native")
    return DIST_DIR


def build_native_c() -> Path:
    """Build qm_native_c wheel via setuptools."""
    print("\n═══ [3/4] qm_native_c (C/SIMD) ═══")
    run([
        sys.executable, "setup.py",
        "bdist_wheel", "--dist-dir", str(DIST_DIR),
    ], cwd=QM_ROOT / "qm_native_c")
    return DIST_DIR


def build_python() -> Path:
    """Build qmvir pure Python wheel."""
    print("\n═══ [4/4] qmvir (Python orchestration) ═══")
    run([sys.executable, "-m", "pip", "install", "build", "-q"])
    run([
        sys.executable, "-m", "build",
        "--wheel", "--outdir", str(DIST_DIR),
    ], cwd=QM_ROOT)
    return DIST_DIR


def install_wheels() -> None:
    """Install all built wheels."""
    print("\n═══ Installing all wheels ═══")
    wheels = sorted(DIST_DIR.glob("*.whl"))
    if not wheels:
        print("  ✗ No wheels found in dist/")
        return
    run([sys.executable, "-m", "pip", "install", "--force-reinstall"] + [str(w) for w in wheels])


def verify() -> None:
    """Verify all components import correctly."""
    print("\n═══ Verification ═══")
    checks = [
        ("qm_engine", "from qm_engine import PostgresGateway; print('  ✓ qm_engine')"),
        ("qm_native", "from qm_native import NATIVE_AVAILABLE; print(f'  ✓ qm_native (available={NATIVE_AVAILABLE})')"),
        ("qm_native_c", "import qm_native_c; print('  ✓ qm_native_c')"),
        ("qmvir", "import qm_core; print('  ✓ qmvir')"),
    ]
    for name, code in checks:
        try:
            subprocess.run([sys.executable, "-c", code], check=True)
        except subprocess.CalledProcessError:
            print(f"  ⚠ {name} — import failed")


def main():
    parser = argparse.ArgumentParser(description="QMvir release builder")
    parser.add_argument("--component", choices=COMPONENTS + ["all"], default="all",
                        help="Which component to build (default: all)")
    parser.add_argument("--target", type=str, help="Rust target triple for cross-compilation")
    parser.add_argument("--install", action="store_true", help="Install after building")
    parser.add_argument("--verify", action="store_true", help="Verify imports after install")
    parser.add_argument("--clean", action="store_true", help="Clean dist/ before building")
    args = parser.parse_args()

    target = args.target or detect_target()
    print(f"Target:   {target}")
    print(f"Root:     {QM_ROOT}")
    print(f"Output:   {DIST_DIR}")

    if args.clean and DIST_DIR.exists():
        shutil.rmtree(DIST_DIR)
    DIST_DIR.mkdir(parents=True, exist_ok=True)

    builders = {
        "engine":   lambda: build_engine(args.target),
        "native":   lambda: build_native(args.target),
        "native_c": lambda: build_native_c(),
        "python":   lambda: build_python(),
    }

    if args.component == "all":
        for name, builder in builders.items():
            builder()
    else:
        builders[args.component]()

    wheels = sorted(DIST_DIR.glob("*.whl"))
    print(f"\n═══ Built {len(wheels)} wheel(s) ═══")
    for w in wheels:
        size_mb = w.stat().st_size / (1024 * 1024)
        print(f"  {w.name}  ({size_mb:.1f} MB)")

    if args.install:
        install_wheels()
    if args.verify:
        verify()

    print(f"\n✓ Done — wheels in {DIST_DIR}/")


if __name__ == "__main__":
    main()
