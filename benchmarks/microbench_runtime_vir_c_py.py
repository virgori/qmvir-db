#!/usr/bin/env python3
"""Micro benchmark suite for Vir vs C vs Python.

Coverage:
- large for/while loops
- integer arithmetic (+, -, *, /)
- fibonacci recursive and iterative
- function calls (simple, many args)
- string length / concat basic
- array traversal
- hashmap (Python/C, Vir marked unsupported in bootstrap runtime)
- file read small / medium
- alloc/free style workload
"""

from __future__ import annotations

import argparse
import json
import re
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

    def _for_loop() -> int:
        s = 0
        for i in range(loop_iters):
            s += i
        return s

    out.append(_run_timed("for_loop_large", loop_iters, _for_loop))

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
    out.append(_run_timed("fibonacci_recursive", 1, lambda: fib_recursive(fib_n)))

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
    out.append(_run_timed("file_read_small", small_reads, lambda: sum(len(small_path.read_bytes()) for _ in range(small_reads))))

    medium_reads = cfg["file_medium_reads"]
    out.append(_run_timed("file_read_medium", medium_reads, lambda: sum(len(medium_path.read_bytes()) for _ in range(medium_reads))))

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
    cmd = ["cc", "-O3", "-std=c11", "-Wall", "-Wextra", "-o", str(out_bin), str(src)]
    subprocess.run(cmd, check=True)


def _run_c_bench(c_bin: Path, profile: str, small_path: Path, medium_path: Path) -> list[dict[str, Any]]:
    proc = subprocess.run([str(c_bin), profile, str(small_path), str(medium_path)], check=True, capture_output=True, text=True)
    results: list[dict[str, Any]] = []
    for line in proc.stdout.splitlines():
        parts = line.strip().split("\t")
        if len(parts) != 5:
            continue
        name, ops, elapsed_ms, ops_per_sec, checksum = parts
        results.append({
            "name": name,
            "ops": int(ops),
            "elapsed_ms": float(elapsed_ms),
            "ops_per_sec": float(ops_per_sec),
            "checksum": int(checksum),
            "status": "ok",
        })
    return results


def _vir_profile(cfg: dict[str, int]) -> dict[str, int]:
    # Keep Vir quick enough for interactive runs while preserving workload shape.
    return {
        "loop_iters": max(80_000, cfg["loop_iters"] // 40),
        "arith_iters": max(80_000, cfg["arith_iters"] // 40),
        "fib_recursive_n": min(28, cfg["fib_recursive_n"]),
        "fib_iterative_repeats": max(30_000, cfg["fib_iterative_repeats"] // 20),
        "fib_iterative_n": cfg["fib_iterative_n"],
        "simple_calls": max(80_000, cfg["simple_calls"] // 40),
        "many_arg_calls": max(60_000, cfg["many_arg_calls"] // 30),
        "string_repeats": max(60_000, cfg["string_repeats"] // 20),
        "array_size": max(80_000, cfg["array_size"] // 40),
        "hashmap_size": max(20_000, cfg["hashmap_size"] // 15),
        "file_small_reads": max(120, cfg["file_small_reads"] // 3),
        "file_medium_reads": max(15, cfg["file_medium_reads"] // 3),
        "alloc_iters": max(60_000, cfg["alloc_iters"] // 20),
        "alloc_size": cfg["alloc_size"],
    }


def _last_int(stdout: str) -> int:
    vals = re.findall(r"-?\d+", stdout)
    if not vals:
        raise ValueError("No integer found in Vir output")
    return int(vals[-1])


def _vir_code(case: str, cfg: dict[str, int], small_path: Path, medium_path: Path) -> str:
    if case == "for_loop_large":
        n = cfg["loop_iters"]
        return f"func main() then\n    var i = 0\n    var s = 0\n    while i < {n} then\n        s = s + i\n        i = i + 1\n    end\n    print s\n    return 0\nend\n"
    if case == "while_loop_large":
        n = cfg["loop_iters"]
        return f"func main() then\n    var i = 0\n    var s = 0\n    while i < {n} then\n        s = s + i\n        i = i + 1\n    end\n    print s\n    return 0\nend\n"
    if case == "int_arithmetic_add_sub_mul_div":
        n = cfg["arith_iters"]
        return f"func main() then\n    var x = 7\n    var y = 3\n    var acc = 0\n    var i = 1\n    while i <= {n} then\n        x = (x + i) & 4294967295\n        y = (y * 3 + 1) & 4294967295\n        acc = acc + x + y\n        acc = acc - (x - y)\n        acc = acc + (x * y)\n        acc = acc + (x / (y | 1))\n        i = i + 1\n    end\n    print acc\n    return 0\nend\n"
    if case == "fibonacci_recursive":
        n = cfg["fib_recursive_n"]
        return f"func fib(n) then\n    if n < 2 then\n        return n\n    end\n    return fib(n - 1) + fib(n - 2)\nend\n\nfunc main() then\n    print fib({n})\n    return 0\nend\n"
    if case == "fibonacci_iterative":
        reps = cfg["fib_iterative_repeats"]
        n = cfg["fib_iterative_n"]
        return f"func fibi(n) then\n    var a = 0\n    var b = 1\n    var i = 0\n    while i < n then\n        var t = a + b\n        a = b\n        b = t\n        i = i + 1\n    end\n    return a\nend\n\nfunc main() then\n    var i = 0\n    var s = 0\n    while i < {reps} then\n        s = s + fibi({n})\n        i = i + 1\n    end\n    print s\n    return 0\nend\n"
    if case == "function_call_simple":
        n = cfg["simple_calls"]
        return f"func inc(v) then\n    return v + 1\nend\n\nfunc main() then\n    var i = 0\n    var s = 0\n    while i < {n} then\n        s = s + inc(i)\n        i = i + 1\n    end\n    print s\n    return 0\nend\n"
    if case == "function_call_many_args":
        n = cfg["many_arg_calls"]
        return f"func mix(a, b, c, d, e, f, g, h) then\n    return (a + b) xor (c + d) xor (e + f) xor (g + h)\nend\n\nfunc main() then\n    var i = 0\n    var s = 0\n    while i < {n} then\n        s = s + mix(i, i + 1, i + 2, i + 3, i + 4, i + 5, i + 6, i + 7)\n        i = i + 1\n    end\n    print s\n    return 0\nend\n"
    if case == "string_len_concat_basic":
        n = cfg["string_repeats"]
        return f"func main() then\n    var i = 0\n    var s = 0\n    while i < {n} then\n        var t = str_cat(\"qmvir\", \"benchmark\")\n        s = s + str_len(t)\n        i = i + 1\n    end\n    print s\n    return 0\nend\n"
    if case == "array_traversal":
        n = cfg["array_size"]
        return f"func main() then\n    var arr = []\n    var i = 0\n    while i < {n} then\n        push(arr, i & 255)\n        i = i + 1\n    end\n    var j = 0\n    var s = 0\n    while j < len(arr) then\n        s = s + arr[j]\n        j = j + 1\n    end\n    print s\n    return 0\nend\n"
    if case == "file_read_small":
        p = str(small_path)
        r = cfg["file_small_reads"]
        return f"func main() then\n    var i = 0\n    var s = 0\n    while i < {r} then\n        var fd = file_open(\"{p}\", \"r\")\n        var c = file_read(fd)\n        file_close(fd)\n        s = s + str_len(c)\n        i = i + 1\n    end\n    print s\n    return 0\nend\n"
    if case == "file_read_medium":
        p = str(medium_path)
        r = cfg["file_medium_reads"]
        return f"func main() then\n    var i = 0\n    var s = 0\n    while i < {r} then\n        var fd = file_open(\"{p}\", \"r\")\n        var c = file_read(fd)\n        file_close(fd)\n        s = s + str_len(c)\n        i = i + 1\n    end\n    print s\n    return 0\nend\n"
    if case == "alloc_free_runtime":
        n = cfg["alloc_iters"]
        sz = cfg["alloc_size"]
        return f"func main() then\n    var i = 0\n    var s = 0\n    while i < {n} then\n        var p = alloc({sz})\n        write_byte(p, 0, i & 255)\n        s = s + read_byte(p, 0)\n        dealloc(p)\n        i = i + 1\n    end\n    print s\n    return 0\nend\n"
    raise ValueError(f"unknown case: {case}")


def _run_vir_case(vir_bin: Path, vir_root: Path, case: str, ops: int, cfg: dict[str, int], small_path: Path, medium_path: Path, work: Path) -> dict[str, Any]:
    if case == "hashmap_insert_lookup":
        return {
            "name": case,
            "ops": int(ops),
            "elapsed_ms": 0.0,
            "ops_per_sec": 0.0,
            "checksum": 0,
            "status": "skipped",
            "reason": "hashmap benchmark not wired in bootstrap runtime without imports",
        }

    src = work / f"{case}.vir"
    src.write_text(_vir_code(case, cfg, small_path, medium_path), encoding="utf-8")

    t0 = time.perf_counter()
    proc = subprocess.run([str(vir_bin), "run", str(src)], cwd=str(vir_root), capture_output=True, text=True, timeout=300)
    elapsed = time.perf_counter() - t0
    if proc.returncode != 0:
        return {
            "name": case,
            "ops": int(ops),
            "elapsed_ms": elapsed * 1000.0,
            "ops_per_sec": 0.0,
            "checksum": 0,
            "status": "error",
            "reason": proc.stderr.strip() or proc.stdout.strip() or "Vir run failed",
        }

    checksum = _last_int(proc.stdout)
    return {
        "name": case,
        "ops": int(ops),
        "elapsed_ms": elapsed * 1000.0,
        "ops_per_sec": float(ops / elapsed) if elapsed > 0 else 0.0,
        "checksum": checksum,
        "status": "ok",
    }


def _run_vir_bench(profile: str, base_cfg: dict[str, int], small_path: Path, medium_path: Path, vir_bin: Path, vir_root: Path, work: Path) -> list[dict[str, Any]]:
    cfg = _vir_profile(base_cfg)
    ops_by_case = {
        "for_loop_large": cfg["loop_iters"],
        "while_loop_large": cfg["loop_iters"],
        "int_arithmetic_add_sub_mul_div": cfg["arith_iters"],
        "fibonacci_recursive": 1,
        "fibonacci_iterative": cfg["fib_iterative_repeats"],
        "function_call_simple": cfg["simple_calls"],
        "function_call_many_args": cfg["many_arg_calls"],
        "string_len_concat_basic": cfg["string_repeats"],
        "array_traversal": cfg["array_size"],
        "hashmap_insert_lookup": cfg["hashmap_size"] * 2,
        "file_read_small": cfg["file_small_reads"],
        "file_read_medium": cfg["file_medium_reads"],
        "alloc_free_runtime": cfg["alloc_iters"],
    }

    ordered_cases = [
        "for_loop_large",
        "while_loop_large",
        "int_arithmetic_add_sub_mul_div",
        "fibonacci_recursive",
        "fibonacci_iterative",
        "function_call_simple",
        "function_call_many_args",
        "string_len_concat_basic",
        "array_traversal",
        "hashmap_insert_lookup",
        "file_read_small",
        "file_read_medium",
        "alloc_free_runtime",
    ]

    out: list[dict[str, Any]] = []
    for case in ordered_cases:
        out.append(_run_vir_case(vir_bin, vir_root, case, ops_by_case[case], cfg, small_path, medium_path, work))
    return out


def _merge(py_results: list[dict[str, Any]], c_results: list[dict[str, Any]], vir_results: list[dict[str, Any]]) -> list[dict[str, Any]]:
    c_by_name = {r["name"]: r for r in c_results}
    v_by_name = {r["name"]: r for r in vir_results}
    rows: list[dict[str, Any]] = []
    for py in py_results:
        name = py["name"]
        c = c_by_name.get(name)
        v = v_by_name.get(name)
        row: dict[str, Any] = {"name": name, "python": py, "c": c, "vir": v}
        if c and py["ops_per_sec"] > 0:
            row["c_vs_python_speedup"] = c["ops_per_sec"] / py["ops_per_sec"]
        if v and v.get("status") == "ok" and py["ops_per_sec"] > 0:
            row["vir_vs_python_speedup"] = v["ops_per_sec"] / py["ops_per_sec"]
        if c and v and v.get("status") == "ok" and v["ops_per_sec"] > 0:
            row["c_vs_vir_speedup"] = c["ops_per_sec"] / v["ops_per_sec"]
        rows.append(row)
    return rows


def _fmt_engine(r: dict[str, Any] | None) -> tuple[str, str]:
    if not r:
        return "-", "-"
    if r.get("status") == "skipped":
        return "SKIP", "SKIP"
    if r.get("status") == "error":
        return "ERR", "ERR"
    return f"{r['elapsed_ms']:.3f}", f"{r['ops_per_sec']:.2f}"


def _to_markdown(profile: str, rows: list[dict[str, Any]], vir_note: str) -> str:
    lines = [
        "# Microbenchmark: Vir vs C vs Python",
        "",
        f"Profile: `{profile}`",
        "",
        "| Benchmark | Python ms | C ms | Vir ms | Python ops/s | C ops/s | Vir ops/s | C/Python | Vir/Python | C/Vir |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for row in rows:
        p = row["python"]
        c = row.get("c")
        v = row.get("vir")
        c_ms, c_ops = _fmt_engine(c)
        v_ms, v_ops = _fmt_engine(v)
        cpy = row.get("c_vs_python_speedup", 0.0)
        vpy = row.get("vir_vs_python_speedup", 0.0)
        cv = row.get("c_vs_vir_speedup", 0.0)
        lines.append(
            f"| {row['name']} | {p['elapsed_ms']:.3f} | {c_ms} | {v_ms} | {p['ops_per_sec']:.2f} | {c_ops} | {v_ops} | {cpy:.2f}x | {vpy:.2f}x | {cv:.2f}x |"
        )

    lines.extend([
        "",
        "Notes:",
        "- Vir results are measured by wall-clock timing around `vir run <case>.vir`.",
        "- `hashmap_insert_lookup` is currently `SKIP` on Vir bootstrap runtime in this harness.",
        f"- Vir config is downscaled from profile for practical run time: {vir_note}.",
    ])
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Micro benchmark suite (Vir vs C vs Python)")
    p.add_argument("--profile", choices=sorted(PROFILES.keys()), default="quick")
    p.add_argument("--output-json", default="/Users/gengyang/Desktop/AI/QM/benchmarks/MICROBENCH_PY_VS_C.json")
    p.add_argument("--output-md", default="/Users/gengyang/Desktop/AI/QM/benchmarks/MICROBENCH_PY_VS_C.md")
    p.add_argument("--c-src", default="/Users/gengyang/Desktop/AI/QM/benchmarks/microbench_runtime_vs_c.c")
    p.add_argument("--c-bin", default="/Users/gengyang/Desktop/AI/QM/benchmarks/.microbench_runtime_vs_c")
    p.add_argument("--vir-bin", default="/Users/gengyang/Desktop/AI/Vir/core/build/vir")
    p.add_argument("--vir-root", default="/Users/gengyang/Desktop/AI/Vir/core")
    p.add_argument("--skip-c-build", action="store_true")
    p.add_argument("--skip-vir", action="store_true")
    return p.parse_args()


def main() -> None:
    args = parse_args()
    cfg = PROFILES[args.profile]

    out_json = Path(args.output_json)
    out_md = Path(args.output_md)
    c_src = Path(args.c_src)
    c_bin = Path(args.c_bin)
    vir_bin = Path(args.vir_bin)
    vir_root = Path(args.vir_root)

    with tempfile.TemporaryDirectory(prefix="qm_microbench_vcp_") as td:
        tmp = Path(td)
        small_path = tmp / "small.bin"
        medium_path = tmp / "medium.bin"
        work_vir = tmp / "vir_cases"
        work_vir.mkdir(parents=True, exist_ok=True)

        small_path.write_bytes((b"QM" * 2048)[:4096])
        medium_path.write_bytes((b"QMVIR_BENCH" * 60000)[:524288])

        py_results = _python_bench(cfg, small_path, medium_path)

        if not args.skip_c_build:
            _build_c_binary(c_src, c_bin)
        c_results = _run_c_bench(c_bin, args.profile, small_path, medium_path)

        if args.skip_vir:
            vir_results = []
        else:
            if not vir_bin.exists():
                raise SystemExit(f"Vir binary not found: {vir_bin}")
            vir_results = _run_vir_bench(args.profile, cfg, small_path, medium_path, vir_bin, vir_root, work_vir)

    rows = _merge(py_results, c_results, vir_results)
    vir_cfg = _vir_profile(cfg)

    payload = {
        "profile": args.profile,
        "config_python_c": cfg,
        "config_vir": vir_cfg,
        "python_results": py_results,
        "c_results": c_results,
        "vir_results": vir_results,
        "comparison": rows,
    }

    out_json.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    out_md.write_text(_to_markdown(args.profile, rows, f"loop={vir_cfg['loop_iters']}, arith={vir_cfg['arith_iters']}, fib_rec_n={vir_cfg['fib_recursive_n']}"), encoding="utf-8")

    print(f"JSON written: {out_json}")
    print(f"MD written:   {out_md}")


if __name__ == "__main__":
    main()
