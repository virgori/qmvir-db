# QMvir Documentation (v6.2.0)

Canonical documentation for **QMvir 6.2.0** — maintained with the `qm_engine` release.

## Primary guides

| # | Document | Description |
|---|----------|-------------|
| 1 | [QMVIR_ARCHITECTURE.md](QMVIR_ARCHITECTURE.md) | System architecture — gateway, engine, storage, cluster |
| 2 | [QMVIR_ALGORITHMS.md](QMVIR_ALGORITHMS.md) | Algorithms & data structures (with source file map) |
| 3 | [BASIC_USAGE.md](BASIC_USAGE.md) | Install, quick start, SQL, search, vector, backup, CLI |
| 4 | [ENTERPRISE_HA_GUIDE.md](ENTERPRISE_HA_GUIDE.md) | Enterprise HA, multi-DC, certification, deployment |
| 4b | [ENTERPRISE_HA_GUIDE_VI.md](ENTERPRISE_HA_GUIDE_VI.md) | Hướng dẫn Enterprise HA (Tiếng Việt) |

## Quick links

- **npm:** `npm install qmvir@6.2.0`
- **Binaries:** [github.com/virgori/qmvir-releases](https://github.com/virgori/qmvir-releases/releases/tag/v6.2.0)
- **Certify HA:** `qm cluster certify` (enterprise) · `qm cluster certify --chaos` (jepsen tier)
- **Build Linux releases on remote host:** `bash scripts/sync_and_build_release_quizzman.sh`

## Legacy / historical docs

Older audits, benchmark reports, optimization passes, and superseded guides are in **[\_archive/legacy/](_archive/legacy/)** — not maintained for v6.2.0. Use the five primary guides above for current behaviour.
