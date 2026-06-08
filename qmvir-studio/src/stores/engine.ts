import { createSignal } from "solid-js";
import type { ConnectionInfo, TableOverview } from "../lib/tauri-bridge";

// Current active connection
const [activeConnection, setActiveConnection] =
  createSignal<ConnectionInfo | null>(null);

// List of tables in the current connection
const [tables, setTables] = createSignal<TableOverview[]>([]);

// Currently selected table
const [selectedTable, setSelectedTable] = createSignal<string | null>(null);

export {
  activeConnection,
  setActiveConnection,
  tables,
  setTables,
  selectedTable,
  setSelectedTable,
};
