# QMvir Documentation (v6.2.8)

Canonical documentation for **QMvir 6.2.8**. Guides are grouped by language, with maintainer-only handoff notes in `internal/`.

## English

| Document | Description |
|----------|-------------|
| [Basic Usage](en/BASIC_USAGE.md) | Install, quick start, SQL, search, vector, backup, CLI |
| [Enterprise HA](en/ENTERPRISE_HA_GUIDE.md) | Multi-DC deployment, certification, env vars |
| [HTAP](en/HTAP_GUIDE.md) | HTAP, MVCC, planner, PITR |

## Vietnamese

| Tài liệu | Nội dung |
|----------|----------|
| [Kiến trúc](vi/QMVIR_ARCHITECTURE.md) | Gateway, engine, storage, enterprise cluster |
| [Thuật toán](vi/QMVIR_ALGORITHMS.md) | Data structures & algorithms with source map |
| [Enterprise HA](vi/ENTERPRISE_HA_GUIDE.md) | Hướng dẫn HA / multi-DC tiếng Việt |
| [HTAP](vi/HTAP_GUIDE.md) | HTAP / MVCC / PITR tiếng Việt |
| [Build & Distribution](vi/BUILD_GUIDE.md) | Build, release, GitHub Actions, quizzman helper |

## Internal

| Document | Purpose |
|----------|---------|
| [Bench Optimization Handover](internal/HANDOVER_BENCH_OPT.md) | Maintainer handover notes for benchmark optimization |

## Quick Links

- **Source repo:** private `virgori/qmvir-db`
- **GitHub release build:** `.github/workflows/release-binaries.yml`
- **Certify HA:** `qm cluster certify` · `qm cluster certify --chaos`
- **Remote build helper:** `bash scripts/sync_and_build_release_quizzman.sh`

Historical audits and superseded Python-era docs were removed from the active tree.
