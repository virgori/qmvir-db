#!/usr/bin/env bash
# Run P0 release gate on quizzman after sync.
set -euo pipefail
cd /root/qm-linux-bench
export PIP_BREAK_SYSTEM_PACKAGES=1
pip3 install -q --break-system-packages maturin 2>/dev/null || true
(cd qm_engine && cargo clean -q)
python3 -m maturin build --release --out /tmp/qm_linux_wheels 2>&1 | tail -5
WHEEL=$(ls -t /tmp/qm_linux_wheels/qmvir-*.whl /tmp/qm_linux_wheels/qm_engine-*.whl 2>/dev/null | head -1)
echo "installing $WHEEL"
pip3 install -q --break-system-packages --force-reinstall "$WHEEL"
python3 -c 'import qm_engine; print("qm_engine:", qm_engine.__file__)'

# Optional: point QM_PREV_WHEEL at prior release wheel for true N-1 upgrade test
export QM_PREV_WHEEL="${QM_PREV_WHEEL:-}"

echo '=== P0 release gate (quick) ==='
python3 scripts/release_gate_realdata.py --quick --output /tmp/qm_release_gate_quick.json
echo '=== P0 release gate (full bulk may take several minutes) ==='
python3 scripts/release_gate_realdata.py --output /tmp/qm_release_gate_full.json
python3 -c '
import json
for path in ["/tmp/qm_release_gate_quick.json","/tmp/qm_release_gate_full.json"]:
    d=json.load(open(path))
    print("\n=== %s: %s/%s ok=%s ===" % (path, d.get("passed"), d.get("total"), d.get("ok")))
    for name, sec in d.get("sections", {}).items():
        print("  %-28s %s" % (name, "PASS" if sec.get("ok") else "FAIL"))
'
