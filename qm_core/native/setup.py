"""Build script for QM native C extension.

Usage:
    cd qm_core/native
    python setup.py build_ext --inplace

Or from project root:
    pip install -e .  (if qm_core has a pyproject.toml with native build)
"""

import platform
from setuptools import setup, Extension

extra_compile_args = ["-O3", "-Wall"]

# Platform-specific flags
machine = platform.machine().lower()
if machine in ("arm64", "aarch64"):
    # Apple Silicon / ARM: enable NEON + CRC32 hw instructions
    extra_compile_args += ["-march=armv8-a+crc", "-mfpu=neon"]
elif machine in ("x86_64", "amd64"):
    extra_compile_args += ["-msse4.2", "-mavx2"]

qm_native = Extension(
    "qm_native",
    sources=["qm_native.c"],
    extra_compile_args=extra_compile_args,
)

setup(
    name="qm_native",
    version="1.0.0",
    description="QM high-performance C kernels",
    ext_modules=[qm_native],
)
