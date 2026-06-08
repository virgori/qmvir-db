"""qmvir logs - layered log viewer."""

from __future__ import annotations

import time
from pathlib import Path


_LAYER_FILES = {
    "gateway-rust": ["gateway-rust.log", "gateway.log"],
    "satellite-python": ["satellite-python.log", "satellite.log"],
    "satellite-vir": ["satellite-vir.log", "vir-satellite.log"],
}


def _tail_lines(path: Path, n: int) -> list[str]:
    if not path.exists():
        return []
    lines = path.read_text(errors="replace").splitlines()
    return lines[-n:] if n > 0 else lines


def _resolve_layer_paths(data_dir: str, layer: str) -> list[tuple[str, Path]]:
    logs_dir = Path(data_dir) / "logs"
    out: list[tuple[str, Path]] = []

    def _existing_candidates(names: list[str]) -> list[Path]:
        found: list[Path] = []
        for name in names:
            base = logs_dir / name
            if base.exists():
                found.append(base)
            # Include rotated files: <name>.1, <name>.2, ...
            found.extend(sorted(logs_dir.glob(f"{name}.*")))
        # Stable order: newest file last for natural tail display.
        unique = {p.resolve(): p for p in found if p.is_file()}
        return sorted(unique.values(), key=lambda p: p.name)

    if layer == "all":
        for lname in ("gateway-rust", "satellite-python", "satellite-vir"):
            for p in _existing_candidates(_LAYER_FILES[lname]):
                out.append((lname, p))
        return out

    for p in _existing_candidates(_LAYER_FILES.get(layer, [])):
        out.append((layer, p))
    return out


def run_logs(data_dir: str, layer: str = "all", tail: int = 200, follow: bool = False) -> int:
    paths = _resolve_layer_paths(data_dir, layer)
    if not paths:
        print(f"No log files found for layer={layer!r} under {Path(data_dir) / 'logs'}")
        return 1

    if not follow:
        for lname, p in paths:
            print(f"=== {lname} :: {p} ===")
            for line in _tail_lines(p, tail):
                print(line)
        return 0

    positions: dict[Path, int] = {}
    for _lname, p in paths:
        try:
            positions[p] = p.stat().st_size
            for line in _tail_lines(p, tail):
                print(line)
        except Exception:
            positions[p] = 0

    try:
        while True:
            for lname, p in paths:
                try:
                    size = p.stat().st_size
                    pos = positions.get(p, 0)
                    if size < pos:
                        pos = 0
                    if size > pos:
                        with p.open("r", errors="replace") as f:
                            f.seek(pos)
                            chunk = f.read()
                        for line in chunk.splitlines():
                            print(f"[{lname}] {line}")
                        positions[p] = size
                except Exception:
                    continue
            time.sleep(0.5)
    except KeyboardInterrupt:
        return 0
