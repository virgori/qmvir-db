# Handover: QMvir vs PostgreSQL fair benchmark (quizzman)

Tài liệu này cho agent/dev tiếp tục tối ưu **QMvir** (`qm_engine`) để thắng **PostgreSQL** trên fair `node-pg` benchmark tại server **quizzman**.

**Mục tiêu:** 15/15 workload vs PG (Redis là baseline riêng, không tính).  
**Nguyên tắc:** build trên **quizzman** (aarch64 Linux), không build local để deploy.  
**Không commit** trừ khi user yêu cầu rõ.

**Cách làm việc (bắt buộc):** không để AI “tự nghĩ” kiến trúc. Chỉ điều tra giả thuyết §7 theo thứ tự, dùng prompt §8, **profile trước khi sửa**, benchmark trước/sau, cấm regress workload đang thắng.

---

## 1. Môi trường

| Thành phần | Giá trị |
|------------|---------|
| Server | `quizzman` (SSH alias) |
| Arch | `aarch64-unknown-linux-gnu` |
| Source sync | local `/Users/gengyang/QM/qm_engine/` → remote `/root/qm-linux-bench/qm_engine/` |
| Binary deploy | `/opt/benchmark/node_modules/qmvir/bin/native/qm-linux-aarch64` |
| Data dir (bench) | `/opt/benchmark/qmvir_data_v623` |
| QM port | `55433` (user `admin` / password `admin`) |
| PG port | `5433` |
| Fair bench | `/opt/benchmark/full_bench.js` |
| Results JSON | `/opt/benchmark/full_bench_fair_results.json` |
| Micro scripts | `/tmp/qm_bench_micro.js`, `/tmp/qm_read_micro.js` |

Fair bench: **15 test vs PG**, session policy `writes=1 conn`, reads = fresh connection mỗi phase, Concurrent = pool 16 workers × 62 ops = 992 ops.

---

## 2. Sync / build / deploy / test (checklist bắt buộc)

### 2.1 Sync — tránh sync thiếu

Luôn rsync **cả tree `qm_engine`**, exclude `target/` và `.git/`:

```bash
rsync -az --delete \
  --exclude='target/' \
  --exclude='.git/' \
  /Users/gengyang/QM/qm_engine/ \
  quizzman:/root/qm-linux-bench/qm_engine/
```

**Lưu ý:**
- `--delete` xoá file remote đã xoá local → tránh binary/build “dính” file cũ.
- Trailing `/` ở source và dest: sync **nội dung** `qm_engine`, không tạo thêm cấp thư mục.
- **Đừng** chỉ scp 1–2 file rồi build — dễ quên `Cargo.toml`, bin probe, gateway sibling modules.
- Sau sync, nếu sửa nhiều file: verify nhanh trên remote:

```bash
ssh quizzman 'cd /root/qm-linux-bench/qm_engine && git status -sb 2>/dev/null; ls -la src/gateway/{stream,scram,connection,native_sql}.rs'
```

(Local repo có thể dirty; remote tree thường **không** phải git checkout đầy đủ — tin `rsync`, không tin “đã pull”. )

### 2.2 Build — luôn trên quizzman

```bash
ssh quizzman 'cd /root/qm-linux-bench/qm_engine && \
  cargo build --release \
    --target aarch64-unknown-linux-gnu \
    --no-default-features \
    --bin qm'
```

Artifact:  
`/root/qm-linux-bench/qm_engine/target/aarch64-unknown-linux-gnu/release/qm`

**Tránh cache / stale binary:**
1. Sau đổi code: **phải** `cargo build --release ...` lại trên remote (không copy binary local darwin).
2. Incremental compile thường ổn; nếu nghi ngờ stale:

```bash
ssh quizzman 'cd /root/qm-linux-bench/qm_engine && cargo clean -p qm_engine && cargo build --release --target aarch64-unknown-linux-gnu --no-default-features --bin qm'
```

3. Confirm mtime/size binary vừa build trước khi `cp` vào `/opt/benchmark/...`.

Local `cargo check` trên macOS có thể fail (`posix_fallocate` / Linux-only) — **không** dùng làm gate deploy.

### 2.3 Deploy — tránh “Text file busy”

Binary đang chạy thì `cp` fail (`Text file busy`). Luôn kill theo port **trước** khi copy:

```bash
ssh quizzman '
  fuser -k 55433/tcp 2>/dev/null || true
  sleep 1
  # fallback:
  ss -lntp | grep 55433 || echo "port free"
  cp /root/qm-linux-bench/qm_engine/target/aarch64-unknown-linux-gnu/release/qm \
     /opt/benchmark/node_modules/qmvir/bin/native/qm-linux-aarch64
  echo deployed
'
```

**Không** chỉ `pkill` mơ hồ rồi `cp` ngay — verify port free.

### 2.4 Chạy full fair bench

```bash
ssh quizzman 'cd /opt/benchmark && node full_bench.js > full_bench_<tag>_run.log 2>&1'
```

Bench tự `restartQmvir623(true)` (fresh data dir) trước phase QM.

Đọc kết quả (đừng đợi shell treo vì node có thể crash sau summary — xem mục bug):

```bash
ssh quizzman 'grep -A20 "TỔNG KẾT" /opt/benchmark/full_bench_<tag>_run.log'
```

Micro (Single INSERT + Concurrent Write), cần QM đang listen:

```bash
# start QM giống full_bench (hoặc để full_bench start), rồi:
QM_PORT=55433 node /tmp/qm_bench_micro.js
```

### 2.5 Fresh data / tránh OS + engine cache lệch so sánh

| Việc | Khi nào |
|------|---------|
| `restartQmvir623(true)` / `rm -rf qmvir_data_v623` | So sánh “cold” / sau đổi WAL layout |
| Fresh node-pg session mỗi read phase | Đã có trong `full_bench.js` — đừng “tối ưu” bằng reuse conn nếu phá fairness |
| Re-run bench ≥2 lần | Noise lớn (LIKE/Full Scan/Concurrent biến động mạnh) — lấy run ổn định, ghi rõ tag log |
| Đổi WAL sync policy / O_DSYNC | Luôn A/B trên quizzman; lần tắt O_DSYNC **đã regress** |

---

## 3. Tình hình hiện tại (2026-07-12)

### Scorecard — mới nhất: `full_bench_dml_noupcase_run.log` / `full_bench_wire_trim_run.log`

| Tag | Batch QM/PG | Conc Write QM/PG | Notes |
|-----|-------------|------------------|-------|
| adaptive_gc | 745/504 | 336/300 | Peak ~13/15 |
| no_upcase | 724/386 | 535/245 | |
| **dml_noupcase** | **561/367** | 602/257 | Best Batch; skip full-SQL uppercase for DML |
| wire_trim | 601/393 | 450/241 | Avoid Query String re-copy; Single INSERT mạnh |

**Profile Batch (ext4, không tmpfs):** engine `group_commit` ≈ **3.5ms/batch** (fsync); memory ≈ 0.3ms; wire node-pg ≈ 5.5–6ms; PG wire ≈ 3.5–4ms. → **WAL compress không đủ** (SQL ~13KB); gap còn lại chủ yếu wire/gateway trên nền fsync đã gần sàn PG.

**13 / 15 thắng PG**

| Test | PG (ms) | QM (ms) | |
|------|---------|---------|---|
| Single INSERT 10k | 17075 | **16767** | ✅ |
| Batch INSERT 100 literal | **504** | 744 | ❌ ~1.48× |
| UPDATE 1k | 2120 | **1907** | ✅ |
| DELETE 500 | 818 | **688** | ✅ |
| Point Lookup 1k | 552 | **361** | ✅ |
| Range Scan 100x | 228 | **158** | ✅ |
| LIKE %pat% 100x | 1116 | **454** | ✅ |
| Full Scan 10x | 647 | **452** | ✅ |
| COUNT 50x | 179 | **21** | ✅ |
| Filter 100x | 459 | **242** | ✅ |
| Vec INSERT 500 | 1777 | **1654** | ✅ |
| HNSW Build | 243 | **237** | ✅ |
| Vec Search 50x | 269 | **64** | ✅ |
| Concurrent Read 16W | 514 | **235** | ✅ |
| Concurrent Write 16W | **299** | 336 | ❌ ~1.12× |

Lịch sử sớm hơn (`full_bench_scram_run.log`): chỉ ~6/15. Các tối ưu TCP_NODELAY, WAL buffer + group commit, SCRAM 4096, prepared SELECT, LIKE LIMIT fix, adaptive group-commit đã kéo lên 13/15.

### Code đã thay (chưa commit, trong `qm_engine/`)

| File | Việc làm |
|------|----------|
| `src/gateway/stream.rs` | `TCP_NODELAY` on accept |
| `src/gateway/scram.rs` | Default iterations **4096** (PG-like); env `QMVIR_SCRAM_ITERATIONS` (min 4096) |
| `src/gateway/connection.rs` | Prepared Execute inline; read-only dùng compiled plan; Describe Portal ưu tiên plan |
| `src/gateway/native_sql.rs` | WAL không flush mỗi `append_sql`; group commit adaptive; InsertFast WAL prefix; LIKE prepared + LIMIT-aware fast path; SelectAll prepared; cell/param/vector parse hot paths |
| `src/bin/insert_hotpath_probe.rs` + `Cargo.toml` | Probe engine insert (không qua wire) |

**Không còn ưu tiên:** tắt O_DSYNC / chỉ `fdatasync` mặc định — đã thử trên quizzman → regress, đã revert.

---

## 4. Chậm ở đâu (bottleneck)

### Còn thua (ưu tiên)

1. **Batch INSERT (~1.5×)**  
   - Bench: `INSERT INTO t VALUES (...),(...),...` **literal SQL** (không prepared multi-row).  
   - Cost: parse multi-value groups + materialize `NativeRow` ×100 + **một WAL append chuỗi SQL rất lớn** + sync.  
   - Comment trong `full_bench.js`: QM từng vỡ parameterized multi-row extended protocol → bench cố ý dùng literal.

2. **Concurrent Write 16W (~1.12×)**  
   - Group commit đã giúp (764ms → ~336ms với adaptive window).  
   - Vẫn còn khoảng O_DSYNC / kích thước nhóm commit. Solo INSERT vs multi-writer trade-off nhạy:  
     - Sleep window cố định 100µs → giúp 16W, **phạt** Single INSERT.  
     - Adaptive (`pending > 1` rồi mới chờ) giữ Single INSERT thắng nhưng Concurrent Write vẫn sát thua PG.

### Đã hiểu / đã xử lý phần lớn

| Vấn đề | Ảnh hưởng | Fix |
|--------|-----------|-----|
| Nagle trên loopback | Vec INSERT / large payload ~45ms/op cảm giác | `set_nodelay(true)` |
| `flush()` mỗi WAL append + O_DSYNC | Phá group commit; Single INSERT ~3–4ms/op wire | Buffer append; sync ở group commit |
| SCRAM 600k iterations | Mỗi connect mới cực chậm; Concurrent Read thua | Default 4096 |
| Prepared SELECT re-parse | Point/range chậm hơn cần thiết | Compiled plan + Execute path |
| LIKE + LIMIT bỏ fast path | LIKE chậm ~1.2× | Cho phép LIMIT trên id-only / prepared LikeContains |

### Engine vs wire

Micro / probe: engine insert ~0.1–0.23ms/op. Wire Single INSERT trước tối ưu ~3.5–4.5ms/op (đồng bộ O_DSYNC). PG ~1.8ms/op order. Hiện Single INSERT đã **thắng sát** trên một số run — đừng phá solo path khi tối ưu 16W.

---

## 5. Bug / gotcha đã gặp

1. **`cp: Text file busy`** — process vẫn giữ binary; kill port 55433 trước.  
2. **Shell SSH “treo” sau khi log xong** — `full_bench.js` đôi khi ném `Unhandled 'error' event` / `Connection terminated unexpectedly` **sau** khi đã in `TỔNG KẾT`. Đọc log file, đừng tin exit code SSH.  
3. **QMvir 6.1.1 (55435)** thường `ECONNREFUSED` trong summary — bỏ qua.  
4. **Noise bench** — cùng binary, Full Scan / LIKE / Concurrent có thể lệch hàng trăm ms. Luôn tag log + so sánh ≥2 run.  
5. **Trusted bulk insert + index** — thử `trusted_series=true` cho batch literal từng khiến Concurrent Write / index maintenance regress; đã **revert**.  
6. **Group commit sleep cố định** — regress Single INSERT; dùng adaptive (chỉ chờ khi `pending > 1`).  
7. **O_DSYNC off** — regress trên ổ/đĩa quizzman; giữ mặc định hiện tại.  
8. **Parameterized multi-row INSERT** — vẫn là điểm yếu protocol/path; bench tránh bằng literal (đừng “sửa bench” để thắng — mục tiêu công bằng).  
9. Local compile macOS ≠ Linux aarch64.  
10. **Release rustc SIGKILL / OOM trên quizzman** — khi `available` RAM ~2–3Gi và có `llama-completion` / `ai-audio-worker` / cargo khác chạy song song, `cargo build --release` của `qm_engine` (codegen-units=1 + thin LTO) bị kernel kill. `cargo check --release` thường vẫn pass. **Không** set `RUSTFLAGS=-C codegen-units=N` tùy tiện — sẽ rebuild toàn bộ deps và làm OOM nặng hơn. Chờ máy rảnh (`available` ≳ 4–6Gi, không có rustc/llama nặng) rồi build lại.

---

## 6. Quy tắc làm việc: số liệu dẫn đường, không “tự nghĩ”

**Mục tiêu 15/15 → AI không được refactor lung tung.**  
Mỗi vòng phải:

1. Chọn **một giả thuyết** trong §7 (theo thứ tự ưu tiên).  
2. **Profile** để xác nhận hotspot (flamegraph / `perf` / counter nội bộ) **trước** khi sửa lớn.  
3. Một thay đổi hẹp, gắn giả thuyết.  
4. Benchmark trước/sau trên quizzman (`full_bench_<tag>_run.log`).  
5. **Từ chối** thay đổi nếu regress bất kỳ workload đang thắng (đặc biệt Single INSERT, UPDATE, DELETE, Concurrent Read, Vec Search).

**Không tối ưu lúc này** (đã thắng PG hoặc nguy cơ regress cao):

- Vector engine / HNSW query path  
- Query optimizer / planner chung  
- GC / compaction  
- Reader path (Point Lookup, Range, COUNT, Filter, LIKE, Full Scan)  
- Search / Join / Aggregation  
- Đổi `full_bench.js` để dễ thắng  
- Tắt durability / O_DSYNC mặc định  

**WAL:** đừng thắng bằng cách bỏ durable. Thay vào đó: compact header, compress/batch encode, **một** `write` + **một** fsync cho cả batch.

**Syscall mục tiêu (Batch INSERT 100 rows):**

```text
100 rows → 1 write() → 1 fdatasync/O_DSYNC
```

không phải `100 × write()`.

---

## 7. Giả thuyết bottleneck (xác suất cao) — theo thứ tự điều tra

### 7.1 Batch INSERT (~1.5×) — ưu tiên #1

Bench shape (`full_bench.js`): literal multi-row SQL, `BATCH_SIZE=100`:

```sql
INSERT INTO bench_text2_* VALUES (id,...),(id,...),...  -- 100 tuples
```

Entry: `handle_insert` → `parse_multi_value_groups` → `bulk_insert_rows_fast` → `wal_append(s)` (toàn bộ SQL string) → group commit sync.

| ID | Giả thuyết | Sao | Triệu chứng / chỗ nhìn trong code | Hướng tối ưu |
|----|------------|-----|-----------------------------------|--------------|
| **A** | Parse SQL lặp / nặng theo từng tuple | ⭐⭐⭐⭐⭐ | `parse_multi_value_groups` + `par_iter`/`parse_value` per cell; AST/string slice clone | Parse một lần; slice không clone ValueExpr; arena |
| **B** | Materialize Value quá nhiều lớp | ⭐⭐⭐⭐⭐ | `Literal → Cell → HashMap NativeRow → insert` | `Literal → TupleWriter` / SoA bulk; bỏ HashMap per row nếu được |
| **C** | WAL append / encode từng row hoặc dump SQL khổng lồ | ⭐⭐⭐⭐⭐ | Hiện: **một** `wal_append` với **cả** literal SQL batch (đúng 1 append, nhưng encode/copy string lớn) | Compact binary batch record; một append + một fsync; tránh re-render |
| **D** | Visibility / metadata cập nhật từng row | ⭐⭐⭐⭐ | HTAP track / row insert loop trong `with_write` | Vector/batch publish xmin nếu có |
| **E** | Index update từng row + rebalance | ⭐⭐⭐⭐ | Vòng `tree.insert` per row trong bulk path | Sort keys → bulk insert → rebalance once |
| **F** | Allocator: `String`/`Vec`/`HashMap`/`Box` mỗi row | ⭐⭐⭐⭐ | Mỗi cell `parse_value` → `Cell::Text(String)` | Arena / bump / SmallVec / reuse buffer |

**Fast path mong muốn (giống tinh thần PG):**

```text
Special INSERT parser → InsertExecutor / TupleWriter
(bỏ full planner / Logical→Physical cho multi-VALUES literal)
```

**Đã thử & cấm lặp lại mù:** `trusted_series=true` cho batch literal mà không bảo vệ index → regress Concurrent Write — đã revert.

### 7.2 Concurrent Write (~1.12×) — ưu tiên #2

Bench: 16 pool workers × 62 prepared single-row INSERT = 992 ops. Gap nhỏ → chỉ sửa chỗ contention nhỏ; **không** phá solo INSERT.

| ID | Giả thuyết | Sao | Ghi chú QM hiện tại | Hướng |
|----|------------|-----|---------------------|-------|
| **A** | WAL mutex toàn cục mỗi txn | ⭐⭐⭐⭐⭐ | `wal_writer` RwLock quanh append | Per-thread buffer → merge; giảm thời gian giữ lock |
| **B** | fsync từng txn thay vì group | ⭐⭐⭐⭐⭐ | Đã có `wal_group_commit_sync` + adaptive window (`pending > 1`) | Đo `group_commit_snapshot` (max_group_size, wait); tinh chỉnh notify/window **không** sleep cố định trên solo |
| **C** | Global allocator contention | ⭐⭐⭐⭐ | Nhiều String trên prepared path | TLS arena cho bind/WAL render |
| **D** | MVCC metadata atomics thừa | ⭐⭐⭐⭐ | HTAP track nếu bật | Batch publish; giảm atomic |
| **E** | Cache-line bouncing (page/txn state) | ⭐⭐⭐⭐ | Shared counters / table lock | Shard / partition metadata |

**Trade-off đã chứng minh bằng số:**

| Thử nghiệm | Concurrent Write | Single INSERT |
|------------|------------------|---------------|
| Sleep window 100µs cố định | Tốt hơn | **Regress** |
| Adaptive (`pending > 1` rồi chờ) | ~336 vs PG ~300 | Vẫn thắng sát |
| Tắt O_DSYNC | Regress tổng thể | — |

### 7.3 Các tầng pipeline — chỉ đụng khi profile chỉ vào đó

| Tầng | Việc đúng | Việc sai |
|------|-----------|----------|
| WAL | compress/compact/batch encode, single memcpy | Tắt durable để “thắng bench” |
| Parser | Fast-path INSERT multi-VALUES | Refactor toàn bộ SQL parser |
| Planner | Bypass optimizer cho INSERT đơn giản | Viết lại cost model |
| Executor | `Parser → TupleWriter` cho batch | Full AST→Logical→Physical cho mỗi batch |
| Memory | Giảm `clone`/`to_vec`/`String::from` trong hot loop | Đổi kiểu dữ liệu toàn engine |

---

## 8. Prompt giao AI (copy nguyên)

```text
Mục tiêu: đạt 15/15 benchmark thắng PostgreSQL.

Hiện trạng:
- Thắng: 13/15
- Thua:
  • Batch INSERT (~1.5×)
  • Concurrent Write (~1.12×)

Ưu tiên tuyệt đối:
1. Batch INSERT
2. Concurrent Write

Tập trung điều tra theo thứ tự:

Batch INSERT
- SQL parser có parse lặp theo từng tuple?
- AST/Expr có clone hoặc allocate dư thừa?
- Value -> Datum -> Tuple có thể rút gọn thành đường đi trực tiếp?
- WAL có ghi từng row thay vì một batch? (hoặc một append nhưng copy SQL literal khổng lồ?)
- Index có hỗ trợ bulk insert hay rebalance sau mỗi row?
- Có nhiều clone/String/Vec allocation trong vòng lặp?
- Có thể dùng arena hoặc bump allocator không?

Concurrent Write
- Có mutex toàn cục trên WAL?
- Có fsync cho từng transaction thay vì group commit?
- Có contention trên allocator hoặc MVCC metadata?
- Có cache-line bouncing trên page header hoặc transaction state?

Yêu cầu:
- Mỗi thay đổi phải kèm benchmark trước/sau trên quizzman (full_bench_<tag>_run.log).
- Không chấp nhận regress ở các workload đang thắng (đặc biệt Single INSERT / UPDATE / DELETE / Concurrent Read / Vec Search).
- Không refactor kiến trúc hoặc tối ưu module không liên quan nếu không có bằng chứng từ profiling.
- Nếu chưa xác định nguyên nhân: flamegraph/perf/counter trước khi sửa.
- Không tắt O_DSYNC / durability; không sửa full_bench.js để dễ thắng.
- Sync: rsync --delete cả qm_engine; build release aarch64 trên quizzman; kill :55433 rồi mới cp binary.
```

---

## 9. Workflow mỗi vòng tối ưu

```text
0. Chọn đúng 1 giả thuyết (§7) — ghi rõ A/B/C/...
1. Profile trên quizzman (perf record / flamegraph / native_profile counters) cho workload đó
2. Sửa hẹp đúng hotspot
3. rsync --delete (exclude target/.git)
4. cargo build --release --target aarch64-unknown-linux-gnu --no-default-features --bin qm
5. fuser -k 55433/tcp; sleep 1; cp binary → qmvir bin
6. node full_bench.js > full_bench_<hypothesis>_run.log
7. grep TỔNG KẾT — bảng 15 win/lose; so sánh với adaptive_gc baseline
8. Nếu Single INSERT hoặc bất kỳ win khác regress → revert ngay, không “bù” bằng sửa chỗ khác
```

Micro trước full khi đụng write path:

```bash
QM_PORT=55433 node /tmp/qm_bench_micro.js
# Single INSERT + Concurrent Write 16W nhanh; rồi mới full_bench
```

Profile gợi ý (trên quizzman, sau khi start QM + load):

```bash
# Ví dụ — gắn PID qm đang listen 55433, chạy batch micro song song
pid=$(ss -lntp | awk '/55433/ {print $NF}' | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1)
perf record -g -p "$pid" -- sleep 20   # trong lúc chạy batch insert micro
perf script | stackcollapse-perf.pl | flamegraph.pl > /tmp/qm_batch.svg
```

(Điều chỉnh tool path theo máy; quan trọng là **có** profile trước refactor lớn.)

---

## 10. Tham chiếu log trên quizzman

| Log | Ý nghĩa |
|-----|---------|
| `full_bench_adaptive_gc_run.log` | **Baseline 13/15** — adaptive GC |
| `full_bench_gcwin_run.log` | Sleep 100µs cố định — Concurrent tốt hơn, Single INSERT xấu |
| `full_bench_likefix_run.log` | Sau LIKE LIMIT / prepared Like |
| `full_bench_scram_run.log` | Baseline ~6/15 trước chuỗi tối ưu lớn |
| `full_bench_fair_results.json` | JSON snapshot lần chạy sau cùng |

---

## 11. Tóm tắt một dòng cho agent mới

> **13/15.** Chỉ còn Batch INSERT (~1.5×) và Concurrent Write (~1.12×). Làm theo giả thuyết §7 + prompt §8: profile → sửa hẹp → full_bench trước/sau; không đụng reader/vector/optimizer; không tắt durable. Sync/build/deploy theo §2. Số liệu dẫn đường — không tự nghĩ kiến trúc mới.
