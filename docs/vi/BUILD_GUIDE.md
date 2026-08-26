# QMvir Build & Distribution Guide

## Mục tiêu

Repo `virgori/qmvir-db` là source private của QMvir. Release binary nên được build trên GitHub Actions để tránh Mac local phải cross-compile nặng.

## Kiến trúc hiện tại

```
PRIVATE: virgori/qmvir-db
├── qm_engine/                         # Rust engine + CLI
├── .github/workflows/release-gate.yml # CI smoke / tests
└── .github/workflows/release-binaries.yml
    └── GitHub-hosted runners build Linux, macOS, Windows binaries
```

Release artifact được gắn vào GitHub Release trong chính repo private `qmvir-db`.

## Build release trên GitHub

### Cách 1: Push tag

```bash
git tag v6.2.8
git push origin v6.2.8
```

Tag `v*` sẽ chạy workflow `Release Binaries` và build các artifact:

- `qm-linux-x86_64`
- `qm-linux-aarch64`
- `qm-macos-arm64`
- `qm-macos-x86_64`
- `qm-windows-x86_64.exe`

### Cách 2: Chạy thủ công

Vào GitHub repo `virgori/qmvir-db`:

1. `Actions`
2. `Release Binaries`
3. `Run workflow`
4. Nhập version, ví dụ `v6.2.8`

## CI trên repo private

| Workflow | Khi chạy | Mục đích |
|----------|----------|----------|
| `Release Gate` | PR / push `main` | `cargo check`, Rust tests, PyO3 smoke tests, HA gate |
| `Repo Hygiene` | PR / push | Kiểm tra duplicate files |
| `Release Binaries` | Tag `v*` hoặc manual | Build binary cross-platform trên GitHub |

## Secret cần thiết

Không cần `RELEASE_REPO_TOKEN` nếu publish release trong chính `qmvir-db`. Workflow dùng `GITHUB_TOKEN` mặc định với quyền `contents: write`.

## Build local nhẹ

Dùng khi dev nhanh trên máy local:

```bash
cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo build --manifest-path qm_engine/Cargo.toml --no-default-features --bin qm
```

## Build remote quizzman

Script này vẫn giữ lại như helper tùy chọn nếu cần build/test trên server riêng:

```bash
bash scripts/sync_and_build_release_quizzman.sh
```

Không cần dùng script này cho release chính nếu GitHub Actions đang chạy ổn.
