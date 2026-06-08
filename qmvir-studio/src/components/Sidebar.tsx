import { Show, For, createSignal, onMount, onCleanup } from "solid-js";
import type { Component } from "solid-js";
import { connect, listTables, dropTable, tableDetail, exportCsv, importCsv, truncateTable, addColumn, dropColumn, renameTable, createIndex, dropIndex, executeSql } from "../lib/tauri-bridge";
import type { TableDetail, ColumnDef } from "../lib/tauri-bridge";
import {
  activeConnection,
  setActiveConnection,
  tables,
  setTables,
  selectedTable,
  setSelectedTable,
} from "../stores/engine";
import ConnectDialog from "./ConnectDialog";
import InsertRowDialog from "./InsertRowDialog";

interface ContextMenu {
  x: number;
  y: number;
  type: "table" | "column" | "index" | "background";
  tableName?: string;
  columnName?: string;
  indexName?: string;
}

const Sidebar: Component = () => {
  const [connecting, setConnecting] = createSignal(false);
  const [expanded, setExpanded] = createSignal<Record<string, boolean>>({});
  const [details, setDetails] = createSignal<Record<string, TableDetail>>({});
  const [contextMenu, setContextMenu] = createSignal<ContextMenu | null>(null);
  const [showCreateDialog, setShowCreateDialog] = createSignal(false);
  const [showConnectDialog, setShowConnectDialog] = createSignal(false);
  const [insertTarget, setInsertTarget] = createSignal<{ table: string; columns: ColumnDef[] } | null>(null);
  const [addColumnTarget, setAddColumnTarget] = createSignal<string | null>(null);
  const [createIndexTarget, setCreateIndexTarget] = createSignal<string | null>(null);

  onMount(async () => {
    try {
      setConnecting(true);
      const conn = await connect("default", undefined);
      setActiveConnection(conn);
    } catch (e) {
      console.error("Auto-connect failed:", e);
    } finally {
      setConnecting(false);
    }
  });

  // Close context menu on click elsewhere
  function handleGlobalClick() {
    setContextMenu(null);
  }
  onMount(() => document.addEventListener("click", handleGlobalClick));
  onCleanup(() => document.removeEventListener("click", handleGlobalClick));

  async function refreshTables() {
    const conn = activeConnection();
    if (!conn) return;
    try {
      const t = await listTables(conn.id);
      setTables(t);
    } catch (e) {
      console.error("Failed to list tables:", e);
    }
  }

  async function toggleExpand(name: string) {
    const wasExpanded = expanded()[name];
    setExpanded((prev) => ({ ...prev, [name]: !prev[name] }));
    if (!wasExpanded && !details()[name]) {
      const conn = activeConnection();
      if (!conn) return;
      try {
        const d = await tableDetail(conn.id, name);
        setDetails((prev) => ({ ...prev, [name]: d }));
      } catch (e) {
        console.error("Failed to get table detail:", e);
      }
    }
  }

  function handleContextMenu(e: MouseEvent, tableName: string) {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY, type: "table", tableName });
  }

  function handleColumnContextMenu(e: MouseEvent, tableName: string, columnName: string) {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY, type: "column", tableName, columnName });
  }

  function handleIndexContextMenu(e: MouseEvent, tableName: string, indexName: string) {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY, type: "index", tableName, indexName });
  }

  function handleBackgroundContextMenu(e: MouseEvent) {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY, type: "background" });
  }

  async function handleDropTable() {
    const menu = contextMenu();
    const conn = activeConnection();
    if (!menu?.tableName || !conn) return;
    if (!confirm(`Drop table "${menu.tableName}"? This cannot be undone.`)) return;
    try {
      await dropTable(conn.id, menu.tableName);
      await refreshTables();
      if (selectedTable() === menu.tableName) setSelectedTable(null);
    } catch (e: any) {
      alert("Drop failed: " + (typeof e === "string" ? e : e.message));
    }
    setContextMenu(null);
  }

  function handleSelectAll(tableName: string) {
    window.dispatchEvent(
      new CustomEvent("qmvir:executeQuery", {
        detail: `SELECT * FROM ${tableName};`,
      })
    );
    setContextMenu(null);
  }

  async function handleCountRows() {
    const menu = contextMenu();
    const conn = activeConnection();
    if (!menu?.tableName || !conn) return;
    try {
      const res = await executeSql(conn.id, `SELECT COUNT(*) AS count FROM ${menu.tableName}`);
      const count = res.rows.length > 0 ? res.rows[0][0] : 0;
      alert(`Table "${menu.tableName}" has ${count} row(s)`);
    } catch (e: any) {
      alert("Count failed: " + (typeof e === "string" ? e : e.message));
    }
    setContextMenu(null);
  }

  async function handleInsertRow() {
    const menu = contextMenu();
    const conn = activeConnection();
    if (!menu?.tableName || !conn) return;
    try {
      const d = await tableDetail(conn.id, menu.tableName);
      setInsertTarget({ table: menu.tableName, columns: d.columns });
    } catch (e: any) {
      alert("Cannot get table schema: " + (typeof e === "string" ? e : e.message));
    }
    setContextMenu(null);
  }

  function handleAddColumn() {
    const menu = contextMenu();
    if (!menu?.tableName) return;
    setAddColumnTarget(menu.tableName);
    setContextMenu(null);
  }

  function handleCreateIndex() {
    const menu = contextMenu();
    if (!menu?.tableName) return;
    setCreateIndexTarget(menu.tableName);
    setContextMenu(null);
  }

  async function handleTruncateTable() {
    const menu = contextMenu();
    const conn = activeConnection();
    if (!menu?.tableName || !conn) return;
    if (!confirm(`Truncate table "${menu.tableName}"? All rows will be deleted.`)) return;
    try {
      await truncateTable(conn.id, menu.tableName);
      await refreshTables();
      const d = await tableDetail(conn.id, menu.tableName);
      setDetails((prev) => ({ ...prev, [menu.tableName!]: d }));
    } catch (e: any) {
      alert("Truncate failed: " + (typeof e === "string" ? e : e.message));
    }
    setContextMenu(null);
  }

  async function handleRenameTable() {
    const menu = contextMenu();
    const conn = activeConnection();
    if (!menu?.tableName || !conn) return;
    const newName = prompt(`Rename "${menu.tableName}" to:`, menu.tableName);
    if (!newName || newName === menu.tableName) { setContextMenu(null); return; }
    try {
      await renameTable(conn.id, menu.tableName, newName);
      await refreshTables();
      if (selectedTable() === menu.tableName) setSelectedTable(newName);
    } catch (e: any) {
      alert("Rename failed: " + (typeof e === "string" ? e : e.message));
    }
    setContextMenu(null);
  }

  async function handleExportCsv() {
    const menu = contextMenu();
    const conn = activeConnection();
    if (!menu?.tableName || !conn) return;
    const path = prompt(`Export "${menu.tableName}" to CSV. Enter file path:`, `/tmp/${menu.tableName}.csv`);
    if (!path) { setContextMenu(null); return; }
    try {
      const count = await exportCsv(conn.id, menu.tableName, path);
      alert(`Exported ${count} rows to ${path}`);
    } catch (e: any) {
      alert("Export failed: " + (typeof e === "string" ? e : e.message));
    }
    setContextMenu(null);
  }

  async function handleImportCsv() {
    const menu = contextMenu();
    const conn = activeConnection();
    if (!menu?.tableName || !conn) return;
    const path = prompt(`Import CSV into "${menu.tableName}":`, `/tmp/${menu.tableName}.csv`);
    if (!path) { setContextMenu(null); return; }
    try {
      const count = await importCsv(conn.id, menu.tableName, path);
      alert(`Imported ${count} rows into ${menu.tableName}`);
      await refreshTables();
    } catch (e: any) {
      alert("Import failed: " + (typeof e === "string" ? e : e.message));
    }
    setContextMenu(null);
  }

  async function handleDropColumn() {
    const menu = contextMenu();
    const conn = activeConnection();
    if (!menu?.tableName || !menu.columnName || !conn) return;
    if (!confirm(`Drop column "${menu.columnName}" from "${menu.tableName}"?`)) return;
    try {
      await dropColumn(conn.id, menu.tableName, menu.columnName);
      const d = await tableDetail(conn.id, menu.tableName);
      setDetails((prev) => ({ ...prev, [menu.tableName!]: d }));
    } catch (e: any) {
      alert("Drop column failed: " + (typeof e === "string" ? e : e.message));
    }
    setContextMenu(null);
  }

  async function handleDropIndex() {
    const menu = contextMenu();
    const conn = activeConnection();
    if (!menu?.indexName || !conn) return;
    if (!confirm(`Drop index "${menu.indexName}"?`)) return;
    try {
      await dropIndex(conn.id, menu.indexName);
      if (menu.tableName) {
        const d = await tableDetail(conn.id, menu.tableName);
        setDetails((prev) => ({ ...prev, [menu.tableName!]: d }));
      }
    } catch (e: any) {
      alert("Drop index failed: " + (typeof e === "string" ? e : e.message));
    }
    setContextMenu(null);
  }

  return (
    <div class="w-56 bg-qm-surface border-r border-qm-border flex flex-col h-full overflow-hidden">
      {/* Header */}
      <div class="px-3 py-2 border-b border-qm-border flex items-center justify-between">
        <span class="text-sm font-semibold text-qm-accent">QMvir Studio</span>
        <div class="flex items-center gap-1">
          <Show when={activeConnection()}>
            <button
              onClick={() => setShowCreateDialog(true)}
              class="text-xs text-qm-muted hover:text-qm-accent"
              title="Create Table"
            >
              +
            </button>
            <button
              onClick={refreshTables}
              class="text-xs text-qm-muted hover:text-qm-text"
              title="Refresh"
            >
              ↻
            </button>
          </Show>
        </div>
      </div>

      {/* Connection status */}
      <div class="px-3 py-1.5 text-xs border-b border-qm-border">
        <Show when={activeConnection()} fallback={
          <div class="flex items-center justify-between">
            <span class="text-qm-muted">
              {connecting() ? "Connecting..." : "No connection"}
            </span>
            <button
              onClick={() => setShowConnectDialog(true)}
              class="text-xs text-qm-accent hover:underline"
            >
              Connect
            </button>
          </div>
        }>
          <div class="flex items-center justify-between mb-1">
            <div class="flex items-center gap-1">
              <span class="text-emerald-400">●</span>
              <span class="text-qm-text font-medium">{activeConnection()!.name}</span>
            </div>
            <button
              onClick={() => setShowConnectDialog(true)}
              class="text-xs text-qm-accent hover:underline"
            >
              Switch
            </button>
          </div>
          <div class="flex items-center gap-1 text-qm-muted/70">
            <span class="font-mono">{activeConnection()!.username ?? "admin"}</span>
            <span>@</span>
            <span class="font-mono">
              {activeConnection()!.conn_type === "remote"
                ? (activeConnection()!.host ?? "127.0.0.1") + ":" + (activeConnection()!.port ?? 55433)
                : "local"
              }
            </span>
          </div>
          <div class="flex items-center gap-1 text-qm-muted/50 mt-0.5">
            <span>Engine v{activeConnection()!.engine_version ?? "?"}</span>
            <Show when={activeConnection()!.data_dir}>
              <span class="text-qm-border">|</span>
              <span class="font-mono truncate max-w-[120px]" title={activeConnection()!.data_dir!}>
                {activeConnection()!.data_dir}
              </span>
            </Show>
            <Show when={!activeConnection()!.data_dir}>
              <span class="text-qm-border">|</span>
              <span class="italic">in-memory</span>
            </Show>
          </div>
        </Show>
      </div>

      {/* Tables tree */}
      <div class="flex-1 overflow-y-auto text-sm" onContextMenu={handleBackgroundContextMenu}>
        <div class="px-3 py-1.5 text-xs text-qm-muted uppercase tracking-wider">
          Tables ({tables().length})
        </div>
        <Show when={tables().length > 0} fallback={
          <div class="px-3 py-2 text-xs text-qm-muted italic">
            No tables. Run CREATE TABLE.
          </div>
        }>
          <For each={tables()}>
            {(table) => (
              <div>
                <button
                  class={`w-full text-left px-3 py-1 hover:bg-qm-border/30 flex items-center gap-1 ${
                    selectedTable() === table.name ? "bg-qm-accent/10 text-qm-accent" : "text-qm-text"
                  }`}
                  onClick={() => {
                    setSelectedTable(table.name);
                    toggleExpand(table.name);
                  }}
                  onContextMenu={(e) => handleContextMenu(e, table.name)}
                >
                  <span class="text-xs">{expanded()[table.name] ? "▼" : "▶"}</span>
                  <span class="font-mono text-xs">{table.name}</span>
                  <span class="ml-auto text-xs text-qm-muted">{table.row_count}</span>
                </button>
                {/* Expanded detail: columns + indexes */}
                <Show when={expanded()[table.name] && details()[table.name]}>
                  <div class="pl-7 pr-2 py-0.5 text-xs text-qm-muted">
                    <For each={details()[table.name]!.columns}>
                      {(col) => (
                        <div class="flex items-center gap-1 py-0.5 hover:bg-qm-border/20 rounded px-1 -mx-1 cursor-context-menu"
                          onContextMenu={(e: MouseEvent) => handleColumnContextMenu(e, table.name, col.name)}>
                          <span class="text-qm-accent/60">◦</span>
                          <span class="font-mono">{col.name}</span>
                          <span class="ml-auto text-qm-muted/70">{col.type_name}</span>
                        </div>
                      )}
                    </For>
                    <Show when={details()[table.name]!.indexes.length > 0}>
                      <div class="mt-1 pt-1 border-t border-qm-border/30">
                        <For each={details()[table.name]!.indexes}>
                          {(idx) => (
                            <div class="flex items-center gap-1 py-0.5 hover:bg-qm-border/20 rounded px-1 -mx-1 cursor-context-menu"
                              onContextMenu={(e: MouseEvent) => handleIndexContextMenu(e, table.name, idx.name)}>
                              <span class="text-yellow-500/60">⚡</span>
                              <span class="font-mono">{idx.name}</span>
                              <Show when={idx.unique}>
                                <span class="text-yellow-500 text-[10px]">UQ</span>
                              </Show>
                            </div>
                          )}
                        </For>
                      </div>
                    </Show>
                  </div>
                </Show>
              </div>
            )}
          </For>
        </Show>
      </div>

      {/* Context Menu */}
      <Show when={contextMenu()}>
        <div
          class="fixed z-50 bg-qm-surface border border-qm-border rounded shadow-lg py-1 min-w-[180px]"
          style={{ left: `${contextMenu()!.x}px`, top: `${contextMenu()!.y}px` }}
        >
          {/* Table context menu */}
          <Show when={contextMenu()!.type === "table"}>
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={() => handleSelectAll(contextMenu()!.tableName!)}>
              SELECT * FROM ...
            </button>
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={handleCountRows}>
              Count Rows
            </button>
            <div class="border-t border-qm-border my-0.5" />
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={handleInsertRow}>
              Insert Row...
            </button>
            <div class="border-t border-qm-border my-0.5" />
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={handleAddColumn}>
              Add Column...
            </button>
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={handleCreateIndex}>
              Create Index...
            </button>
            <div class="border-t border-qm-border my-0.5" />
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={handleTruncateTable}>
              Truncate Table
            </button>
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={handleRenameTable}>
              Rename Table...
            </button>
            <div class="border-t border-qm-border my-0.5" />
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={handleExportCsv}>
              Export CSV...
            </button>
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={handleImportCsv}>
              Import CSV...
            </button>
            <div class="border-t border-qm-border my-0.5" />
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-red-900/30 text-red-400"
              onClick={handleDropTable}>
              Drop Table
            </button>
          </Show>

          {/* Column context menu */}
          <Show when={contextMenu()!.type === "column"}>
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={() => { navigator.clipboard.writeText(contextMenu()!.columnName!); setContextMenu(null); }}>
              Copy Column Name
            </button>
            <div class="border-t border-qm-border my-0.5" />
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-red-900/30 text-red-400"
              onClick={handleDropColumn}>
              Drop Column
            </button>
          </Show>

          {/* Index context menu */}
          <Show when={contextMenu()!.type === "index"}>
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={() => { navigator.clipboard.writeText(contextMenu()!.indexName!); setContextMenu(null); }}>
              Copy Index Name
            </button>
            <div class="border-t border-qm-border my-0.5" />
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-red-900/30 text-red-400"
              onClick={handleDropIndex}>
              Drop Index
            </button>
          </Show>

          {/* Background context menu */}
          <Show when={contextMenu()!.type === "background"}>
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={() => { setShowCreateDialog(true); setContextMenu(null); }}>
              Create Table...
            </button>
            <div class="border-t border-qm-border my-0.5" />
            <button class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
              onClick={() => { refreshTables(); setContextMenu(null); }}>
              Refresh
            </button>
          </Show>
        </div>
      </Show>

      {/* Create Table Dialog */}
      <Show when={showCreateDialog()}>
        <CreateTableDialog
          onClose={() => setShowCreateDialog(false)}
          onCreated={refreshTables}
        />
      </Show>

      {/* Connect Dialog */}
      <Show when={showConnectDialog()}>
        <ConnectDialog onClose={() => setShowConnectDialog(false)} />
      </Show>

      {/* Insert Row Dialog */}
      <Show when={insertTarget()}>
        <InsertRowDialog
          tableName={insertTarget()!.table}
          columns={insertTarget()!.columns}
          onClose={() => setInsertTarget(null)}
          onInserted={() => {
            // Re-run select on the table
            window.dispatchEvent(
              new CustomEvent("qmvir:executeQuery", {
                detail: `SELECT * FROM ${insertTarget()!.table};`,
              })
            );
          }}
        />
      </Show>

      {/* Add Column Dialog */}
      <Show when={addColumnTarget()}>
        <AddColumnDialog
          tableName={addColumnTarget()!}
          onClose={() => setAddColumnTarget(null)}
          onAdded={async () => {
            const conn = activeConnection();
            const tbl = addColumnTarget();
            if (!conn || !tbl) return;
            try {
              const d = await tableDetail(conn.id, tbl);
              setDetails((prev) => ({ ...prev, [tbl]: d }));
            } catch {}
          }}
        />
      </Show>

      {/* Create Index Dialog */}
      <Show when={createIndexTarget()}>
        <CreateIndexDialog
          tableName={createIndexTarget()!}
          columns={details()[createIndexTarget()!]?.columns ?? []}
          onClose={() => setCreateIndexTarget(null)}
          onCreated={async () => {
            const conn = activeConnection();
            const tbl = createIndexTarget();
            if (!conn || !tbl) return;
            try {
              const d = await tableDetail(conn.id, tbl);
              setDetails((prev) => ({ ...prev, [tbl]: d }));
            } catch {}
          }}
        />
      </Show>
    </div>
  );
};

// ── Inline Create Table Dialog ──
import { createTable } from "../lib/tauri-bridge";
import type { NewColumn } from "../lib/tauri-bridge";

const CreateTableDialog: Component<{ onClose: () => void; onCreated: () => void }> = (props) => {
  const [tableName, setTableName] = createSignal("");
  const [columns, setColumns] = createSignal<NewColumn[]>([{ name: "", type_name: "TEXT" }]);
  const [error, setError] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);

  function addColumn() {
    setColumns([...columns(), { name: "", type_name: "TEXT" }]);
  }

  function removeColumn(idx: number) {
    setColumns(columns().filter((_, i) => i !== idx));
  }

  function updateColumn(idx: number, field: "name" | "type_name", value: string) {
    setColumns(columns().map((c, i) => (i === idx ? { ...c, [field]: value } : c)));
  }

  async function handleCreate() {
    const conn = activeConnection();
    if (!conn) return;
    const name = tableName().trim();
    if (!name) { setError("Table name required"); return; }
    const cols = columns().filter((c) => c.name.trim());
    if (cols.length === 0) { setError("At least one column required"); return; }

    setCreating(true);
    setError(null);
    try {
      await createTable(conn.id, name, cols);
      props.onCreated();
      props.onClose();
    } catch (e: any) {
      setError(typeof e === "string" ? e : e.message || String(e));
    } finally {
      setCreating(false);
    }
  }

  const types = ["INT", "BIGINT", "FLOAT", "DOUBLE", "TEXT", "VARCHAR", "BOOLEAN", "TIMESTAMP"];

  return (
    <div class="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={props.onClose}>
      <div class="bg-qm-surface border border-qm-border rounded-lg p-4 w-96 max-h-[80vh] overflow-y-auto" onClick={(e) => e.stopPropagation()}>
        <h3 class="text-sm font-semibold text-qm-accent mb-3">Create Table</h3>

        <input
          type="text"
          placeholder="Table name"
          value={tableName()}
          onInput={(e) => setTableName(e.currentTarget.value)}
          class="w-full bg-qm-bg text-qm-text px-2 py-1.5 rounded border border-qm-border text-xs font-mono mb-3 focus:outline-none focus:ring-1 focus:ring-qm-accent/50"
        />

        <div class="text-xs text-qm-muted mb-1">Columns</div>
        <For each={columns()}>
          {(col, idx) => (
            <div class="flex gap-1 mb-1">
              <input
                type="text"
                placeholder="name"
                value={col.name}
                onInput={(e) => updateColumn(idx(), "name", e.currentTarget.value)}
                class="flex-1 bg-qm-bg text-qm-text px-2 py-1 rounded border border-qm-border text-xs font-mono focus:outline-none"
              />
              <select
                value={col.type_name}
                onChange={(e) => updateColumn(idx(), "type_name", e.currentTarget.value)}
                class="bg-qm-bg text-qm-text px-1 py-1 rounded border border-qm-border text-xs"
              >
                <For each={types}>{(t) => <option value={t}>{t}</option>}</For>
              </select>
              <button
                onClick={() => removeColumn(idx())}
                class="text-red-400 hover:text-red-300 text-xs px-1"
                title="Remove"
              >
                ✕
              </button>
            </div>
          )}
        </For>

        <button onClick={addColumn} class="text-xs text-qm-accent hover:underline mb-3">
          + Add Column
        </button>

        <Show when={error()}>
          <div class="text-xs text-red-400 mb-2">{error()}</div>
        </Show>

        <div class="flex gap-2 justify-end">
          <button onClick={props.onClose} class="px-3 py-1 text-xs text-qm-muted hover:text-qm-text">
            Cancel
          </button>
          <button
            onClick={handleCreate}
            disabled={creating()}
            class="px-3 py-1 bg-qm-accent text-white text-xs rounded hover:bg-blue-600 disabled:opacity-50"
          >
            {creating() ? "Creating..." : "Create"}
          </button>
        </div>
      </div>
    </div>
  );
};

// ── Inline Add Column Dialog ──
const AddColumnDialog: Component<{
  tableName: string;
  onClose: () => void;
  onAdded: () => void;
}> = (props) => {
  const [colName, setColName] = createSignal("");
  const [colType, setColType] = createSignal("TEXT");
  const [error, setError] = createSignal<string | null>(null);
  const [adding, setAdding] = createSignal(false);

  const types = ["INT", "BIGINT", "FLOAT", "DOUBLE", "TEXT", "VARCHAR", "BOOLEAN", "TIMESTAMP"];

  async function handleAdd() {
    const conn = activeConnection();
    if (!conn) return;
    const name = colName().trim();
    if (!name) { setError("Column name required"); return; }
    setAdding(true);
    setError(null);
    try {
      await addColumn(conn.id, props.tableName, name, colType());
      props.onAdded();
      props.onClose();
    } catch (e: any) {
      setError(typeof e === "string" ? e : e.message || String(e));
    } finally {
      setAdding(false);
    }
  }

  return (
    <div class="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={props.onClose}>
      <div class="bg-qm-surface border border-qm-border rounded-lg p-4 w-80" onClick={(e) => e.stopPropagation()}>
        <h3 class="text-sm font-semibold text-qm-accent mb-3">Add Column to {props.tableName}</h3>
        <input
          type="text"
          placeholder="Column name"
          value={colName()}
          onInput={(e) => setColName(e.currentTarget.value)}
          class="w-full bg-qm-bg text-qm-text px-2 py-1.5 rounded border border-qm-border text-xs font-mono mb-2 focus:outline-none focus:ring-1 focus:ring-qm-accent/50"
        />
        <select
          value={colType()}
          onChange={(e) => setColType(e.currentTarget.value)}
          class="w-full bg-qm-bg text-qm-text px-2 py-1.5 rounded border border-qm-border text-xs mb-3"
        >
          <For each={types}>{(t) => <option value={t}>{t}</option>}</For>
        </select>
        <Show when={error()}>
          <div class="text-xs text-red-400 mb-2">{error()}</div>
        </Show>
        <div class="flex gap-2 justify-end">
          <button onClick={props.onClose} class="px-3 py-1 text-xs text-qm-muted hover:text-qm-text">Cancel</button>
          <button onClick={handleAdd} disabled={adding()}
            class="px-3 py-1 bg-qm-accent text-white text-xs rounded hover:bg-blue-600 disabled:opacity-50">
            {adding() ? "Adding..." : "Add Column"}
          </button>
        </div>
      </div>
    </div>
  );
};

// ── Inline Create Index Dialog ──
const CreateIndexDialog: Component<{
  tableName: string;
  columns: ColumnDef[];
  onClose: () => void;
  onCreated: () => void;
}> = (props) => {
  const [indexName, setIndexName] = createSignal("");
  const [selectedCols, setSelectedCols] = createSignal<string[]>([]);
  const [unique, setUnique] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);

  function toggleCol(name: string) {
    setSelectedCols((prev) =>
      prev.includes(name) ? prev.filter((c) => c !== name) : [...prev, name]
    );
  }

  async function handleCreate() {
    const conn = activeConnection();
    if (!conn) return;
    const name = indexName().trim();
    if (!name) { setError("Index name required"); return; }
    if (selectedCols().length === 0) { setError("Select at least one column"); return; }
    setCreating(true);
    setError(null);
    try {
      await createIndex(conn.id, props.tableName, name, selectedCols(), unique());
      props.onCreated();
      props.onClose();
    } catch (e: any) {
      setError(typeof e === "string" ? e : e.message || String(e));
    } finally {
      setCreating(false);
    }
  }

  return (
    <div class="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={props.onClose}>
      <div class="bg-qm-surface border border-qm-border rounded-lg p-4 w-80" onClick={(e) => e.stopPropagation()}>
        <h3 class="text-sm font-semibold text-qm-accent mb-3">Create Index on {props.tableName}</h3>
        <input
          type="text"
          placeholder="Index name (e.g. idx_users_name)"
          value={indexName()}
          onInput={(e) => setIndexName(e.currentTarget.value)}
          class="w-full bg-qm-bg text-qm-text px-2 py-1.5 rounded border border-qm-border text-xs font-mono mb-2 focus:outline-none focus:ring-1 focus:ring-qm-accent/50"
        />
        <div class="text-xs text-qm-muted mb-1">Columns:</div>
        <div class="mb-2 max-h-32 overflow-y-auto border border-qm-border rounded">
          <For each={props.columns}>
            {(col) => (
              <label class="flex items-center gap-2 px-2 py-1 text-xs hover:bg-qm-border/20 cursor-pointer">
                <input
                  type="checkbox"
                  checked={selectedCols().includes(col.name)}
                  onChange={() => toggleCol(col.name)}
                />
                <span class="font-mono">{col.name}</span>
                <span class="text-qm-muted/70 ml-auto">{col.type_name}</span>
              </label>
            )}
          </For>
        </div>
        <label class="flex items-center gap-2 text-xs text-qm-text mb-3 cursor-pointer">
          <input type="checkbox" checked={unique()} onChange={(e) => setUnique(e.currentTarget.checked)} />
          Unique index
        </label>
        <Show when={error()}>
          <div class="text-xs text-red-400 mb-2">{error()}</div>
        </Show>
        <div class="flex gap-2 justify-end">
          <button onClick={props.onClose} class="px-3 py-1 text-xs text-qm-muted hover:text-qm-text">Cancel</button>
          <button onClick={handleCreate} disabled={creating()}
            class="px-3 py-1 bg-qm-accent text-white text-xs rounded hover:bg-blue-600 disabled:opacity-50">
            {creating() ? "Creating..." : "Create Index"}
          </button>
        </div>
      </div>
    </div>
  );
};

export default Sidebar;
