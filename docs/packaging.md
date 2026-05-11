# QMvir — Cross-Platform Binary Packaging

## Architecture

QMvir là hệ thống 4 tầng, mỗi tầng cần đóng gói riêng cho từng nền tảng:

```
┌─────────────────────────────────────────────────────────┐
│  qmvir (Python)         CLI, daemon, gateway, caching   │
│  ──────────────────────────────────────────────────────  │
│  qm_engine (Rust/PyO3)  DB core, SQL, SIMD JOIN, WAL    │
│  qm_native (Rust/PyO3)  HNSW vector index, distance     │
│  qm_native_c (C/SIMD)   Low-level NEON/AVX2 distance    │
└─────────────────────────────────────────────────────────┘
```

| Component | Build tool | Output | Platform-specific? |
|---|---|---|---|
| **qm_engine** | maturin (Rust → PyO3) | `.whl` chứa `.so`/`.dylib`/`.pyd` | ✅ OS × Arch × Python |
| **qm_native** | maturin (Rust → PyO3) | `.whl` chứa `.so`/`.dylib`/`.pyd` | ✅ OS × Arch × Python |
| **qm_native_c** | setuptools (C ext) | `.whl` chứa `.so`/`.pyd` | ✅ OS × Arch × Python |
| **qmvir** | setuptools (Python) | `.whl` (pure Python) | ❌ Universal |

---

## Target Matrix

### Hỗ trợ chính thức

| | x86_64 | ARM64 |
|---|---|---|
| **macOS 13+** | ✅ `macosx_13_0_x86_64` | ✅ `macosx_14_0_arm64` |
| **Linux glibc 2.35+** | ✅ `manylinux_2_35_x86_64` | ✅ `manylinux_2_35_aarch64` |
| **Windows 10+** | ✅ `win_amd64` | 🔶 Planned |

### Linux Distribution Compatibility

| glibc version | Distros |
|---|---|
| **2.35** (mặc định) | Ubuntu 22.04+, Debian 12+, Rocky 9+, Fedora 36+, Arch (rolling) |
| 2.31 (tùy chọn) | Ubuntu 20.04+, Debian 11+, Rocky 8+, CentOS Stream 8+ |

### SIMD Optimizations

| Arch | Instruction Set | Sử dụng trong |
|---|---|---|
| **x86_64** | AVX2 (runtime detect) + SSE4.2 fallback | JOIN, vector distance, aggregation |
| **ARM64** | NEON (always available) | JOIN, vector distance, aggregation |

---

## Build Locally

### Full build (tất cả components)

```bash
python scripts/build_release.py --clean
```

### Build từng component

```bash
# Chỉ qm_engine
python scripts/build_release.py --component engine

# Chỉ qm_native
python scripts/build_release.py --component native

# Chỉ qm_native_c
python scripts/build_release.py --component native_c

# Chỉ Python wheel
python scripts/build_release.py --component python
```

### Cross-compile

```bash
# Build cho Linux x86_64 từ macOS
python scripts/build_release.py --target x86_64-unknown-linux-gnu

# Build cho macOS Intel từ Apple Silicon
python scripts/build_release.py --target x86_64-apple-darwin
```

### Build + Install + Verify

```bash
python scripts/build_release.py --install --verify
```

Output:
```
═══ Built 4 wheel(s) ═══
  qm_engine-0.1.0-cp313-cp313-macosx_14_0_arm64.whl  (4.2 MB)
  qm_native-0.1.0-cp313-cp313-macosx_14_0_arm64.whl  (1.8 MB)
  qm_native_c-0.1.0-cp313-cp313-macosx_14_0_arm64.whl (0.1 MB)
  qmvir-1.0.0-py3-none-any.whl  (0.3 MB)

═══ Verification ═══
  ✓ qm_engine
  ✓ qm_native (available=True)
  ✓ qm_native_c
  ✓ qmvir
```

---

## CI/CD Pipeline

### `.github/workflows/release.yml`

**Trigger**: Git tag `v*` hoặc workflow_dispatch

```
build-engine (5 targets)     build-native (5 targets)     build-native-c (4 targets)
  ├─ macOS ARM64               ├─ macOS ARM64               ├─ macOS ARM64
  ├─ macOS x86_64              ├─ macOS x86_64              ├─ macOS x86_64
  ├─ Linux x86_64              ├─ Linux x86_64              ├─ Linux x86_64
  ├─ Linux ARM64 (cross)       ├─ Linux ARM64 (cross)       └─ Windows x86_64
  └─ Windows x86_64            └─ Windows x86_64
        │                           │                             │
        └───────────────────────────┴─────────────────────────────┤
                                                                  ▼
                                                          build-python (1)
                                                                  │
                                                                  ▼
                                                          build-docker (multi-arch)
                                                                  │
                                                                  ▼
                                                          test (4 platforms)
                                                            ├─ macOS 14 (ARM64)
                                                            ├─ macOS 13 (x86)
                                                            ├─ Ubuntu 22.04
                                                            └─ Windows
                                                                  │
                                                                  ▼
                                                              release
                                                          (GitHub Release + Docker)
```

### Release Artifacts

Mỗi release tạo ra:
- **5 platform bundles**: `qmvir-{platform}.tar.gz` chứa tất cả wheels cần thiết
- **~15 individual wheels**: từng component cho từng platform
- **Docker image**: `ghcr.io/<org>/qm:v1.0.0` (linux/amd64 + linux/arm64)

---

## Docker

### Sử dụng image có sẵn

```bash
# Chạy trực tiếp (auto-detect arm64/amd64)
docker run -d \
  -p 5433:5433 \
  -v qm-data:/data/qm \
  -e QM_ADMIN_PASSWORD=mysecret \
  ghcr.io/<org>/qm:latest

# Kết nối
psql -h localhost -p 5433 -U admin
```

### Build image locally

```bash
# Build tất cả wheels trước
python scripts/build_release.py --clean

# Build Docker image
docker build -f Dockerfile.release -t qmvir:local .
```

---

## Installation cho End Users

### Cách 1: pip install từ wheels (nhanh nhất)

```bash
# Download platform bundle
curl -L https://github.com/<org>/qm/releases/download/v1.0.0/qmvir-linux-x86_64.tar.gz | tar xz
pip install *.whl

# Chạy
qmvir start
```

### Cách 2: Docker

```bash
docker run -d -p 5433:5433 ghcr.io/<org>/qm:v1.0.0
```

### Cách 3: Build từ source

```bash
git clone https://github.com/<org>/qm.git && cd qm

# Yêu cầu: Rust toolchain, Python 3.11+, numpy
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
pip install maturin numpy

python scripts/build_release.py --install --verify
qmvir start
```

---

## Release Process

```bash
# 1. Cập nhật version
# pyproject.toml: version = "1.1.0"
# qm_engine/Cargo.toml: version = "1.1.0"
# qm_native/Cargo.toml: version = "1.1.0"

# 2. Commit + tag
git add -A && git commit -m "Release v1.1.0"
git tag v1.1.0
git push origin main --tags

# 3. CI tự động:
#    build → test → release → Docker push

# 4. Kết quả:
#    GitHub Release: https://github.com/<org>/qm/releases/tag/v1.1.0
#    Docker: ghcr.io/<org>/qm:v1.1.0
```

---

## Files

| File | Mục đích |
|---|---|
| `.github/workflows/release.yml` | CI: build 14+ targets, test, release, Docker |
| `scripts/build_release.py` | Local builder: 4 components → wheels |
| `Dockerfile.release` | Multi-arch Docker image với pre-built wheels |
| `docs/packaging.md` | Tài liệu này |
