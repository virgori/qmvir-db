# QMvir Build & Distribution Guide

## Kiến trúc 2 repo

```
┌──────────────────────────────────────────────────────────────────────────┐
│  PRIVATE: virgori/qmvir-db  ← Source (Rust/Python), chỉ team có quyền   │
│  ├── qm_engine/                                                           │
│  ├── .github/workflows/release-gate.yml   (CI build/test)                 │
│  └── .github/workflows/release-binaries.yml (manual/tag → binaries only)  │
└──────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ GitHub Actions (RELEASE_REPO_TOKEN)
                                    ▼
┌──────────────────────────────────────────────────────────────────────────┐
│  PUBLIC: virgori/qmvir-releases  ← Chỉ bare binaries + install scripts   │
│  ├── qm-macos-arm64, qm-linux-x86_64, qm-windows-x86_64.exe, ...         │
│  └── npm postinstall tải binary từ đây                                   │
└──────────────────────────────────────────────────────────────────────────┘
```

**Không publish source** (wheels, `.tar.gz` chứa `.whl`, hay GitHub Release trên `qmvir-db`).

## Cài đặt end-user

```bash
npm install -g qmvir
# hoặc
curl -fsSL https://raw.githubusercontent.com/virgori/qmvir-releases/main/install.sh | bash
```

Binary tải từ: https://github.com/virgori/qmvir-releases/releases

## CI trên repo private

| Workflow | Khi chạy | Mục đích |
|----------|----------|----------|
| `Release Gate` | PR / push `main` | `cargo check`, pytest, benchmark smoke |
| `Repo Hygiene` | PR / push | Kiểm tra duplicate files |
| `Release Binaries` | Tag `v*` hoặc manual | Build `qm` binary → **qmvir-releases** + npm |
| `QMvir — Cross-Platform Build` | Manual only | Build wheels nội bộ (artifact CI, không public) |

Workflow `Publish Release Binaries (deprecated)` đã tắt.

## Secrets (Settings → Actions)

| Secret | Mục đích |
|--------|----------|
| `RELEASE_REPO_TOKEN` | PAT `repo` scope — tạo release trên `virgori/qmvir-releases` |
| `NPM_TOKEN` | (Optional) publish `qmvir` lên npmjs.com |

## Publish release (admin)

### Cách 1: GitHub Actions (từ repo private)

1. Bump version trong `qm_engine/Cargo.toml`.
2. Push tag: `git tag v6.2.4 && git push origin v6.2.4`
3. Hoặc **Actions → Release Binaries → Run workflow** với `version: v6.2.4`

### Cách 2: Local (quizzman + Mac)

```bash
bash scripts/sync_and_build_release_quizzman.sh --mac-too
bash scripts/publish_packages.sh
```

## Build từ source (chỉ collaborator)

```bash
git clone git@github.com:virgori/qmvir-db.git
cd qmvir-db/qm_engine
cargo build --release --no-default-features --bin qm
```
