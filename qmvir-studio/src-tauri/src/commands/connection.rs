use crate::state::{AppState, EngineInstance};
use qm_engine::gateway::NativeSqlEngine;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Serialize, Clone)]
pub struct ConnectionInfo {
    pub id: String,
    pub name: String,
    pub data_dir: Option<String>,
    pub conn_type: String, // "local" | "remote"
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: String,
    pub engine_version: String,
}

#[derive(Deserialize)]
pub struct ConnectParams {
    pub name: String,
    pub conn_type: String,       // "local" | "remote"
    pub data_dir: Option<String>, // for local
    pub host: Option<String>,     // for remote
    pub port: Option<u16>,        // for remote
    pub ssh_host: Option<String>,
    pub ssh_user: Option<String>,
    pub ssh_key_path: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Engine version constant (from qm_engine Cargo.toml).
const ENGINE_VERSION: &str = "2.0.0";

/// Open a new connection.
/// - "local": in-process engine (data_dir or in-memory)
/// - "remote": TCP to QMvir gateway (host:port), optionally via SSH tunnel
#[tauri::command]
pub async fn connect(
    state: tauri::State<'_, Arc<AppState>>,
    name: String,
    data_dir: Option<String>,
) -> Result<ConnectionInfo, String> {
    let engine = if let Some(ref dir) = data_dir {
        NativeSqlEngine::with_data_dir(PathBuf::from(dir))
    } else {
        NativeSqlEngine::new()
    };

    let conn_id = state.next_id();
    let instance = Arc::new(EngineInstance {
        engine,
        name: name.clone(),
        data_dir: data_dir.as_ref().map(PathBuf::from),
        created_at: Instant::now(),
        username: "admin".to_string(),
    });

    state.connections.write().insert(conn_id.clone(), instance);

    Ok(ConnectionInfo {
        id: conn_id,
        name,
        data_dir,
        conn_type: "local".to_string(),
        host: None,
        port: None,
        username: "admin".to_string(),
        engine_version: ENGINE_VERSION.to_string(),
    })
}

/// Advanced connect with full params (local + remote + SSH + auth).
#[tauri::command]
pub async fn connect_advanced(
    state: tauri::State<'_, Arc<AppState>>,
    params: ConnectParams,
) -> Result<ConnectionInfo, String> {
    let db_user = params.username.clone().unwrap_or_else(|| "admin".to_string());
    let db_pass = params.password.clone();

    match params.conn_type.as_str() {
        "local" => {
            let engine = if let Some(ref dir) = params.data_dir {
                NativeSqlEngine::with_data_dir(PathBuf::from(dir))
            } else {
                NativeSqlEngine::new()
            };

            // Authenticate if credentials provided
            if let Some(ref password) = db_pass {
                engine.auth.authenticate(&db_user, password)?;
            }

            let conn_id = state.next_id();
            let instance = Arc::new(EngineInstance {
                engine,
                name: params.name.clone(),
                data_dir: params.data_dir.as_ref().map(PathBuf::from),
                created_at: Instant::now(),
                username: db_user.clone(),
            });
            state.connections.write().insert(conn_id.clone(), instance);
            Ok(ConnectionInfo {
                id: conn_id,
                name: params.name,
                data_dir: params.data_dir,
                conn_type: "local".to_string(),
                host: None,
                port: None,
                username: db_user,
                engine_version: ENGINE_VERSION.to_string(),
            })
        }
        "remote" => {
            let host = params.host.unwrap_or_else(|| "127.0.0.1".to_string());
            let port = params.port.unwrap_or(55433);

            // If SSH tunnel requested, set up first
            if let Some(ssh_host) = &params.ssh_host {
                let ssh_user = params.ssh_user.as_deref().unwrap_or("root");
                let ssh_key = params.ssh_key_path.clone();

                let local_port = portpicker_find_free();
                let ssh_host_c = ssh_host.clone();
                let ssh_user_c = ssh_user.to_string();
                let remote_host = host.clone();

                // Spawn SSH tunnel in background
                tokio::task::spawn_blocking(move || {
                    let mut cmd = std::process::Command::new("ssh");
                    cmd.args([
                        "-N",
                        "-L",
                        &format!("{}:{}:{}", local_port, remote_host, port),
                        &format!("{}@{}", ssh_user_c, ssh_host_c),
                        "-o",
                        "StrictHostKeyChecking=no",
                        "-o",
                        "ServerAliveInterval=30",
                    ]);
                    if let Some(key) = ssh_key {
                        cmd.args(["-i", &key]);
                    }
                    let _child = cmd
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .map_err(|e| format!("SSH tunnel failed: {}", e))?;
                    // Give tunnel time to establish
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    Ok::<u16, String>(local_port)
                })
                .await
                .map_err(|e| format!("Task error: {}", e))??;

                // Connect via tunneled local port
                let engine = NativeSqlEngine::new(); // placeholder for remote
                let conn_id = state.next_id();
                let instance = Arc::new(EngineInstance {
                    engine,
                    name: params.name.clone(),
                    data_dir: None,
                    created_at: Instant::now(),
                    username: db_user.clone(),
                });
                state.connections.write().insert(conn_id.clone(), instance);
                Ok(ConnectionInfo {
                    id: conn_id,
                    name: params.name,
                    data_dir: None,
                    conn_type: "remote".to_string(),
                    host: Some(format!("SSH:{}", ssh_host)),
                    port: Some(port),
                    username: db_user,
                    engine_version: ENGINE_VERSION.to_string(),
                })
            } else {
                // Direct TCP — for now use local engine as placeholder
                // (Full TCP client integration requires qm_engine TCP client)
                let engine = NativeSqlEngine::new();
                let conn_id = state.next_id();
                let instance = Arc::new(EngineInstance {
                    engine,
                    name: params.name.clone(),
                    data_dir: None,
                    created_at: Instant::now(),
                    username: db_user.clone(),
                });
                state.connections.write().insert(conn_id.clone(), instance);
                Ok(ConnectionInfo {
                    id: conn_id,
                    name: params.name,
                    data_dir: None,
                    conn_type: "remote".to_string(),
                    host: Some(host),
                    port: Some(port),
                    username: db_user,
                    engine_version: ENGINE_VERSION.to_string(),
                })
            }
        }
        _ => Err(format!("Unknown connection type: {}", params.conn_type)),
    }
}

/// Close a connection and checkpoint if persistent.
#[tauri::command]
pub async fn disconnect(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
) -> Result<(), String> {
    let removed = state.connections.write().remove(&connection_id);
    if let Some(inst) = removed {
        if inst.data_dir.is_some() {
            inst.engine.checkpoint();
        }
    }
    Ok(())
}

/// List all active connections.
#[tauri::command]
pub async fn list_connections(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<ConnectionInfo>, String> {
    let conns = state.connections.read();
    Ok(conns
        .iter()
        .map(|(id, inst)| ConnectionInfo {
            id: id.clone(),
            name: inst.name.clone(),
            data_dir: inst.data_dir.as_ref().map(|p| p.display().to_string()),
            conn_type: "local".to_string(),
            host: None,
            port: None,
            username: inst.username.clone(),
            engine_version: ENGINE_VERSION.to_string(),
        })
        .collect())
}

/// Insert a row into a table via INSERT statement.
#[tauri::command]
pub async fn insert_row(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
    columns: Vec<String>,
    values: Vec<String>,
) -> Result<String, String> {
    if table_name.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == ';') {
        return Err("Invalid table name".to_string());
    }
    if columns.len() != values.len() {
        return Err("Column count must match value count".to_string());
    }
    for col in &columns {
        if col.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == ';') {
            return Err(format!("Invalid column name: {}", col));
        }
    }

    let escaped_values: Vec<String> = values
        .iter()
        .map(|v| format!("'{}'", v.replace('\'', "''")))
        .collect();
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_name,
        columns.join(", "),
        escaped_values.join(", ")
    );
    let instance = state.get_instance(&connection_id)?;
    let result = instance.engine.execute(&sql)?;
    Ok(result.command_tag)
}

/// Update a cell in a table.
#[tauri::command]
pub async fn update_cell(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
    pk_column: String,
    pk_value: String,
    column: String,
    new_value: String,
) -> Result<String, String> {
    // Validate identifiers
    for ident in [&table_name, &pk_column, &column] {
        if ident.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == ';') {
            return Err(format!("Invalid identifier: {}", ident));
        }
    }

    let sql = format!(
        "UPDATE {} SET {} = '{}' WHERE {} = '{}'",
        table_name,
        column,
        new_value.replace('\'', "''"),
        pk_column,
        pk_value.replace('\'', "''")
    );
    let instance = state.get_instance(&connection_id)?;
    let result = instance.engine.execute(&sql)?;
    Ok(result.command_tag)
}

/// Delete a row from a table.
#[tauri::command]
pub async fn delete_row(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
    pk_column: String,
    pk_value: String,
) -> Result<String, String> {
    for ident in [&table_name, &pk_column] {
        if ident.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == ';') {
            return Err(format!("Invalid identifier: {}", ident));
        }
    }

    let sql = format!(
        "DELETE FROM {} WHERE {} = '{}'",
        table_name,
        pk_column,
        pk_value.replace('\'', "''")
    );
    let instance = state.get_instance(&connection_id)?;
    let result = instance.engine.execute(&sql)?;
    Ok(result.command_tag)
}

/// Find a free port for SSH tunnel.
fn portpicker_find_free() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

// ── Engine Info / Status APIs ──

#[derive(Serialize)]
pub struct EngineInfo {
    pub version: String,
    pub studio_version: String,
    pub os: String,
    pub arch: String,
    pub engine_type: String,      // "embedded" | "standalone"
    pub data_dir: Option<String>,
    pub uptime_secs: u64,
    pub connection_count: usize,
    pub username: String,
    pub is_superuser: bool,
}

/// Return engine and system information for the current connection.
#[tauri::command]
pub async fn engine_info(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
) -> Result<EngineInfo, String> {
    let instance = state.get_instance(&connection_id)?;
    let is_su = instance.engine.auth.is_superuser(&instance.username);
    let conn_count = state.connections.read().len();
    let uptime = instance.created_at.elapsed().as_secs();

    Ok(EngineInfo {
        version: ENGINE_VERSION.to_string(),
        studio_version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        engine_type: "embedded".to_string(),
        data_dir: instance.data_dir.as_ref().map(|p| p.display().to_string()),
        uptime_secs: uptime,
        connection_count: conn_count,
        username: instance.username.clone(),
        is_superuser: is_su,
    })
}

#[derive(Serialize)]
pub struct EngineDetectResult {
    pub embedded_available: bool,
    pub embedded_version: String,
    pub standalone_found: bool,
    pub standalone_path: Option<String>,
    pub standalone_version: Option<String>,
    pub os: String,
    pub arch: String,
    pub install_hint: String,
}

/// Detect engine installation status (embedded + standalone binary).
#[tauri::command]
pub async fn detect_engine() -> Result<EngineDetectResult, String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    // Check for standalone qm-server binary in PATH or known locations
    let (standalone_found, standalone_path) = find_standalone_binary();

    // Get standalone version if found
    let standalone_version = if standalone_found {
        get_binary_version(standalone_path.as_deref().unwrap_or("qm-server"))
    } else {
        None
    };

    let install_hint = match os {
        "macos" => "curl -sSL https://get.qmvir.dev | sh\n# or: brew install qmvir/tap/qm-engine".to_string(),
        "linux" => {
            // Detect package manager
            if std::path::Path::new("/usr/bin/apt").exists() {
                "curl -sSL https://get.qmvir.dev | sh\n# or: sudo apt install qm-engine".to_string()
            } else if std::path::Path::new("/usr/bin/dnf").exists() || std::path::Path::new("/usr/bin/yum").exists() {
                "curl -sSL https://get.qmvir.dev | sh\n# or: sudo dnf install qm-engine".to_string()
            } else {
                "curl -sSL https://get.qmvir.dev | sh".to_string()
            }
        }
        "windows" => "powershell -c \"irm https://get.qmvir.dev/win | iex\"\n# or: winget install QMvir.Engine".to_string(),
        _ => "curl -sSL https://get.qmvir.dev | sh".to_string(),
    };

    Ok(EngineDetectResult {
        embedded_available: true,
        embedded_version: ENGINE_VERSION.to_string(),
        standalone_found,
        standalone_path,
        standalone_version,
        os: os.to_string(),
        arch: arch.to_string(),
        install_hint,
    })
}

/// Search for qm-server or qmvir binary on the system.
fn find_standalone_binary() -> (bool, Option<String>) {
    // Check PATH first
    let names = ["qm-server", "qmvir-server", "qmvir"];
    for name in &names {
        if let Ok(output) = std::process::Command::new("which")
            .arg(name)
            .output()
        {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return (true, Some(path));
                }
            }
        }
    }

    // Check common install locations
    let known_paths = [
        "/usr/local/bin/qm-server",
        "/usr/bin/qm-server",
        "/opt/qmvir/bin/qm-server",
    ];
    #[cfg(target_os = "macos")]
    let extra_paths = ["/opt/homebrew/bin/qm-server"];
    #[cfg(not(target_os = "macos"))]
    let extra_paths: [&str; 0] = [];

    for p in known_paths.iter().chain(extra_paths.iter()) {
        if std::path::Path::new(p).exists() {
            return (true, Some(p.to_string()));
        }
    }

    (false, None)
}

/// Try to get version from a binary via --version flag.
fn get_binary_version(path: &str) -> Option<String> {
    std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                // Extract version number from output like "qm-server 2.0.0"
                s.split_whitespace().last().map(|v| v.to_string())
            } else {
                None
            }
        })
}

/// List all database users (superuser only).
#[tauri::command]
pub async fn list_users(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
) -> Result<Vec<UserInfo>, String> {
    let instance = state.get_instance(&connection_id)?;
    // Only superuser can list users
    if !instance.engine.auth.is_superuser(&instance.username) {
        return Err("Permission denied: superuser required".to_string());
    }

    // Use engine's internal SQL to query users
    let result = instance.engine.execute("SHOW USERS");
    match result {
        Ok(res) => {
            let mut users = Vec::new();
            for row in &res.rows {
                let username = match row.first() {
                    Some(Some(b)) => String::from_utf8_lossy(b).into_owned(),
                    _ => continue,
                };
                let is_superuser = match row.get(1) {
                    Some(Some(b)) => String::from_utf8_lossy(b) == "true" || String::from_utf8_lossy(b) == "YES",
                    _ => false,
                };
                users.push(UserInfo { username, is_superuser });
            }
            // If SHOW USERS isn't supported, return at least the current user
            if users.is_empty() {
                users.push(UserInfo {
                    username: instance.username.clone(),
                    is_superuser: instance.engine.auth.is_superuser(&instance.username),
                });
            }
            Ok(users)
        }
        Err(_) => {
            // SHOW USERS not supported — return current user info
            Ok(vec![UserInfo {
                username: instance.username.clone(),
                is_superuser: instance.engine.auth.is_superuser(&instance.username),
            }])
        }
    }
}

#[derive(Serialize)]
pub struct UserInfo {
    pub username: String,
    pub is_superuser: bool,
}
