# Huong dan HTAP QMvir

HTAP (Hybrid Transaction/Analytical Processing) ket hop OLTP row-store voi column segment ben vung, giao dich MVCC, va planner chon duong dan.

## Kien truc

- **TableMvccStore** — chuoi version, visibility tren doc/ghi
- **TableStore** — heap row per-table (RwLock, khong clone catalog tren hot path)
- **column_segments/** — file QMCS mmap cho OLAP
- **HtapPlanner** — EXPLAIN + chon Index / Column / Row / HNSW

## Giao dich MVCC

| Lenh | Hanh vi |
|------|---------|
| `BEGIN` | Mo transaction thuc trong `TransactionManager` |
| `COMMIT` | Publish version, dong bo heap, columnize, ghi WAL archive |
| `ROLLBACK` | Huy version chua commit |

Autocommit (`INSERT`/`UPDATE`/`DELETE` khong co `BEGIN`) dung transaction ngam: BEGIN → DML → COMMIT.

## Duong dan truy van

Chay `EXPLAIN <sql>`:

| Duong dan | Khi nao |
|-----------|---------|
| **Index Scan** | `WHERE id = ?` |
| **Column Scan** | SUM/COUNT/GROUP BY/BETWEEN, ≥4096 dong, co column segment |
| **Seq Scan** | Mac dinh |
| **HNSW Vector Scan** | `ORDER BY col <-> query` |
| **GIN/Inverted Scan** | Full-text |

**Vector KNN khong doi** — HTAP khong thay doi cong thuc khoang cach; van dung HNSW/exact scan.

## Column segment ben vung

Sau moi `COMMIT`, columnizer ghi:

```
<data-dir>/column_segments/<table>/<column>/cseg_00000001.qmcs
```

`SELECT SUM/AVG/COUNT` doc mmap khi planner chon Column Scan.

## PITR (khoi phuc theo thoi diem)

```bash
qm --data-dir ./data backup -o snap.qmvb --pitr
qm --data-dir ./data pitr plan --timestamp 1719000000
qm --data-dir ./data pitr restore --timestamp 1719000000 -o ./pitr_out
```

## Chung nhan

```bash
qm --data-dir ./data htap certify
qm --data-dir ./data htap certify --isolation
```

`--isolation` chay bo kiem tra Jepsen-style: read-your-writes, rollback, commit, autocommit.

## Benchmark hon hop

```bash
python3 scripts/htap_mixed_benchmark.py --engine-bin qm --json
```

## Lenh CLI

```bash
qm guide htap
qm htap certify [--isolation]
qm pitr plan --timestamp T
qm pitr restore --timestamp T -o ./out
```

## Hieu nang

- Doc nong dung `table_read_guard()` thay vi `to_native_map()`.
- `to_native_map()` van dung cho FK, JOIN, transaction snapshot.
- Sau thay doi engine: `qm benchtest --profile quick`.

## Xử lý sự cố

| Trieu chung | Kiem tra |
|-------------|----------|
| Khong co Column Scan | Can ≥4096 dong + commit voi `data_dir` |
| PITR that bai | Can `wal_archive.json` sau commit |
| Vector lech ket qua | So sanh exact scan (bang nho hoac khong dung HNSW) |

Tai lieu tieng Anh: [HTAP_GUIDE.md](HTAP_GUIDE.md)
