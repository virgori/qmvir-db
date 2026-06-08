import { createSignal, Show, For, onMount, onCleanup, lazy, Suspense } from "solid-js";
import type { Component } from "solid-js";
import { executeSql, cancelQuery, getHistory, listTables } from "../lib/tauri-bridge";
import {
  activeConnection,
  setTables,
} from "../stores/engine";
import {
  tabs,
  setTabs,
  activeTabId,
  setActiveTabId,
  newTab,
  history,
  setHistory,
  showHistory,
  setShowHistory,
} from "../stores/query";
import type { QueryResponse } from "../lib/tauri-bridge";
import ResultGrid from "./ResultGrid";

const SqlEditor = lazy(() => import("./SqlEditor"));

const QueryEditor: Component = () => {
  const [localSql, setLocalSql] = createSignal("-- Try these:\n-- CREATE TABLE users (id INT, name TEXT, email TEXT)\n-- INSERT INTO users VALUES (1, 'Alice', 'alice@example.com')\n-- SELECT * FROM users\n\nSELECT 1 + 1 AS result;");
  const [result, setResult] = createSignal<QueryResponse | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [duration, setDuration] = createSignal(0);
  const [currentQueryId, setCurrentQueryId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<string[]>([]);
  const [outputTab, setOutputTab] = createSignal<"data" | "messages">("data");

  // Listen for setQuery events from Sidebar context menu
  function handleSetQuery(e: Event) {
    const detail = (e as CustomEvent).detail;
    if (typeof detail === "string") setLocalSql(detail);
  }
  // Listen for executeQuery events (set text AND auto-run)
  function handleExecuteQuery(e: Event) {
    const detail = (e as CustomEvent).detail;
    if (typeof detail === "string") {
      setLocalSql(detail);
      executeQuery();
    }
  }
  onMount(() => {
    window.addEventListener("qmvir:setQuery", handleSetQuery);
    window.addEventListener("qmvir:executeQuery", handleExecuteQuery);
  });
  onCleanup(() => {
    window.removeEventListener("qmvir:setQuery", handleSetQuery);
    window.removeEventListener("qmvir:executeQuery", handleExecuteQuery);
  });

  async function executeQuery() {
    const conn = activeConnection();
    if (!conn) {
      setError("Not connected — click 'Connect' in the sidebar");
      setMessages(["\u2717 Not connected to any database"]);
      setOutputTab("messages");
      return;
    }
    const sql = localSql().trim();
    if (!sql) return;

    setLoading(true);
    setError(null);
    setResult(null);
    setMessages([]);
    setCurrentQueryId(null);

    try {
      const res = await executeSql(conn.id, sql);
      setResult(res);
      setDuration(res.duration_us);
      setCurrentQueryId(res.query_id);

      // Build output messages
      const msgs: string[] = [];
      if (res.columns.length > 0) {
        msgs.push(`\u2713 ${res.row_count} row(s) returned \u2014 ${formatDuration(res.duration_us)}`);
        if (res.truncated) msgs.push("\u26A0 Result truncated to 10,000 rows");
        setOutputTab("data");
      } else {
        msgs.push(`\u2713 ${res.command_tag} \u2014 ${formatDuration(res.duration_us)}`);
        if (res.row_count > 0) msgs.push(`  ${res.row_count} row(s) affected`);
        setOutputTab("messages");
      }
      setMessages(msgs);

      // Refresh table list after DDL/DML
      const upper = sql.toUpperCase().trimStart();
      if (
        upper.startsWith("CREATE") ||
        upper.startsWith("DROP") ||
        upper.startsWith("ALTER") ||
        upper.startsWith("INSERT") ||
        upper.startsWith("UPDATE") ||
        upper.startsWith("DELETE") ||
        upper.startsWith("TRUNCATE")
      ) {
        try {
          const t = await listTables(conn.id);
          setTables(t);
        } catch {}
      }
    } catch (e: any) {
      const msg = typeof e === "string" ? e : e.message || String(e);
      setError(msg);
      setMessages([`\u2717 ERROR: ${msg}`]);
      setOutputTab("messages");
    } finally {
      setLoading(false);
      setCurrentQueryId(null);
    }
  }

  async function handleCancel() {
    const qid = currentQueryId();
    if (qid) {
      try {
        await cancelQuery(qid);
      } catch {}
    }
  }

  async function loadHistory() {
    try {
      const h = await getHistory();
      setHistory(h);
      setShowHistory(true);
    } catch (e) {
      console.error("Failed to load history:", e);
    }
  }

  function formatDuration(us: number): string {
    if (us < 1000) return `${us}μs`;
    if (us < 1_000_000) return `${(us / 1000).toFixed(1)}ms`;
    return `${(us / 1_000_000).toFixed(2)}s`;
  }

  return (
    <div class="flex flex-col h-full">
      {/* SQL Editor — Monaco */}
      <div class="border-b border-qm-border">
        <div class="h-40">
          <Suspense fallback={
            <textarea
              value={localSql()}
              onInput={(e) => setLocalSql(e.currentTarget.value)}
              class="w-full h-full bg-qm-surface text-qm-text p-3 font-mono text-sm resize-none focus:outline-none"
              placeholder="Loading editor..."
              spellcheck={false}
            />
          }>
            <SqlEditor
              value={localSql()}
              onChange={setLocalSql}
              onExecute={executeQuery}
            />
          </Suspense>
        </div>
        <div class="flex items-center gap-2 px-3 py-1.5 bg-qm-bg border-t border-qm-border">
          <Show when={!loading()} fallback={
            <button
              onClick={handleCancel}
              class="px-3 py-1 bg-red-600 text-white text-xs rounded
                     hover:bg-red-500 font-medium"
            >
              ■ Cancel
            </button>
          }>
            <button
              onClick={executeQuery}
              class="px-3 py-1 bg-qm-accent text-white text-xs rounded
                     hover:bg-blue-600 disabled:opacity-50 font-medium"
            >
              ▶ Run (⌘+Enter)
            </button>
          </Show>

          <button
            onClick={loadHistory}
            class="px-2 py-1 text-xs text-qm-muted hover:text-qm-text"
            title="Query History"
          >
            ⏱ History
          </button>

          <Show when={result()}>
            <span class="text-xs text-qm-muted">
              {result()!.row_count} rows · {formatDuration(duration())}
              {" · "}
              {result()!.command_tag}
            </span>
            <Show when={result()!.truncated}>
              <span class="text-xs text-yellow-400 font-medium">
                ⚠ Truncated (limit 10,000 rows)
              </span>
            </Show>
          </Show>
        </div>
      </div>

      {/* Error */}
      <Show when={error()}>
        <div class="px-3 py-2 bg-red-900/20 border-b border-red-800 text-red-400 text-xs font-mono">
          {error()}
        </div>
      </Show>

      {/* History Panel */}
      <Show when={showHistory()}>
        <div class="border-b border-qm-border bg-qm-surface max-h-48 overflow-y-auto">
          <div class="flex items-center justify-between px-3 py-1.5 border-b border-qm-border">
            <span class="text-xs font-semibold text-qm-accent">Query History</span>
            <button onClick={() => setShowHistory(false)} class="text-xs text-qm-muted hover:text-qm-text">✕</button>
          </div>
          <Show when={history().length > 0} fallback={
            <div class="px-3 py-2 text-xs text-qm-muted italic">No history yet.</div>
          }>
            <For each={history()}>
              {(entry) => (
                <button
                  class="w-full text-left px-3 py-1 text-xs hover:bg-qm-border/30 border-b border-qm-border/30 flex items-center gap-2"
                  onClick={() => { setLocalSql(entry.sql); setShowHistory(false); }}
                >
                  <span class={`w-1.5 h-1.5 rounded-full flex-shrink-0 ${entry.error ? "bg-red-400" : "bg-green-400"}`} />
                  <span class="font-mono truncate flex-1 text-qm-text">{entry.sql}</span>
                  <span class="text-qm-muted flex-shrink-0">{formatDuration(entry.duration_us)}</span>
                  <span class="text-qm-muted flex-shrink-0">{entry.row_count}r</span>
                </button>
              )}
            </For>
          </Show>
        </div>
      </Show>

      {/* Output area with tabs */}
      <div class="flex-1 flex flex-col overflow-hidden">
        {/* Tab bar */}
        <div class="flex items-center bg-qm-surface border-b border-qm-border">
          <button
            class={`px-3 py-1.5 text-xs font-medium border-b-2 transition-colors ${
              outputTab() === "data"
                ? "border-qm-accent text-qm-accent"
                : "border-transparent text-qm-muted hover:text-qm-text"
            }`}
            onClick={() => setOutputTab("data")}
          >
            Data Output
          </button>
          <button
            class={`px-3 py-1.5 text-xs font-medium border-b-2 transition-colors ${
              outputTab() === "messages"
                ? "border-qm-accent text-qm-accent"
                : "border-transparent text-qm-muted hover:text-qm-text"
            }`}
            onClick={() => setOutputTab("messages")}
          >
            Messages
          </button>
        </div>

        {/* Tab content */}
        <div class="flex-1 overflow-hidden">
          <Show when={outputTab() === "data"}>
            <Show
              when={result() && result()!.columns.length > 0}
              fallback={
                <div class="flex items-center justify-center h-full text-qm-muted text-sm">
                  Run a SELECT query to see results
                </div>
              }
            >
              <ResultGrid data={result()!} />
            </Show>
          </Show>
          <Show when={outputTab() === "messages"}>
            <div class="h-full overflow-y-auto p-3 font-mono text-xs space-y-1">
              <Show
                when={messages().length > 0}
                fallback={<span class="text-qm-muted italic">No messages</span>}
              >
                <For each={messages()}>
                  {(msg) => (
                    <div
                      class={
                        msg.startsWith("\u2717")
                          ? "text-red-400"
                          : msg.startsWith("\u26A0")
                            ? "text-yellow-400"
                            : msg.startsWith("\u2713")
                              ? "text-green-400"
                              : "text-qm-muted"
                      }
                    >
                      {msg}
                    </div>
                  )}
                </For>
              </Show>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
};

export default QueryEditor;
