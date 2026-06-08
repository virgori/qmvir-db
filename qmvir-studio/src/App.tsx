import { ErrorBoundary, Show, createSignal, onMount } from "solid-js";
import type { Component } from "solid-js";
import Sidebar from "./components/Sidebar";
import QueryEditor from "./components/QueryEditor";
import { activeConnection } from "./stores/engine";
import { engineInfo } from "./lib/tauri-bridge";
import type { EngineInfo } from "./lib/tauri-bridge";

const App: Component = () => {
  console.log("[QMvir Studio] App rendering...");
  const [info, setInfo] = createSignal<EngineInfo | null>(null);

  // Refresh engine info whenever connection changes
  let lastConnId: string | null = null;
  function checkRefresh() {
    const conn = activeConnection();
    if (conn && conn.id !== lastConnId) {
      lastConnId = conn.id;
      engineInfo(conn.id).then(setInfo).catch(() => {});
    } else if (!conn) {
      lastConnId = null;
      setInfo(null);
    }
  }
  // Poll connection state (SolidJS signals don't trigger outside components easily)
  onMount(() => {
    const interval = setInterval(checkRefresh, 1000);
    checkRefresh();
    return () => clearInterval(interval);
  });

  return (
    <ErrorBoundary fallback={(err) => (
      <div style="padding:20px;color:#ef4444;background:#0f1117;font-family:monospace;white-space:pre-wrap;height:100vh">
        <h2>QMvir Studio Error</h2>
        <p>{String(err)}</p>
        <p>{err?.stack}</p>
      </div>
    )}>
      <div class="flex h-screen bg-qm-bg text-qm-text">
        {/* Sidebar — Object Browser */}
        <Sidebar />

        {/* Main content — Query Editor + Results */}
        <div class="flex-1 flex flex-col min-w-0">
          {/* Top bar with engine info */}
          <div class="h-8 bg-qm-surface border-b border-qm-border flex items-center px-3 justify-between">
            <div class="flex items-center gap-3">
              <span class="text-xs text-qm-muted">
                QMvir Studio v1.0.0
              </span>
              <Show when={info()}>
                <span class="text-xs text-qm-border">|</span>
                <span class="text-xs text-qm-accent/80 font-mono">
                  Engine v{info()!.version}
                </span>
                <span class="text-xs text-qm-border">|</span>
                <span class="text-xs text-qm-muted font-mono">
                  {info()!.username}
                  <Show when={info()!.is_superuser}>
                    <span class="text-yellow-500 ml-0.5" title="Superuser">★</span>
                  </Show>
                </span>
                <Show when={info()!.data_dir}>
                  <span class="text-xs text-qm-border">|</span>
                  <span class="text-xs text-qm-muted/70 font-mono truncate max-w-[200px]" title={info()!.data_dir!}>
                    {info()!.data_dir}
                  </span>
                </Show>
              </Show>
            </div>
            <div class="flex items-center gap-3">
              <Show when={info()}>
                <span class="text-xs text-qm-muted/50">
                  {info()!.os}/{info()!.arch} • {info()!.engine_type}
                </span>
              </Show>
              <span class="text-xs text-qm-muted/50">
                ⌘+Enter to execute
              </span>
            </div>
          </div>

          {/* Query editor + result grid */}
          <div class="flex-1 overflow-hidden">
            <QueryEditor />
          </div>
        </div>
      </div>
    </ErrorBoundary>
  );
};

export default App;
