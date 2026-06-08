import { createSignal, Show, For } from "solid-js";
import type { Component } from "solid-js";
import { insertRow, listTables, tableDetail } from "../lib/tauri-bridge";
import type { ColumnDef } from "../lib/tauri-bridge";
import { activeConnection, setTables, selectedTable } from "../stores/engine";

const InsertRowDialog: Component<{
  tableName: string;
  columns: ColumnDef[];
  onClose: () => void;
  onInserted: () => void;
}> = (props) => {
  const [values, setValues] = createSignal<Record<string, string>>(
    Object.fromEntries(props.columns.map((c) => [c.name, ""]))
  );
  const [error, setError] = createSignal<string | null>(null);
  const [inserting, setInserting] = createSignal(false);

  function updateValue(colName: string, val: string) {
    setValues((prev) => ({ ...prev, [colName]: val }));
  }

  async function handleInsert() {
    const conn = activeConnection();
    if (!conn) return;

    const cols = props.columns.map((c) => c.name);
    const vals = cols.map((c) => values()[c] || "");

    setInserting(true);
    setError(null);
    try {
      await insertRow(conn.id, props.tableName, cols, vals);
      // Refresh
      try {
        const t = await listTables(conn.id);
        setTables(t);
      } catch {}
      props.onInserted();
      props.onClose();
    } catch (e: any) {
      setError(typeof e === "string" ? e : e.message || String(e));
    } finally {
      setInserting(false);
    }
  }

  return (
    <div class="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={props.onClose}>
      <div class="bg-qm-surface border border-qm-border rounded-lg p-4 w-96 max-h-[80vh] overflow-y-auto" onClick={(e) => e.stopPropagation()}>
        <h3 class="text-sm font-semibold text-qm-accent mb-3">
          Insert into <span class="font-mono">{props.tableName}</span>
        </h3>

        <div class="space-y-2">
          <For each={props.columns}>
            {(col) => (
              <div>
                <label class="flex items-center gap-2 text-xs text-qm-muted mb-0.5">
                  <span class="font-mono">{col.name}</span>
                  <span class="text-qm-muted/50">{col.type_name}</span>
                </label>
                <input
                  type="text"
                  value={values()[col.name] || ""}
                  onInput={(e) => updateValue(col.name, e.currentTarget.value)}
                  placeholder={col.type_name}
                  class="w-full bg-qm-bg text-qm-text px-2 py-1 rounded border border-qm-border text-xs font-mono focus:outline-none focus:ring-1 focus:ring-qm-accent/50"
                />
              </div>
            )}
          </For>
        </div>

        <Show when={error()}>
          <div class="text-xs text-red-400 mt-2">{error()}</div>
        </Show>

        <div class="flex gap-2 justify-end mt-3">
          <button onClick={props.onClose} class="px-3 py-1 text-xs text-qm-muted hover:text-qm-text">
            Cancel
          </button>
          <button
            onClick={handleInsert}
            disabled={inserting()}
            class="px-3 py-1 bg-qm-accent text-white text-xs rounded hover:bg-blue-600 disabled:opacity-50"
          >
            {inserting() ? "Inserting..." : "Insert"}
          </button>
        </div>
      </div>
    </div>
  );
};

export default InsertRowDialog;
