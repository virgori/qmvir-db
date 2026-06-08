import { createSignal } from "solid-js";
import type { QueryResponse, HistoryEntry } from "../lib/tauri-bridge";

export interface QueryTab {
  id: string;
  sql: string;
  result: QueryResponse | null;
  error: string | null;
  loading: boolean;
  queryId: string | null;
}

let tabCounter = 0;

export function newTab(): QueryTab {
  tabCounter += 1;
  return {
    id: `tab-${tabCounter}`,
    sql: "",
    result: null,
    error: null,
    loading: false,
    queryId: null,
  };
}

const [tabs, setTabs] = createSignal<QueryTab[]>([newTab()]);
const [activeTabId, setActiveTabId] = createSignal<string>(tabs()[0].id);
const [history, setHistory] = createSignal<HistoryEntry[]>([]);
const [showHistory, setShowHistory] = createSignal(false);

export { tabs, setTabs, activeTabId, setActiveTabId, history, setHistory, showHistory, setShowHistory };
