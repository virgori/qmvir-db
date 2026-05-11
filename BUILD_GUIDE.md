# QMvir Build & Distribution Guide

## Kiến trúc 2 Repo

```
┌──────────────────────────────────────────────────────────────────────────┐
│  PRIVATE: virgori/qmvir  ← Source code (Rust/Python/C)                    │
│  ├── qm_engine/ (Rust/PyO3 - Database core)                              │
│  ├── qm_native/ (Rust - Vector ops, HNSW)                               │
│  ├── qm_native_c/ (C/SIMD - Distance functions)                          │
│  ├── qmvir-studio/ (Tauri - Desktop app)                               │
│  └── scripts/build_release.py                                            │
└──────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ GitHub Actions (private token)
                                    ▼
┌──────────────────────────────────────────────────────────────────────────┐
│  PUBLIC: virgori/qmvir-db  ← THIS REPO                                  │
│  ├── .github/workflows/publish.yml  ← Build from private repo            │
│  ├── docs/              (Documentation)                                   │
│  ├── benchmarks/        (Performance reports)                             │
│  ├── scripts/           (Local build scripts)                             │
│  ├── install.sh         ← Quick install script                           │
│  ├── build_from_source.sh  ← Build script (needs repo access)             │
│  └── README.md                                                            │
└──────────────────────────────────────────────────────────────────────────┘
```

## Phương án 1: Pre-built Binaries (Không lộ source)

Người dùng cài đặt nhanh qua script:

```bash
curl -fsSL https://raw.githubusercontent.com/virgori/qmvir-db/main/install.sh | bash
```

Script tự động:
- Detect platform (macOS/Linux ARM64/x86_64)
- Download bundle từ GitHub Releases
- Extract và cài đặt

### Thủ công (không dùng script)

```bash
# 1. Vào https://github.com/virgori/qmvir-db/releases
# 2. Download file cho platform của bạn:
#    - qmvir-macos-arm64.tar.gz
#    - qmvir-macos-x86_64.tar.gz
#    - qmvir-linux-x86_64.tar.gz
#    - qmvir-linux-arm64.tar.gz

# 3. Extract và cài
tar -xzf qmvir-macos-arm64.tar.gz
pip install *.whl --force-reinstall
```

## Phương án 2: Build từ Source (Cần repo access)

### Yêu cầu
- SSH key hoặc PAT có quyền `repo` trên `virgori/qmvir`
- Rust (cargo) + Python3 + pip

### Cách build

```bash
# Clone repo private (cần authentication)
git clone git@github.com:virgori/qmvir.git
cd qmvir

# Build tất cả components
python scripts/build_release.py --install --verify

# Output trong dist/*.whl
```

Hoặc dùng script trong repo này:

```bash
curl -fsSL https://raw.githubusercontent.com/virgori/qmvir-db/main/build_from_source.sh | bash
```

## Phương án 3: GitHub Actions Build (Admin only)

Trigger workflow trong repo này để build từ private repo:

### Bước 1: Cấu hình Secrets

Vào `Settings → Secrets and variables → Actions`, thêm:

| Secret | Giá trị |
|--------|---------|
| `SOURCE_REPO_TOKEN` | GitHub PAT với quyền `repo` trên `virgori/qmvir` |
| `NPM_TOKEN` | (Optional) npm token để publish package |

### Bước 2: Trigger Build

Vào **Actions → Publish Release Binaries → Run workflow**:

```
version: v4.8.6   ← tag cần build
```

### Bước 3: Workflow chạy

| Job | Platform | Output |
|-----|----------|--------|
| build-linux | Ubuntu x86_64 | `qmvir-linux-x86_64.tar.gz` |
| build-linux-arm64 | Ubuntu ARM64 | `qmvir-linux-arm64.tar.gz` |
| build (macOS) | macOS 14/15 | `qmvir-macos-*.tar.gz` |
| build (Windows) | Windows latest | `qmvir-windows-x86_64.tar.gz` |
| build-studio | All platforms | DMG, DEB, AppImage, MSI |
| release | - | GitHub Release với tất cả files |
| publish-npm | - | Package trên npmjs.com |

### Bước 4: Download

Sau khi workflow hoàn thành, vào **Releases** để download.

## Scripts trong Repo này

| Script | Mục đích |
|--------|----------|
| `install.sh` | Cài đặt nhanh cho end users |
| `build_from_source.sh` | Build từ source (cần repo access) |
| `build.sh` | Link static library đã build sẵn |
| `scripts/build_release.py` | Full build script (copy từ source repo) |
| `scripts/build_native.sh` | Build native extensions local |

## Cách hoạt động của `publish.yml`

```yaml
# 1. Checkout từ PRIVATE repo
- uses: actions/checkout@v4
  with:
    repository: virgori/qmvir        # ← Private repo
    token: ${{ secrets.SOURCE_REPO_TOKEN }}

# 2. Build qm_engine (Rust/PyO3)
- uses: PyO3/maturin-action@v1
  with:
    target: x86_64-unknown-linux-gnu
    manylinux: 2_28

# 3. Build qm_native (Rust/Maturin)
# 4. Build qm_native_c (C/setuptools)
# 5. Build qmvir Python wheel
# 6. Package → tar.gz
# 7. Create GitHub Release
# 8. Publish npm (optional)
```

**Quan trọng:** Source code KHÔNG BAO GIỜ xuất hiện trong repo public này. Chỉ có output binaries trong Releases.

## Tóm tắt

| Cách | Source lộ? | Cần auth? | Output |
|------|-----------|-----------|--------|
| Pre-built | **Không** | Không | GitHub Releases |
| Build source | Có | SSH/PAT | Local wheels |
| GitHub Actions | **Không** | Secret token | GitHub Releases |
