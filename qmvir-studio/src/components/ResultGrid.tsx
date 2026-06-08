import { For, Show, createSignal, onMount, onCleanup } from "solid-js";
import type { Component } from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import type { QueryResponse } from "../lib/tauri-bridge";
import { updateCell, deleteRow } from "../lib/tauri-bridge";
import { activeConnection, selectedTable } from "../stores/engine";

const ROW_HEIGHT = 28;

interface EditingCell {
  rowIdx: number;
  colIdx: number;
  value: string;
}

interface RowContextMenu {
  x: number;
  y: number;
  rowIdx: number;
}

const ResultGrid: Component<{ data: QueryResponse }> = (props) => {
  let parentRef: HTMLDivElement | undefined;
  const [sortCol, setSortCol] = createSignal<number | null>(null);
  const [sortAsc, setSortAsc] = createSignal(true);
  const [editing, setEditing] = createSignal<EditingCell | null>(null);
  const [rowMenu, setRowMenu] = createSignal<RowContextMenu | null>(null);

  // Close row context menu on click elsewhere
  function handleGlobalClick() { setRowMenu(null); }
  onMount(() => document.addEventListener("click", handleGlobalClick));
  onCleanup(() => document.removeEventListener("click", handleGlobalClick));

  function sortedRows() {
    const col = sortCol();
    if (col === null) return props.data.rows;
    const asc = sortAsc();
    return [...props.data.rows].sort((a, b) => {
      const va = a[col];
      const vb = b[col];
      if (va === null && vb === null) return 0;
      if (va === null) return 1;
      if (vb === null) return -1;
      if (typeof va === "number" && typeof vb === "number") {
        return asc ? va - vb : vb - va;
      }
      const sa = String(va);
      const sb = String(vb);
      return asc ? sa.localeCompare(sb) : sb.localeCompare(sa);
    });
  }

  function handleHeaderClick(idx: number) {
    if (sortCol() === idx) {
      setSortAsc(!sortAsc());
    } else {
      setSortCol(idx);
      setSortAsc(true);
    }
  }

  const virtualizer = createVirtualizer({
    get count() {
      return sortedRows().length;
    },
    getScrollElement: () => parentRef!,
    estimateSize: () => ROW_HEIGHT,
    overscan: 20,
  });

  function formatCell(val: string | number | null): string {
    if (val === null) return "NULL";
    return String(val);
  }

  function handleDoubleClick(rowIdx: number, colIdx: number) {
    const row = sortedRows()[rowIdx];
    const val = row[colIdx];
    setEditing({ rowIdx, colIdx, value: val === null ? "" : String(val) });
  }

  async function commitEdit() {
    const ed = editing();
    if (!ed) return;
    const conn = activeConnection();
    const table = selectedTable();
    if (!conn || !table || props.data.columns.length === 0) {
      setEditing(null);
      return;
    }

    // Use first column as PK for the WHERE clause
    const pkCol = props.data.columns[0].name;
    const row = sortedRows()[ed.rowIdx];
    const pkVal = row[0] === null ? "" : String(row[0]);
    const targetCol = props.data.columns[ed.colIdx].name;

    try {
      await updateCell(conn.id, table, pkCol, pkVal, targetCol, ed.value);
    } catch (e) {
      console.error("Update failed:", e);
    }
    setEditing(null);
  }

  function handleEditKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      commitEdit();
    }
    if (e.key === "Escape") {
      setEditing(null);
    }
  }

  function handleRowContextMenu(e: MouseEvent, rowIdx: number) {
    if (!selectedTable()) return;
    e.preventDefault();
    e.stopPropagation();
    setRowMenu({ x: e.clientX, y: e.clientY, rowIdx });
  }

  async function handleDeleteRow() {
    const menu = rowMenu();
    const conn = activeConnection();
    const table = selectedTable();
    if (!menu || !conn || !table || props.data.columns.length === 0) {
      setRowMenu(null);
      return;
    }
    const pkCol = props.data.columns[0].name;
    const row = sortedRows()[menu.rowIdx];
    const pkVal = row[0] === null ? "" : String(row[0]);
    if (!confirm(`Delete row where ${pkCol} = ${pkVal}?`)) {
      setRowMenu(null);
      return;
    }
    try {
      await deleteRow(conn.id, table, pkCol, pkVal);
      // Trigger re-query
      window.dispatchEvent(
        new CustomEvent("qmvir:executeQuery", {
          detail: `SELECT * FROM ${table};`,
        })
      );
    } catch (e) {
      console.error("Delete failed:", e);
    }
    setRowMenu(null);
  }

  return (
    <div class="flex flex-col h-full">
      {/* Header row */}
      <div class="flex bg-qm-surface border-b border-qm-border sticky top-0 z-10">
        <div class="w-12 flex-shrink-0 px-2 py-1 text-xs text-qm-muted border-r border-qm-border">
          #
        </div>
        <For each={props.data.columns}>
          {(col, idx) => (
            <div
              class="min-w-[120px] flex-1 px-2 py-1 text-xs font-semibold border-r
                     border-qm-border cursor-pointer hover:bg-qm-border/30
                     select-none flex items-center gap-1"
              onClick={() => handleHeaderClick(idx())}
            >
              <span>{col.name}</span>
              <Show when={sortCol() === idx()}>
                <span class="text-qm-accent">{sortAsc() ? "↑" : "↓"}</span>
              </Show>
            </div>
          )}
        </For>
      </div>

      {/* Virtualized body */}
      <div ref={parentRef} class="flex-1 overflow-auto">
        <div
          style={{
            height: `${virtualizer.getTotalSize()}px`,
            position: "relative",
          }}
        >
          <For each={virtualizer.getVirtualItems()}>
            {(virtualRow) => {
              const row = sortedRows()[virtualRow.index];
              return (
                <div
                  class="flex absolute w-full hover:bg-qm-border/20"
                  style={{
                    height: `${ROW_HEIGHT}px`,
                    transform: `translateY(${virtualRow.start}px)`,
                  }}
                  onContextMenu={(e) => handleRowContextMenu(e, virtualRow.index)}
                >
                  <div class="w-12 flex-shrink-0 px-2 py-0.5 text-xs text-qm-muted
                              border-r border-qm-border font-mono flex items-center">
                    {virtualRow.index + 1}
                  </div>
                  <For each={row}>
                    {(cell, colIdx) => {
                      const isNull = cell === null;
                      const isEditing = () => {
                        const ed = editing();
                        return ed !== null && ed.rowIdx === virtualRow.index && ed.colIdx === colIdx();
                      };
                      return (
                        <div
                          class={`min-w-[120px] flex-1 px-2 py-0.5 text-xs border-r
                                  border-qm-border/50 truncate font-mono flex items-center
                                  ${isNull ? "text-qm-muted italic" : "text-qm-text"}
                                  ${selectedTable() ? "cursor-pointer" : ""}`}
                          onDblClick={() => selectedTable() && handleDoubleClick(virtualRow.index, colIdx())}
                        >
                          <Show when={isEditing()} fallback={formatCell(cell)}>
                            <input
                              type="text"
                              value={editing()!.value}
                              onInput={(e) => setEditing((prev) => prev ? { ...prev, value: e.currentTarget.value } : null)}
                              onBlur={commitEdit}
                              onKeyDown={handleEditKeyDown}
                              class="w-full bg-qm-bg text-qm-text px-1 py-0 text-xs font-mono border border-qm-accent rounded focus:outline-none"
                              ref={(el) => setTimeout(() => el.focus(), 0)}
                            />
                          </Show>
                        </div>
                      );
                    }}
                  </For>
                </div>
              );
            }}
          </For>
        </div>
      </div>

      {/* Row context menu */}
      <Show when={rowMenu()}>
        <div
          class="fixed z-50 bg-qm-surface border border-qm-border rounded shadow-lg py-1 min-w-[140px]"
          style={{ left: `${rowMenu()!.x}px`, top: `${rowMenu()!.y}px` }}
          onClick={() => setRowMenu(null)}
        >
          <button
            class="w-full text-left px-3 py-1.5 text-xs hover:bg-qm-border/30 text-qm-text"
            onClick={() => {
              const row = sortedRows()[rowMenu()!.rowIdx];
              const text = props.data.columns.map((c, i) => `${c.name}: ${formatCell(row[i])}`).join(", ");
              navigator.clipboard.writeText(text);
              setRowMenu(null);
            }}
          >
            Copy Row
          </button>
          <div class="border-t border-qm-border my-0.5" />
          <button
            class="w-full text-left px-3 py-1.5 text-xs hover:bg-red-900/30 text-red-400"
            onClick={handleDeleteRow}
          >
            Delete Row
          </button>
        </div>
      </Show>
    </div>
  );
};

export default ResultGrid;
