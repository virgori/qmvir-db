#!/usr/bin/env python3
"""Micro benchmark suite (Python vs C) for algorithm + runtime primitives.

Covers:
- large for/while loops
- integer arithmetic (+, -, *, /)
- fibonacci recursive and iterative
- function calls (simple, many args)
- string len/concat basic
- array traversal
- hashmap (dict)
- file read (small/medium)
- alloc/free style workload
"""

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any


PROFILES: dict[str, dict[str, int]] = {
    "quick": {
        "loop_iters": 2_000_000,
        "arith_iters": 2_000_000,
        "fib_recursive_n": 30,
        "fib_iterative_repeats": 500_000,
        "fib_iterative_n": 90,
        "simple_calls": 2_000_000,
        "many_arg_calls": 1_000_000,
        "string_repeats": 400_000,
        "array_size": 2_000_000,
        "hashmap_size": 300_000,
        "file_small_reads": 400,
        "file_medium_reads": 40,
        "alloc_iters": 300_000,
        "alloc_size": 64,
    },
    "standard": {
        "loop_iters": 6_000_000,
        "arith_iters": 6_000_000,
        "fib_recursive_n": 33,
        "fib_iterative_repeats": 1_200_000,
        "fib_iterative_n": 90,
        "simple_calls": 6_000_000,
        "many_arg_calls": 3_000_000,
        "string_repeats": 1_000_000,
        "array_size": 6_000_000,
        "hashmap_size": 900_000,
        "file_small_reads": 1000,
        "file_medium_reads": 120,
        "alloc_iters": 900_000,
        "alloc_size": 64,
    },
    "heavy": {
        "loop_iters": 12_000_000,
        "arith_iters": 12_000_000,
        "fib_recursive_n": 35,
        "fib_iterative_repeats": 2_400_000,
        "fib_iterative_n": 90,
        "simple_calls": 12_000_000,
        "many_arg_calls": 6_000_000,
        "string_repeats": 2_000_000,
        "array_size": 12_000_000,
        "hashmap_size": 1_800_000,
        "file_small_reads": 2000,
        "file_medium_reads": 240,
        "alloc_iters": 1_800_000,
        "alloc_size": 64,
    },
}


def _run_timed(name: str, ops: int, fn) -> dict[str, Any]:
    t0 = time.perf_counter()
    checksum = fn()
    elapsed = time.perf_counter() - t0
    ops_per_sec = float(ops / elapsed) if elapsed > 0 else 0.0
    return {
        "name": name,
        "ops": int(ops),
        "elapsed_ms": elapsed * 1000.0,
        "ops_per_sec": ops_per_sec,
        "checksum": int(checksum) & 0xFFFFFFFFFFFFFFFF,
    }


def fib_recursive(n: int) -> int:
    if n < 2:
        return n
    return fib_recursive(n - 1) + fib_recursive(n - 2)


def fib_iterative(n: int) -> int:
    a, b = 0, 1
    for _ in range(n):
        a, b = b, a + b
    return a


def _python_bench(cfg: dict[str, int], small_path: Path, medium_path: Path) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []

    loop_iters = cfg["loop_iters"]
    out.append(
        _run_timed(
            "for_loop_large",
            loop_iters,
            lambda: sum(i for i in range(loop_iters)),
        )
    )

    def _while_loop() -> int:
        i = 0
        s = 0
        while i < loop_iters:
            s += i
            i += 1
        return s

    out.append(_run_timed("while_loop_large", loop_iters, _while_loop))

    arith_iters = cfg["arith_iters"]

    def _int_arith() -> int:
        x = 7
        y = 3
        acc = 0
        for i in range(1, arith_iters + 1):
            x = (x + i) & 0xFFFFFFFF
            y = (y * 3 + 1) & 0xFFFFFFFF
            acc += x + y
            acc -= x - y
            acc += x * y
            acc += x // (y | 1)
        return acc

    out.append(_run_timed("int_arithmetic_add_sub_mul_div", arith_iters, _int_arith))

    fib_n = cfg["fib_recursive_n"]
    out.append(
        _run_timed(
            "fibonacci_recursive",
            1,
            lambda: fib_recursive(fib_n),
        )
    )

    fib_iter_repeats = cfg["fib_iterative_repeats"]
    fib_iter_n = cfg["fib_iterative_n"]

    def _fib_iter_work() -> int:
        s = 0
        for _ in range(fib_iter_repeats):
            s += fib_iterative(fib_iter_n)
        return s

    out.append(_run_timed("fibonacci_iterative", fib_iter_repeats, _fib_iter_work))

    def _simple_fn(v: int) -> int:
        return v + 1

    simple_calls = cfg["simple_calls"]

    def _simple_calls_work() -> int:
        s = 0
        for i in range(simple_calls):
            s += _simple_fn(i)
        return s

    out.append(_run_timed("function_call_simple", simple_calls, _simple_calls_work))

    def _many_args(a: int, b: int, c: int, d: int, e: int, f: int, g: int, h: int) -> int:
        return (a + b) ^ (c + d) ^ (e + f) ^ (g + h)

    many_calls = cfg["many_arg_calls"]

    def _many_calls_work() -> int:
        s = 0
        for i in range(many_calls):
            s += _many_args(i, i + 1, i + 2, i + 3, i + 4, i + 5, i + 6, i + 7)
        return s

    out.append(_run_timed("function_call_many_args", many_calls, _many_calls_work))

    str_repeats = cfg["string_repeats"]

    def _str_work() -> int:
        total = 0
        base = "qmvir"
        suffix = "benchmark"
        for i in range(str_repeats):
            s = base + str(i & 1023) + suffix
            total += len(s)
        return total

    out.append(_run_timed("string_len_concat_basic", str_repeats, _str_work))

    arr_n = cfg["array_size"]
    arr = [i & 255 for i in range(arr_n)]

    def _array_work() -> int:
        s = 0
        for v in arr:
            s += v
        return s

    out.append(_run_timed("array_traversal", arr_n, _array_work))

    hm_n = cfg["hashmap_size"]

    def _hashmap_work() -> int:
        d: dict[str, int] = {}
        for i in range(hm_n):
            d[f"k{i}"] = i
        s = 0
        for i in range(hm_n):
            s += d[f"k{i}"]
        return s

    out.append(_run_timed("hashmap_insert_lookup", hm_n * 2, _hashmap_work))

    small_reads = cfg["file_small_reads"]

    def _file_small() -> int:
        s = 0
        for _ in range(small_reads):
            s += len(small_path.read_bytes())
        return s

    out.append(_run_timed("file_read_small", small_reads, _file_small))

    medium_reads = cfg["file_medium_reads"]

    def _file_medium() -> int:
        s = 0
        for _ in range(medium_reads):
            s += len(medium_path.read_bytes())
        return s

    out.append(_run_timed("file_read_medium", medium_reads, _file_medium))

    alloc_iters = cfg["alloc_iters"]
    alloc_size = cfg["alloc_size"]

    def _alloc_work() -> int:
        s = 0
        for i in range(alloc_iters):
            b = bytearray(alloc_size)
            b[0] = i & 0xFF
            s += b[0]
        return s

    out.append(_run_timed("alloc_free_runtime", alloc_iters, _alloc_work))

    return out


def _build_c_binary(src: Path, out_bin: Path) -> None:
    cc = "cc"
    cmd = [cc, "-O3", "-std=c11", "-Wall", "-Wextra", "-o", str(out_bin), str(src)]
    subprocess.run(cmd, check=True)


def _run_c_bench(c_bin: Path, profile: str, small_path: Path, medium_path: Path) -> list[dict[str, Any]]:
    cmd = [str(c_bin), profile, str(small_path), str(medium_path)]
    proc = subprocess.run(cmd, check=True, capture_output=True, text=True)
    results: list[dict[str, Any]] = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 5:
            continue
        name, ops, elapsed_ms, ops_per_sec, checksum = parts
        results.append(
            {
                "name": name,
                "ops": int(ops),
                "elapsed_ms": float(elapsed_ms),
                "ops_per_sec": float(ops_per_sec),
                "checksum": int(checksum),
            }
        )
    return results


def _compare(py_results: list[dict[str, Any]], c_results: list[dict[str, Any]]) -> list[dict[str, Any]]:
    c_by_name = {r["name"]: r for r in c_results}
    rows: list[dict[str, Any]] = []
    for py in py_results:
        c = c_by_name.get(py["name"])
        if c is None:
            continue
        speedup = c["ops_per_sec"] / py["ops_per_sec"] if py["ops_per_sec"] > 0 else 0.0
        rows.append(
            {
                "name": py["name"],
                "python": py,
                "c": c,
                "c_vs_python_speedup": speedup,
            }
        )
    return rows


def _to_markdown(profile: str, rows: list[dict[str, Any]]) -> str:
    lines = [
        "# Microbenchmark: Python vs C",
        "",
        f"Profile: `{profile}`",
        "",
        "| Benchmark | Python ms | C ms | Python ops/s | C ops/s | C/Python |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for r in rows:
        p = r["python"]
        c = r["c"]
        lines.append(
            f"| {r['name']} | {p['elapsed_ms']:.3f} | {c['elapsed_ms']:.3f} | "
            f"{p['ops_per_sec']:.2f} | {c['ops_per_sec']:.2f} | {r['c_vs_python_speedup']:.2f}x |"
        )
    lines.append("")
    lines.append("Notes:")
    lines.append("- `hashmap_insert_lookup` uses Python `dict` and a linear-probing hashmap in C.")
    lines.append("- `alloc_free_runtime` is `bytearray` alloc in Python and `malloc/free` in C.")
    lines.append("- `file_read_small` and `file_read_medium` read the same generated files in both runtimes.")
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Micro benchmark suite (Python vs C)")
    p.add_argument("--profile", choices=sorted(PROFILES.keys()), default="quick")
    p.add_argument(
        "--output-json",
        default="/Users/gengyang/Desktop/AI/QM/benchmarks/MICROBENCH_PY_VS_C.json",
    )
    p.add_argument(
        "--output-md",
        default="/Users/gengyang/Desktop/AI/QM/benchmarks/MICROBENCH_PY_VS_C.md",
    )
    p.add_argument(
        "--c-src",
        default="/Users/gengyang/Desktop/AI/QM/benchmarks/microbench_runtime_vs_c.c",
    )
    p.add_argument(
        "--c-bin",
        default="/Users/gengyang/Desktop/AI/QM/benchmarks/.microbench_runtime_vs_c",
    )
    p.add_argument("--skip-c-build", action="store_true")
    return p.parse_args()


def main() -> None:
    args = parse_args()
    cfg = PROFILES[args.profile]

    out_json = Path(args.output_json)
    out_md = Path(args.output_md)
    c_src = Path(args.c_src)
    c_bin = Path(args.c_bin)

    with tempfile.TemporaryDirectory(prefix="qm_microbench_") as td:
        tmp = Path(td)
        small_path = tmp / "small.bin"
        medium_path = tmp / "medium.bin"

        small_path.write_bytes((b"QM" * 2048)[:4096])
        medium_path.write_bytes((b"QMVIR_BENCH" * 60000)[:524288])

        py_results = _python_bench(cfg, small_path, medium_path)

        if not args.skip_c_build:
            _build_c_binary(c_src, c_bin)
        c_results = _run_c_bench(c_bin, args.profile, small_path, medium_path)

    rows = _compare(py_results, c_results)

    payload = {
        "profile": args.profile,
        "config": cfg,
        "python_results": py_results,
        "c_results": c_results,
        "comparison": rows,
    }

    out_json.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    out_md.write_text(_to_markdown(args.profile, rows), encoding="utf-8")

    print(f"JSON written: {out_json}")
    print(f"MD written:   {out_md}")


if __name__ == "__main__":
    main()
