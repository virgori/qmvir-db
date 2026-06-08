import { createSignal, Show, For, onMount } from "solid-js";
import type { Component } from "solid-js";
import { connect, connectAdvanced, listTables, detectEngine } from "../lib/tauri-bridge";
import type { ConnectParams, EngineDetectResult } from "../lib/tauri-bridge";
import {
  activeConnection,
  setActiveConnection,
  setTables,
} from "../stores/engine";

const ConnectDialog: Component<{ onClose: () => void }> = (props) => {
  const [connType, setConnType] = createSignal<"local" | "remote">("local");
  const [name, setName] = createSignal("default");
  const [dataDir, setDataDir] = createSignal("");
  const [host, setHost] = createSignal("127.0.0.1");
  const [port, setPort] = createSignal(55433);
  const [sshEnabled, setSshEnabled] = createSignal(false);
  const [sshHost, setSshHost] = createSignal("");
  const [sshUser, setSshUser] = createSignal("root");
  const [sshKeyPath, setSshKeyPath] = createSignal("");
  const [dbUser, setDbUser] = createSignal("admin");
  const [dbPass, setDbPass] = createSignal("");
  const [showAuth, setShowAuth] = createSignal(false);
  const [connecting, setConnecting] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [engineStatus, setEngineStatus] = createSignal<EngineDetectResult | null>(null);
  const [detectingEngine, setDetectingEngine] = createSignal(true);

  onMount(async () => {
    try {
      const status = await detectEngine();
      setEngineStatus(status);
    } catch (e) {
      console.error("Engine detect failed:", e);
    } finally {
      setDetectingEngine(false);
    }
  });

  async function handleConnect() {
    setConnecting(true);
    setError(null);
    try {
      let conn;
      if (connType() === "local" && !dataDir() && !showAuth()) {
        // Simple in-memory connection (no auth)
        conn = await connect(name(), undefined);
      } else {
        // Use advanced connect for auth or data dir
        const params: ConnectParams = {
          name: name(),
          conn_type: connType(),
        };
        if (connType() === "local") {
          if (dataDir()) params.data_dir = dataDir();
        } else {
          params.host = host();
          params.port = port();
          if (sshEnabled()) {
            params.ssh_host = sshHost();
            params.ssh_user = sshUser();
            if (sshKeyPath()) params.ssh_key_path = sshKeyPath();
          }
        }
        if (showAuth() && dbUser()) {
          params.username = dbUser();
          if (dbPass()) params.password = dbPass();
        }
        conn = await connectAdvanced(params);
      }
      setActiveConnection(conn);
      try {
        const t = await listTables(conn.id);
        setTables(t);
      } catch {}
      props.onClose();
    } catch (e: any) {
      setError(typeof e === "string" ? e : e.message || String(e));
    } finally {
      setConnecting(false);
    }
  }

  return (
    <div class="fixed inset-0 bg-black/60 flex items-center justify-center z-50" onClick={props.onClose}>
      <div class="bg-qm-surface border border-qm-border rounded-lg p-5 w-[460px] max-h-[90vh] overflow-y-auto shadow-xl" onClick={(e) => e.stopPropagation()}>
        <h3 class="text-sm font-semibold text-qm-accent mb-4">New Connection</h3>

        {/* Engine Status Banner */}
        <Show when={!detectingEngine()}>
          <Show when={engineStatus()}>
            <div class={`rounded px-3 py-2 mb-4 text-xs border ${engineStatus()!.embedded_available ? "bg-emerald-900/20 border-emerald-700/30 text-emerald-300" : "bg-yellow-900/20 border-yellow-700/30 text-yellow-300"}`}>
              <div class="flex items-center justify-between">
                <div class="flex items-center gap-2">
                  <span class={engineStatus()!.embedded_available ? "text-emerald-400" : "text-yellow-400"}>
                    {engineStatus()!.embedded_available ? "●" : "○"}
                  </span>
                  <span>
                    QMvir Engine v{engineStatus()!.embedded_version}
                    <span class="text-qm-muted ml-1">({engineStatus()!.os}/{engineStatus()!.arch})</span>
                  </span>
                </div>
                <span class="text-[10px] px-1.5 py-0.5 rounded bg-qm-bg/50">embedded</span>
              </div>
              <Show when={engineStatus()!.standalone_found}>
                <div class="mt-1 flex items-center gap-2 text-qm-muted">
                  <span class="text-blue-400">●</span>
                  <span>Standalone server: {engineStatus()!.standalone_version ?? "?"} at {engineStatus()!.standalone_path}</span>
                </div>
              </Show>
              <Show when={!engineStatus()!.standalone_found}>
                <div class="mt-1.5 pt-1.5 border-t border-current/10">
                  <div class="flex items-center justify-between">
                    <span class="text-qm-muted">Standalone server not found</span>
                    <button
                      class="text-[10px] px-2 py-0.5 bg-qm-accent/80 text-white rounded hover:bg-qm-accent"
                      onClick={(e) => { e.stopPropagation(); navigator.clipboard.writeText(engineStatus()!.install_hint); alert("Install command copied to clipboard!"); }}
                      title={engineStatus()!.install_hint}
                    >
                      Copy Install Cmd
                    </button>
                  </div>
                  <div class="mt-1 font-mono text-[10px] text-qm-muted/70 bg-qm-bg/50 rounded px-2 py-1 whitespace-pre">{engineStatus()!.install_hint}</div>
                </div>
              </Show>
            </div>
          </Show>
        </Show>
        <Show when={detectingEngine()}>
          <div class="text-xs text-qm-muted mb-4 animate-pulse">Detecting engine...</div>
        </Show>

        {/* Connection Type Tabs */}
        <div class="flex gap-1 mb-4 bg-qm-bg rounded p-0.5">
          <button
            class={`flex-1 px-3 py-1.5 text-xs rounded transition-colors ${
              connType() === "local" ? "bg-qm-accent text-white" : "text-qm-muted hover:text-qm-text"
            }`}
            onClick={() => setConnType("local")}
          >
            Local / In-Process
          </button>
          <button
            class={`flex-1 px-3 py-1.5 text-xs rounded transition-colors ${
              connType() === "remote" ? "bg-qm-accent text-white" : "text-qm-muted hover:text-qm-text"
            }`}
            onClick={() => setConnType("remote")}
          >
            Remote TCP
          </button>
        </div>

        {/* Connection Name */}
        <label class="block text-xs text-qm-muted mb-1">Connection Name</label>
        <input
          type="text"
          value={name()}
          onInput={(e) => setName(e.currentTarget.value)}
          class="w-full bg-qm-bg text-qm-text px-2 py-1.5 rounded border border-qm-border text-xs font-mono mb-3 focus:outline-none focus:ring-1 focus:ring-qm-accent/50"
        />

        {/* Local settings */}
        <Show when={connType() === "local"}>
          <label class="block text-xs text-qm-muted mb-1">Data Directory (leave empty for in-memory)</label>
          <input
            type="text"
            value={dataDir()}
            onInput={(e) => setDataDir(e.currentTarget.value)}
            placeholder="/path/to/data"
            class="w-full bg-qm-bg text-qm-text px-2 py-1.5 rounded border border-qm-border text-xs font-mono mb-3 focus:outline-none focus:ring-1 focus:ring-qm-accent/50"
          />
        </Show>

        {/* Remote settings */}
        <Show when={connType() === "remote"}>
          <div class="flex gap-2 mb-3">
            <div class="flex-1">
              <label class="block text-xs text-qm-muted mb-1">Host</label>
              <input
                type="text"
                value={host()}
                onInput={(e) => setHost(e.currentTarget.value)}
                class="w-full bg-qm-bg text-qm-text px-2 py-1.5 rounded border border-qm-border text-xs font-mono focus:outline-none focus:ring-1 focus:ring-qm-accent/50"
              />
            </div>
            <div class="w-24">
              <label class="block text-xs text-qm-muted mb-1">Port</label>
              <input
                type="number"
                value={port()}
                onInput={(e) => setPort(parseInt(e.currentTarget.value) || 55433)}
                class="w-full bg-qm-bg text-qm-text px-2 py-1.5 rounded border border-qm-border text-xs font-mono focus:outline-none focus:ring-1 focus:ring-qm-accent/50"
              />
            </div>
          </div>

          {/* SSH Tunnel */}
          <div class="border border-qm-border/50 rounded p-3 mb-3">
            <label class="flex items-center gap-2 text-xs text-qm-muted cursor-pointer mb-2">
              <input
                type="checkbox"
                checked={sshEnabled()}
                onChange={(e) => setSshEnabled(e.currentTarget.checked)}
                class="accent-qm-accent"
              />
              SSH Tunnel
            </label>
            <Show when={sshEnabled()}>
              <div class="space-y-2">
                <div>
                  <label class="block text-xs text-qm-muted mb-0.5">SSH Host</label>
                  <input
                    type="text"
                    value={sshHost()}
                    onInput={(e) => setSshHost(e.currentTarget.value)}
                    placeholder="ssh.example.com"
                    class="w-full bg-qm-bg text-qm-text px-2 py-1 rounded border border-qm-border text-xs font-mono focus:outline-none"
                  />
                </div>
                <div class="flex gap-2">
                  <div class="flex-1">
                    <label class="block text-xs text-qm-muted mb-0.5">SSH User</label>
                    <input
                      type="text"
                      value={sshUser()}
                      onInput={(e) => setSshUser(e.currentTarget.value)}
                      class="w-full bg-qm-bg text-qm-text px-2 py-1 rounded border border-qm-border text-xs font-mono focus:outline-none"
                    />
                  </div>
                  <div class="flex-1">
                    <label class="block text-xs text-qm-muted mb-0.5">Key Path (optional)</label>
                    <input
                      type="text"
                      value={sshKeyPath()}
                      onInput={(e) => setSshKeyPath(e.currentTarget.value)}
                      placeholder="~/.ssh/id_rsa"
                      class="w-full bg-qm-bg text-qm-text px-2 py-1 rounded border border-qm-border text-xs font-mono focus:outline-none"
                    />
                  </div>
                </div>
              </div>
            </Show>
          </div>
        </Show>

        {/* Authentication — pgAdmin style */}
        <div class="border border-qm-border/50 rounded p-3 mb-3">
          <label class="flex items-center gap-2 text-xs text-qm-muted cursor-pointer mb-2">
            <input
              type="checkbox"
              checked={showAuth()}
              onChange={(e) => setShowAuth(e.currentTarget.checked)}
              class="accent-qm-accent"
            />
            Authentication (login as database user)
          </label>
          <Show when={showAuth()}>
            <div class="space-y-2">
              <div>
                <label class="block text-xs text-qm-muted mb-0.5">Username</label>
                <input
                  type="text"
                  value={dbUser()}
                  onInput={(e) => setDbUser(e.currentTarget.value)}
                  placeholder="admin"
                  class="w-full bg-qm-bg text-qm-text px-2 py-1 rounded border border-qm-border text-xs font-mono focus:outline-none"
                />
              </div>
              <div>
                <label class="block text-xs text-qm-muted mb-0.5">Password</label>
                <input
                  type="password"
                  value={dbPass()}
                  onInput={(e) => setDbPass(e.currentTarget.value)}
                  placeholder="••••••••"
                  class="w-full bg-qm-bg text-qm-text px-2 py-1 rounded border border-qm-border text-xs font-mono focus:outline-none"
                />
              </div>
              <div class="text-[10px] text-qm-muted/60">
                Default superuser: <span class="font-mono">admin</span> (password from QM_ADMIN_PASSWORD env or auto-generated)
              </div>
            </div>
          </Show>
        </div>

        <Show when={error()}>
          <div class="text-xs text-red-400 mb-2 bg-red-900/20 rounded px-2 py-1">{error()}</div>
        </Show>

        <div class="flex gap-2 justify-end mt-2">
          <button onClick={props.onClose} class="px-3 py-1.5 text-xs text-qm-muted hover:text-qm-text">
            Cancel
          </button>
          <button
            onClick={handleConnect}
            disabled={connecting()}
            class="px-4 py-1.5 bg-qm-accent text-white text-xs rounded hover:bg-blue-600 disabled:opacity-50 font-medium"
          >
            {connecting() ? "Connecting..." : "Connect"}
          </button>
        </div>
      </div>
    </div>
  );
};

export default ConnectDialog;
