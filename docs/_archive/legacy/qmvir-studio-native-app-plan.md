# QMvir Studio — Native Database Admin App

**Target**: Thay thế pgAdmin bằng app native hiệu suất cao  
**Version**: 3.0.0 &nbsp;|&nbsp; **Engine**: QMvir v2.0.0  
**Date**: 2026-04-02  
**Framework**: Tauri 2 (Rust backend) + SolidJS (reactive UI)

---

## 0. Tại sao pgAdmin chậm — và QMvir Studio sẽ nhanh hơn thế nào

### pgAdmin bottlenecks:

| Vấn đề | Nguyên nhân | pgAdmin | QMvir Studio |
|---|---|---|---|
| **Startup** | Electron + Python Flask server | 3–8s | <0.5s (native binary) |
| **RAM** | Chromium renderer + Python runtime | 400–800 MB | 50–80 MB (WebView OS-native) |
| **Query result render** | DOM re-render 10K+ rows via React | Lag at 5K rows | Virtualized table, 1M rows smooth |
| **Connection overhead** | TCP → psycopg2 → Python → JSON | ~5ms/query roundtrip | 0ms — in-process FFI |
| **Multi-tab** | Each tab = new Electron renderer | +100MB/tab | Shared WebView, +2MB/tab |
| **Packaging** | 200MB+ installer | Bloated | <15MB (Tauri) |

### Kiến trúc so sánh:

```
pgAdmin (chậm):                        QMvir Studio (nhanh):
┌─────────────┐                         ┌─────────────┐
│  Browser/    │                         │  WebView2   │  ← OS-native (WKWebView/WebView2)
│  Electron    │  400MB RAM              │  SolidJS    │  50MB RAM
│  React UI    │                         │  (reactive) │
├─────────────┤                         ├─────────────┤
│  Flask       │  Python GIL             │  Tauri Rust │  ← Zero-copy IPC
│  Server      │  JSON serialize         │  Commands   │  serde binary
├─────────────┤                         ├─────────────┤
│  psycopg2    │  TCP socket             │  Direct     │  ← In-process function call
│  TCP → PG    │  5ms latency            │  FFI link   │  0ms latency
├─────────────┤                         ├─────────────┤
│  PostgreSQL  │  Separate process       │  qm_engine  │  ← Embedded Rust library
│  Server      │                         │  (rlib)     │  Same process
└─────────────┘                         └─────────────┘
```

**Core advantage**: QMvir engine là Rust `rlib` — link trực tiếp vào Tauri
backend. Không có network hop, không có serialization overhead, không có Python GIL.
Query result truyền từ engine → UI qua Tauri IPC dưới dạng binary (serde), không
qua JSON text.

---

## 1. Technology Stack

### 1.1 Tại sao Tauri 2 + SolidJS (không phải Electron/React/Slint/egui)

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **Tauri 2 + SolidJS** | 15MB binary, 50MB RAM, OS-native WebView, Rust backend, reactive UI | WebView rendering không custom | ✅ **Chọn** |
| Electron + React | Familiar, rich ecosystem | 200MB+, 400MB+ RAM, slow | ❌ _Là pgAdmin 2.0_ |
| egui (pure Rust) | Zero dependencies, fast | UI primitive, no rich tables, hard to style | ❌ _Ugly for admin tool_ |
| Slint (Rust) | Declarative, compiled UI | Young ecosystem, limited widgets, table widget yếu | ❌ _Not ready_ |
| Dioxus | Rust-native, React-like | Less mature than Tauri, smaller community | 🟡 _Future option_ |
| GTK4 (gtk4-rs) | Native Linux look | macOS/Windows support kém, complex | ❌ |
| Qt (cxx-qt) | Mature, cross-platform | License issues, C++ bridge overhead | ❌ |

**SolidJS > React** vì:
- Không có Virtual DOM diffing (nhanh hơn 5-10x cho bảng lớn)
- Fine-grained reactivity — chỉ update DOM element thay đổi
- Bundle <10KB (React: 45KB+)
- Compiled, không cần runtime reconciler

### 1.2 Final Stack

```
┌── Frontend ──────────────────────────────┐
│  SolidJS 1.8        — UI framework       │
│  @tanstack/table     — Virtual table     │ ← Render 1M rows
│  @solid-primitives   — Utilities          │
│  Chart.js 4          — Performance charts │
│  Monaco Editor       — SQL editor         │ ← Same as VS Code
│  Tailwind CSS        — Styling            │
│  Vite                — Build tool          │
└──────────────────────────────────────────┘
            │ Tauri IPC (binary serde)
┌── Backend (Rust) ────────────────────────┐
│  tauri 2.x           — App framework     │
│  qm_engine (rlib)    — Database engine   │ ← Direct link
│  tokio               — Async runtime      │ ← Already in engine
│  serde               — Serialization      │ ← Already in engine
│  notify 6            — File watcher       │
│  open 5              — OS integration     │
└──────────────────────────────────────────┘
```

---

## 2. Feature Map — pgAdmin Parity + Beyond

### 2.1 Core Features (pgAdmin parity)

| # | Feature | pgAdmin | QMvir Studio | Priority |
|---|---|---|---|---|
| F1 | **Query Tool** — SQL editor with autocomplete, syntax highlight | ✅ | ✅ Monaco Editor | P0 |
| F2 | **Result Grid** — Tabular result with sort, filter, copy | ✅ | ✅ Virtual table (1M rows) | P0 |
| F3 | **Object Browser** — Tree view: databases → tables → columns → indexes | ✅ | ✅ Sidebar tree | P0 |
| F4 | **Table Viewer** — Browse/edit table data inline | ✅ | ✅ Editable grid | P0 |
| F5 | **DDL Viewer** — View CREATE TABLE statement | ✅ | ✅ | P1 |
| F6 | **Query History** — Past queries with re-run | ✅ | ✅ Persistent (SQLite) | P1 |
| F7 | **Explain/Analyze** — Query plan visualization | ✅ | ✅ + execution heatmap | P1 |
| F8 | **Dashboard** — Server stats, connections, throughput | ✅ | ✅ Real-time charts | P1 |
| F9 | **Import/Export** — CSV, SQL, Parquet | ✅ | ✅ + .qmbk native backup | P1 |
| F10 | **Multi-tab** — Multiple query tabs | ✅ | ✅ Lightweight tabs | P0 |

### 2.2 Beyond pgAdmin (QMvir-exclusive)

| # | Feature | Description | Priority |
|---|---|---|---|
| X1 | **Page Inspector** | Hexdump + parsed PageHeader, verify LZ4/Zstd | P1 |
| X2 | **WAL Viewer** | Browse WAL records with CRC status, LSN timeline | P1 |
| X3 | **Live Metrics** | Real-time throughput/latency charts (not polling — push via events) | P0 |
| X4 | **Backup Manager** | GUI for qm_backup/restore/verify/predict (from backup plan) | P2 |
| X5 | **Index Advisor** | Visualize index usage, suggest create/drop | P2 |
| X6 | **Schema Diff** | Side-by-side schema comparison between data dirs | P2 |
| X7 | **Buffer Pool Heatmap** | Visual cache hot/cold pages (W-TinyLFU frequency) | P2 |
| X8 | **Snapshot Timeline** | Visual LSN timeline with snapshot/WAL overlay | P2 |
| X9 | **In-process Mode** | Zero-latency: engine runs in same process | P0 |
| X10 | **Connection Mode** | Connect to remote QMvir via pgwire protocol | P1 |

---

## 3. Architecture Detail

### 3.1 Project Structure

```
qmvir-studio/
├── src-tauri/
│   ├── Cargo.toml              ← depends on qm_engine (path = "../qm_engine")
│   ├── src/
│   │   ├── main.rs             ← Tauri app entry
│   │   ├── commands/
│   │   │   ├── mod.rs          ← Tauri command registry
│   │   │   ├── query.rs        ← execute_sql, explain_query
│   │   │   ├── schema.rs       ← list_tables, table_detail, ddl_view
│   │   │   ├── metrics.rs      ← get_stats, subscribe_live_stats
│   │   │   ├── storage.rs      ← inspect_page, wal_records, snapshot_info
│   │   │   ├── backup.rs       ← backup, restore, verify, predict
│   │   │   ├── index.rs        ← list_indexes, create_index, advisor
│   │   │   └── connection.rs   ← manage connections (local/remote)
│   │   ├── state.rs            ← AppState: engine instances, connections
│   │   ├── events.rs           ← Tauri event emitters (live metrics push)
│   │   └── db.rs               ← Local SQLite for query history/settings
│   ├── tauri.conf.json
│   └── Cargo.lock
├── src/                         ← SolidJS frontend
│   ├── index.html
│   ├── App.tsx
│   ├── components/
│   │   ├── Sidebar.tsx          ← Object browser tree
│   │   ├── QueryEditor.tsx      ← Monaco SQL editor
│   │   ├── ResultGrid.tsx       ← Virtual table (TanStack)
│   │   ├── Dashboard.tsx        ← Live charts
│   │   ├── TableViewer.tsx      ← Browse/edit table data
│   │   ├── PageInspector.tsx    ← Hex view + PageHeader
│   │   ├── WalViewer.tsx        ← WAL record timeline
│   │   ├── BackupManager.tsx    ← Backup GUI
│   │   ├── IndexAdvisor.tsx     ← Index recommendations
│   │   └── SchemaCompare.tsx    ← Side-by-side diff
│   ├── stores/
│   │   ├── engine.ts            ← Reactive engine state
│   │   ├── query.ts             ← Query history, tabs
│   │   └── metrics.ts           ← Live metrics signal
│   ├── lib/
│   │   ├── tauri-bridge.ts      ← Type-safe invoke wrappers
│   │   └── formatters.ts        ← Cell/type formatters
│   └── styles/
│       └── global.css           ← Tailwind config
├── package.json
├── vite.config.ts
└── tsconfig.json
```

### 3.2 Tauri Backend — Command API

Mỗi Tauri command = một `#[tauri::command]` function trong Rust, gọi trực tiếp
vào `qm_engine` rlib.

```rust
// src-tauri/src/state.rs
use qm_engine::gateway::native_sql::NativeSqlEngine;
use qm_engine::metrics::MetricsRegistry;
use std::sync::Arc;
use parking_lot::RwLock;
use std::collections::HashMap;

pub struct AppState {
    /// Active engine instances (data_dir → engine)
    pub engines: RwLock<HashMap<String, Arc<EngineInstance>>>,
    /// App settings stored in local SQLite
    pub db: rusqlite::Connection,
}

pub struct EngineInstance {
    pub engine: NativeSqlEngine,
    pub metrics: MetricsRegistry,
    pub name: String,
    pub data_dir: String,
}
```

```rust
// src-tauri/src/commands/query.rs
use crate::state::AppState;
use serde::Serialize;

#[derive(Serialize)]
pub struct QueryResponse {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<serde_json::Value>>,  // JSON values
    pub row_count: usize,
    pub duration_us: u64,
    pub command_tag: String,
}

#[derive(Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub type_name: String,
    pub type_oid: i32,
}

#[tauri::command]
pub async fn execute_sql(
    state: tauri::State<'_, AppState>,
    connection_id: String,
    sql: String,
) -> Result<QueryResponse, String> {
    // Input validation
    if sql.len() > 1_000_000 { return Err("Query too long".to_string()); }
    
    let engines = state.engines.read();
    let instance = engines.get(&connection_id)
        .ok_or("Connection not found")?;
    
    let start = std::time::Instant::now();
    let result = instance.engine.execute(&sql)
        .map_err(|e| e.to_string())?;
    let duration_us = start.elapsed().as_micros() as u64;
    
    // Convert QueryResult → QueryResponse (type-safe JSON values)
    Ok(QueryResponse {
        columns: result.columns.iter().map(|(name, oid, _)| ColumnInfo {
            name: name.clone(),
            type_name: oid_to_type_name(*oid),
            type_oid: *oid,
        }).collect(),
        rows: convert_rows(&result),
        row_count: result.rows.len(),
        duration_us,
        command_tag: result.command_tag,
    })
}
```

```rust
// src-tauri/src/commands/schema.rs

#[derive(Serialize)]
pub struct TableInfo {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub row_count: usize,
    pub indexes: Vec<IndexInfo>,
    pub size_estimate: u64,
}

#[derive(Serialize)]
pub struct ColumnDef {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub position: usize,
}

#[tauri::command]
pub async fn list_tables(
    state: tauri::State<'_, AppState>,
    connection_id: String,
) -> Result<Vec<TableInfo>, String> {
    let engines = state.engines.read();
    let instance = engines.get(&connection_id)
        .ok_or("Connection not found")?;
    
    // Direct access to engine internals — no SQL parsing needed
    let tables = instance.engine.tables.read();
    let index_list = instance.engine.index_manager().list_indexes();
    
    Ok(tables.iter().map(|(name, t)| {
        let table_indexes: Vec<_> = index_list.iter()
            .filter(|(_, tbl, _, _, _)| tbl == name)
            .map(|(n, _, cols, state, uses)| IndexInfo {
                name: n.clone(), columns: cols.clone(),
                state: state.clone(), use_count: *uses,
            }).collect();
        
        TableInfo {
            name: name.clone(),
            columns: t.columns.iter().enumerate().map(|(i, col)| ColumnDef {
                name: col.clone(),
                type_name: format!("{:?}", t.column_types[i]),
                nullable: true,
                position: i,
            }).collect(),
            row_count: t.rows.len(),
            indexes: table_indexes,
            size_estimate: estimate_table_size(t),
        }
    }).collect())
}
```

```rust
// src-tauri/src/commands/metrics.rs

#[derive(Serialize, Clone)]
pub struct LiveStats {
    pub queries_per_sec: f64,
    pub inserts_per_sec: f64,
    pub cache_hit_rate: f64,
    pub active_txns: u64,
    pub wal_size_bytes: u64,
    pub total_rows: u64,
    pub table_count: usize,
    pub index_count: usize,
    pub query_latency_p50_ms: f64,
    pub query_latency_p99_ms: f64,
    pub uptime_secs: u64,
}

/// Push live stats to frontend every second via Tauri events
#[tauri::command]
pub async fn subscribe_live_stats(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    connection_id: String,
) -> Result<(), String> {
    let engines = state.engines.read();
    let instance = engines.get(&connection_id)
        .ok_or("Connection not found")?.clone();
    
    // Spawn background task to push metrics
    tokio::spawn(async move {
        let mut prev_queries = 0u64;
        let mut prev_inserts = 0u64;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            
            let m = &instance.metrics;
            let q = m.queries_total.get();
            let i = m.inserts_total.get();
            
            let stats = LiveStats {
                queries_per_sec: (q - prev_queries) as f64,
                inserts_per_sec: (i - prev_inserts) as f64,
                cache_hit_rate: compute_hit_rate(m),
                // ... fill all fields
            };
            
            prev_queries = q;
            prev_inserts = i;
            
            // Push to frontend (no polling!)
            let _ = app.emit("live-stats", &stats);
        }
    });
    
    Ok(())
}
```

```rust
// src-tauri/src/commands/storage.rs

#[derive(Serialize)]
pub struct PageInspection {
    pub page_id: u32,
    pub page_type: String,
    pub flags: u8,
    pub item_count: u16,
    pub free_space: u16,
    pub total_space: u16,
    pub free_pct: f64,
    pub checksum: u32,
    pub checksum_valid: bool,
    pub lsn: u64,
    pub prev_page: u32,
    pub next_page: u32,
    pub hex_dump: String,       // First 512 bytes as hex
    pub hex_ascii: String,      // ASCII printable view
}

#[tauri::command]
pub async fn inspect_page(
    state: tauri::State<'_, AppState>,
    connection_id: String,
    page_id: u32,
) -> Result<PageInspection, String> {
    // Read page from memmap, parse PageHeader, format hex dump
}

#[derive(Serialize)]
pub struct WalRecordView {
    pub lsn: u64,
    pub record_type: String,
    pub txn_id: u64,
    pub data_size: usize,
    pub crc_valid: bool,
    pub sql_preview: String,    // First 200 chars of SQL
}

#[tauri::command]
pub async fn list_wal_records(
    state: tauri::State<'_, AppState>,
    connection_id: String,
    offset: usize,
    limit: usize,
) -> Result<Vec<WalRecordView>, String> {
    // Read WAL file, decode records, paginate
}
```

### 3.3 Frontend — Key Components

#### Query Editor (Monaco + autocomplete)

```tsx
// src/components/QueryEditor.tsx
import { createSignal, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import * as monaco from "monaco-editor";

export function QueryEditor(props: { connectionId: string }) {
  let editorRef: HTMLDivElement;
  const [result, setResult] = createSignal<QueryResponse | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [duration, setDuration] = createSignal(0);
  
  onMount(() => {
    const editor = monaco.editor.create(editorRef, {
      language: "sql",
      theme: "vs-dark",
      minimap: { enabled: false },
      fontSize: 14,
      automaticLayout: true,
    });
    
    // Register QMvir-specific SQL completions
    monaco.languages.registerCompletionItemProvider("sql", {
      provideCompletionItems: async (model, position) => {
        // Fetch table/column names from engine
        const tables = await invoke("list_tables", { 
          connectionId: props.connectionId 
        });
        // Return completion items...
      }
    });
    
    // Ctrl+Enter / Cmd+Enter to execute
    editor.addAction({
      id: "execute-query",
      label: "Execute Query",
      keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter],
      run: () => executeQuery(editor.getValue()),
    });
  });
  
  async function executeQuery(sql: string) {
    setLoading(true);
    try {
      const res = await invoke<QueryResponse>("execute_sql", {
        connectionId: props.connectionId,
        sql,
      });
      setResult(res);
      setDuration(res.duration_us);
    } catch (e) {
      // Show error
    } finally {
      setLoading(false);
    }
  }
  
  return (
    <div class="flex flex-col h-full">
      <div ref={editorRef!} class="h-[300px] border-b" />
      <div class="text-xs text-gray-500 p-1">
        {duration()}μs • {result()?.row_count ?? 0} rows
      </div>
      <Show when={result()}>
        <ResultGrid data={result()!} />
      </Show>
    </div>
  );
}
```

#### Virtual Result Grid (1M rows without lag)

```tsx
// src/components/ResultGrid.tsx
import { createVirtualizer } from "@tanstack/solid-virtual";

export function ResultGrid(props: { data: QueryResponse }) {
  const ROW_HEIGHT = 32;
  let parentRef: HTMLDivElement;
  
  const virtualizer = createVirtualizer({
    get count() { return props.data.rows.length; },
    getScrollElement: () => parentRef,
    estimateSize: () => ROW_HEIGHT,
    overscan: 20,
  });
  
  return (
    <div ref={parentRef!} class="overflow-auto flex-1">
      {/* Header */}
      <div class="flex sticky top-0 bg-gray-800 z-10">
        <For each={props.data.columns}>
          {(col) => (
            <div class="px-3 py-2 font-semibold text-sm border-r border-gray-700 
                        min-w-[120px]">
              {col.name}
              <span class="text-gray-500 ml-1 text-xs">{col.type_name}</span>
            </div>
          )}
        </For>
      </div>
      
      {/* Virtualized body — only renders visible rows */}
      <div style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}>
        <For each={virtualizer.getVirtualItems()}>
          {(virtualRow) => {
            const row = props.data.rows[virtualRow.index];
            return (
              <div
                class="flex absolute w-full hover:bg-gray-750"
                style={{
                  height: `${ROW_HEIGHT}px`,
                  transform: `translateY(${virtualRow.start}px)`,
                }}
              >
                <For each={row}>
                  {(cell) => (
                    <div class="px-3 py-1 border-r border-gray-800 min-w-[120px]
                                text-sm truncate">
                      {cell === null ? <span class="text-gray-600">NULL</span> : String(cell)}
                    </div>
                  )}
                </For>
              </div>
            );
          }}
        </For>
      </div>
    </div>
  );
}
```

#### Live Dashboard (real-time push, no polling)

```tsx
// src/components/Dashboard.tsx
import { createSignal, onMount, onCleanup } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import Chart from "chart.js/auto";

export function Dashboard(props: { connectionId: string }) {
  const [stats, setStats] = createSignal<LiveStats | null>(null);
  const [history, setHistory] = createSignal<LiveStats[]>([]);
  let chartRef: HTMLCanvasElement;
  let chart: Chart;
  
  onMount(async () => {
    // Subscribe to push events (not polling!)
    const unlisten = await listen<LiveStats>("live-stats", (event) => {
      setStats(event.payload);
      setHistory(prev => [...prev.slice(-60), event.payload]); // Last 60 seconds
      updateChart();
    });
    
    // Tell backend to start pushing
    await invoke("subscribe_live_stats", { connectionId: props.connectionId });
    
    // Initialize chart
    chart = new Chart(chartRef, {
      type: "line",
      data: {
        labels: [],
        datasets: [
          { label: "Queries/s", borderColor: "#3b82f6", data: [] },
          { label: "Inserts/s", borderColor: "#10b981", data: [] },
        ]
      },
      options: {
        animation: false,  // No animation for real-time
        scales: { y: { beginAtZero: true } },
      }
    });
    
    onCleanup(() => unlisten());
  });
  
  function updateChart() {
    const h = history();
    chart.data.labels = h.map((_, i) => `${i}s`);
    chart.data.datasets[0].data = h.map(s => s.queries_per_sec);
    chart.data.datasets[1].data = h.map(s => s.inserts_per_sec);
    chart.update("none"); // No animation
  }
  
  return (
    <div class="grid grid-cols-4 gap-4 p-4">
      <StatCard label="Queries/s" value={stats()?.queries_per_sec ?? 0} />
      <StatCard label="Cache Hit" value={`${(stats()?.cache_hit_rate ?? 0).toFixed(1)}%`} />
      <StatCard label="Active TXN" value={stats()?.active_txns ?? 0} />
      <StatCard label="WAL Size" value={formatBytes(stats()?.wal_size_bytes ?? 0)} />
      
      <div class="col-span-4">
        <canvas ref={chartRef!} height={200} />
      </div>
      
      <StatCard label="Tables" value={stats()?.table_count ?? 0} />
      <StatCard label="Indexes" value={stats()?.index_count ?? 0} />
      <StatCard label="p50 Latency" value={`${stats()?.query_latency_p50_ms?.toFixed(1)}ms`} />
      <StatCard label="p99 Latency" value={`${stats()?.query_latency_p99_ms?.toFixed(1)}ms`} />
    </div>
  );
}
```

---

## 4. Performance Benchmarks — Target vs pgAdmin

| Metric | pgAdmin 8 | QMvir Studio (Target) | How |
|---|---|---|---|
| **Startup time** | 3–8s | <500ms | Native binary, no Python/Electron |
| **Memory (idle)** | 400MB | 50MB | OS WebView, no Chromium bundled |
| **Memory (10 tabs)** | 1.2GB | 70MB | Shared WebView renderer |
| **Query result (10K rows)** | 1.5s render | <50ms | Virtual table, only render viewport |
| **Query result (1M rows)** | Crash/freeze | <200ms | TanStack Virtual, constant memory |
| **Query roundtrip** | 5–15ms | <0.1ms | In-process FFI, no TCP |
| **Live metrics refresh** | 1s poll (HTTP) | Push events (0ms latency) | Tauri event system |
| **Installer size** | 200MB+ | <15MB | Tauri (no bundled Chromium) |
| **Cross-platform** | ✅ | ✅ macOS + Linux + Windows | Tauri 2 |

---

## 5. Dependency List

### 5.1 Rust (src-tauri/Cargo.toml)

```toml
[dependencies]
tauri = { version = "2", features = ["tray-icon", "dialog", "shell-open"] }
tauri-plugin-dialog = "2"
tauri-plugin-shell = "2"
tauri-plugin-fs = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }

# QMvir engine — linked directly as rlib
qm_engine = { path = "../../qm_engine" }

# Local state persistence
rusqlite = { version = "0.31", features = ["bundled"] }

# File watching (WAL changes, data dir)
notify = "6"
```

**Không cần thêm dep vào qm_engine** — tất cả đã có sẵn (tokio, serde, parking_lot, rayon).

### 5.2 Frontend (package.json)

```json
{
  "dependencies": {
    "solid-js": "^1.8",
    "@tauri-apps/api": "^2",
    "@tanstack/solid-virtual": "^3",
    "monaco-editor": "^0.45",
    "chart.js": "^4",
    "tailwindcss": "^3"
  },
  "devDependencies": {
    "vite": "^5",
    "vite-plugin-solid": "^2",
    "typescript": "^5",
    "@tauri-apps/cli": "^2"
  }
}
```

---

## 6. Implementation Phases

### Phase 0: Project Scaffold
| Task | LOC |
|---|---|
| `cargo tauri init` trong `qmvir-studio/` | — |
| Link `qm_engine` as path dependency | 5 |
| SolidJS + Vite + Tailwind setup | — |
| AppState struct + engine lifecycle | 80 |
| Verify `cargo tauri dev` builds & runs | — |

**Subtotal**: ~100 LOC Rust, ~50 LOC config

### Phase 1: Query Tool + Result Grid (P0 — core value)
| Task | File | LOC |
|---|---|---|
| `execute_sql` Tauri command | `commands/query.rs` | 80 |
| `list_tables` / `table_detail` commands | `commands/schema.rs` | 120 |
| Monaco SQL editor component | `QueryEditor.tsx` | 150 |
| Virtual result grid (TanStack) | `ResultGrid.tsx` | 120 |
| SQL autocomplete (table/column names) | `QueryEditor.tsx` | 60 |
| Query tabs (multi-tab support) | `App.tsx` | 80 |
| Type-safe Tauri bridge | `tauri-bridge.ts` | 60 |

**Subtotal**: ~670 LOC (200 Rust + 470 TS/TSX)

### Phase 2: Object Browser + Table Viewer
| Task | File | LOC |
|---|---|---|
| Sidebar tree component | `Sidebar.tsx` | 150 |
| Table → columns → indexes hierarchy | `Sidebar.tsx` | 80 |
| Table data viewer (browse rows) | `TableViewer.tsx` | 120 |
| Inline cell editing (UPDATE) | `TableViewer.tsx` | 100 |
| DDL view (CREATE TABLE statement) | `commands/schema.rs` | 40 |
| Row insert/delete from GUI | `TableViewer.tsx` | 60 |

**Subtotal**: ~550 LOC (40 Rust + 510 TS/TSX)

### Phase 3: Dashboard + Live Metrics
| Task | File | LOC |
|---|---|---|
| `subscribe_live_stats` push command | `commands/metrics.rs` | 100 |
| Dashboard layout + stat cards | `Dashboard.tsx` | 120 |
| Throughput chart (Chart.js) | `Dashboard.tsx` | 80 |
| Latency histogram | `Dashboard.tsx` | 60 |
| Cache hit rate gauge | `Dashboard.tsx` | 30 |
| WAL/storage status panel | `Dashboard.tsx` | 50 |

**Subtotal**: ~440 LOC (100 Rust + 340 TS/TSX)

### Phase 4: Storage Inspector (QMvir-exclusive)
| Task | File | LOC |
|---|---|---|
| `inspect_page` command | `commands/storage.rs` | 80 |
| `list_wal_records` command | `commands/storage.rs` | 80 |
| `snapshot_info` command | `commands/storage.rs` | 40 |
| Page inspector component (hex view) | `PageInspector.tsx` | 150 |
| WAL viewer (timeline + records) | `WalViewer.tsx` | 120 |
| Snapshot browser | `WalViewer.tsx` | 40 |

**Subtotal**: ~510 LOC (200 Rust + 310 TS/TSX)

### Phase 5: Backup Manager + Import/Export
| Task | File | LOC |
|---|---|---|
| Backup/restore commands (wrap backup module) | `commands/backup.rs` | 120 |
| Backup manager GUI | `BackupManager.tsx` | 200 |
| Import CSV/SQL/Parquet dialog | `BackupManager.tsx` | 100 |
| Progress bar (indicatif → Tauri event) | `BackupManager.tsx` | 60 |

**Subtotal**: ~480 LOC (120 Rust + 360 TS/TSX)

### Phase 6: Polish & Advanced Features
| Task | File | LOC |
|---|---|---|
| Query history (SQLite persistence) | `db.rs` + `QueryHistory.tsx` | 150 |
| Keyboard shortcuts (Cmd+E, Cmd+N, ...) | `App.tsx` | 40 |
| Dark/Light theme toggle | `styles/` | 30 |
| Index advisor GUI | `IndexAdvisor.tsx` | 100 |
| Schema diff viewer | `SchemaCompare.tsx` | 120 |
| Connection manager (local + remote) | `connection.rs` + `ConnectionDialog.tsx` | 150 |
| Error handling + notifications | `App.tsx` | 50 |

**Subtotal**: ~640 LOC

---

## 7. Total Estimates

| Metric | Value |
|---|---|
| **Rust backend (Tauri commands)** | ~760 LOC |
| **TypeScript/TSX frontend** | ~2,630 LOC |
| **Config files** | ~200 LOC |
| **Total new code** | ~3,590 LOC |
| **New Rust crates** | 5 (tauri, tauri-plugins, rusqlite, notify) |
| **New npm packages** | 6 (solid-js, tanstack, monaco, chart.js, tailwind, tauri-api) |
| **Installer size** | <15MB |
| **RAM usage** | ~50–80MB |
| **Build time** (first) | ~3 min (Tauri + qm_engine) |
| **Build time** (incremental) | ~10s |

---

## 8. Build & Distribution

### 8.1 Development

```bash
# One-time setup
cd qmvir-studio
npm install
cargo tauri dev    # Hot-reload: frontend + Rust backend
```

### 8.2 Release builds

```bash
# macOS (Universal: ARM + x86_64)
cargo tauri build --target universal-apple-darwin
# → qmvir-studio.dmg (~12MB)

# Linux (AppImage + .deb)
cargo tauri build --target x86_64-unknown-linux-gnu
# → qmvir-studio.AppImage (~14MB)
# → qmvir-studio.deb

# Windows (MSI + NSIS)
cargo tauri build --target x86_64-pc-windows-msvc
# → qmvir-studio.msi (~13MB)
```

### 8.3 Auto-update

Tauri 2 built-in updater — publish releases to GitHub, app checks on startup.

```json
// tauri.conf.json
{
  "plugins": {
    "updater": {
      "endpoints": ["https://github.com/user/qmvir-studio/releases/latest/download/latest.json"],
      "pubkey": "..."
    }
  }
}
```

---

## 9. So sánh QMvir Studio vs Alternatives

```
┌────────────────────┬──────────┬──────────┬──────────┬──────────┐
│                    │ pgAdmin  │ DBeaver  │ TablePlus│ QMvir    │
│                    │          │          │          │ Studio   │
├────────────────────┼──────────┼──────────┼──────────┼──────────┤
│ Startup            │ 5s       │ 8s       │ 1s       │ <0.5s   │
│ RAM (idle)         │ 400MB    │ 800MB    │ 100MB    │ 50MB    │
│ 1M row render      │ Crash    │ Slow     │ OK       │ <200ms  │
│ Query latency      │ 5-15ms   │ 3-10ms   │ 2-5ms    │ <0.1ms  │
│ Installer          │ 200MB    │ 150MB    │ 50MB     │ <15MB   │
│ Engine integration │ TCP only │ JDBC     │ Native   │ FFI     │
│ Page inspect       │ ❌       │ ❌       │ ❌       │ ✅      │
│ WAL viewer         │ ❌       │ ❌       │ ❌       │ ✅      │
│ Live push metrics  │ ❌       │ ❌       │ ❌       │ ✅      │
│ Backup GUI         │ Basic    │ ❌       │ ❌       │ ✅      │
│ License            │ Free     │ Free/Pro │ Paid     │ Free    │
│ Open source        │ ✅       │ Partial  │ ❌       │ ✅      │
└────────────────────┴──────────┴──────────┴──────────┴──────────┘
```

---

## 10. Risk Analysis

| Risk | Impact | Mitigation |
|---|---|---|
| Monaco Editor bundle size (3MB) | Larger installer | Lazy-load Monaco; or use CodeMirror 6 (200KB) |
| WebView rendering differences (macOS/Win/Linux) | UI inconsistencies | Test on all 3 platforms in CI |
| qm_engine rlib API changes break Studio | Build failures | Pin engine version; integration tests |
| Tauri 2 still evolving | Breaking changes | Pin exact Tauri version |
| SolidJS smaller ecosystem than React | Missing components | TanStack is framework-agnostic; Chart.js is vanilla |
| In-process engine crash → app crash | Data loss risk | Isolate engine in separate thread; auto-checkpoint |

---

## 11. Priority Roadmap

```
Month 1:  Phase 0 + Phase 1
          → Usable query tool (SQL editor + result grid + object browser)
          → "Better than psql" milestone

Month 2:  Phase 2 + Phase 3
          → Table viewer + live dashboard
          → "Better than basic pgAdmin" milestone

Month 3:  Phase 4 + Phase 5
          → Storage inspector + backup manager
          → "No other tool can do this" milestone

Month 4:  Phase 6 + polish
          → Query history, index advisor, schema diff
          → "Production release" milestone
```

---

## 12. Pre-Implementation Checklist

- [ ] Install Tauri 2 CLI: `cargo install tauri-cli --version "^2"`
- [ ] Install Node.js 20+ (for SolidJS build)
- [ ] Create `qmvir-studio/` directory alongside `qm_engine/`
- [ ] Verify `qm_engine` compiles as `rlib` (already in `crate-type = ["cdylib", "rlib"]`)
- [ ] Verify `cargo tauri init` scaffolds correctly
- [ ] Set up `qm_engine` as path dependency: `qm_engine = { path = "../../qm_engine" }`
- [ ] Run `cargo tauri dev` with empty window
- [ ] Verify SolidJS hot-reload works inside Tauri WebView

*Ready for implementation. Start with Phase 0 scaffold → Phase 1 Query Tool.*
