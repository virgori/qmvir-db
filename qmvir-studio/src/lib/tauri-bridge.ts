import { invoke } from "@tauri-apps/api/core";

// ── Types matching Rust structs ──

export interface ConnectionInfo {
  id: string;
  name: string;
  data_dir: string | null;
  conn_type: string;
  host: string | null;
  port: number | null;
  username: string;
  engine_version: string;
}

export interface ColumnInfo {
  name: string;
  type_oid: number;
  type_len: number;
}

export interface QueryResponse {
  columns: ColumnInfo[];
  rows: (string | number | null)[][];
  row_count: number;
  duration_us: number;
  command_tag: string;
  truncated: boolean;
  query_id: string;
}

export interface TableOverview {
  name: string;
  row_count: number;
}

export interface ColumnDef {
  name: string;
  type_name: string;
  nullable: boolean;
  position: number;
}

export interface IndexInfo {
  name: string;
  columns: string[];
  unique: boolean;
}

export interface TableDetail {
  name: string;
  columns: ColumnDef[];
  row_count: number;
  indexes: IndexInfo[];
}

export interface NewColumn {
  name: string;
  type_name: string;
}

export interface HistoryEntry {
  id: string;
  timestamp: string;
  sql: string;
  duration_us: number;
  row_count: number;
  error: string | null;
  connection_name: string;
}

export interface BackupResult {
  path: string;
  size_bytes: number;
}

export interface ConnectParams {
  name: string;
  conn_type: string;
  data_dir?: string;
  host?: string;
  port?: number;
  ssh_host?: string;
  ssh_user?: string;
  ssh_key_path?: string;
  username?: string;
  password?: string;
}

export interface EngineInfo {
  version: string;
  studio_version: string;
  os: string;
  arch: string;
  engine_type: string;
  data_dir: string | null;
  uptime_secs: number;
  connection_count: number;
  username: string;
  is_superuser: boolean;
}

export interface EngineDetectResult {
  embedded_available: boolean;
  embedded_version: string;
  standalone_found: boolean;
  standalone_path: string | null;
  standalone_version: string | null;
  os: string;
  arch: string;
  install_hint: string;
}

export interface UserInfo {
  username: string;
  is_superuser: boolean;
}

// ── Connection API ──

export async function connect(
  name: string,
  dataDir?: string
): Promise<ConnectionInfo> {
  return invoke<ConnectionInfo>("connect", {
    name,
    dataDir: dataDir ?? null,
  });
}

export async function disconnect(connectionId: string): Promise<void> {
  return invoke("disconnect", { connectionId });
}

export async function listConnections(): Promise<ConnectionInfo[]> {
  return invoke<ConnectionInfo[]>("list_connections");
}

// ── Query API ──

export async function executeSql(
  connectionId: string,
  sql: string
): Promise<QueryResponse> {
  return invoke<QueryResponse>("execute_sql", { connectionId, sql });
}

export async function cancelQuery(queryId: string): Promise<void> {
  return invoke("cancel_query", { queryId });
}

export async function getHistory(): Promise<HistoryEntry[]> {
  return invoke<HistoryEntry[]>("get_history");
}

// ── Schema API ──

export async function listTables(
  connectionId: string
): Promise<TableOverview[]> {
  return invoke<TableOverview[]>("list_tables", { connectionId });
}

export async function tableDetail(
  connectionId: string,
  tableName: string
): Promise<TableDetail> {
  return invoke<TableDetail>("table_detail", { connectionId, tableName });
}

export async function createTable(
  connectionId: string,
  tableName: string,
  columns: NewColumn[]
): Promise<string> {
  return invoke<string>("create_table", { connectionId, tableName, columns });
}

export async function dropTable(
  connectionId: string,
  tableName: string
): Promise<string> {
  return invoke<string>("drop_table", { connectionId, tableName });
}

export async function createIndex(
  connectionId: string,
  tableName: string,
  indexName: string,
  columns: string[],
  unique: boolean
): Promise<string> {
  return invoke<string>("create_index", {
    connectionId,
    tableName,
    indexName,
    columns,
    unique,
  });
}

export async function dropIndex(
  connectionId: string,
  indexName: string
): Promise<string> {
  return invoke<string>("drop_index", { connectionId, indexName });
}

// ── Backup/Import/Export API ──

export async function backupDatabase(
  connectionId: string,
  outputPath: string
): Promise<BackupResult> {
  return invoke<BackupResult>("backup_database", { connectionId, outputPath });
}

export async function restoreDatabase(
  backupPath: string,
  targetDir: string
): Promise<string> {
  return invoke<string>("restore_database", { backupPath, targetDir });
}

export async function exportCsv(
  connectionId: string,
  tableName: string,
  outputPath: string
): Promise<number> {
  return invoke<number>("export_csv", { connectionId, tableName, outputPath });
}

export async function importCsv(
  connectionId: string,
  tableName: string,
  inputPath: string
): Promise<number> {
  return invoke<number>("import_csv", { connectionId, tableName, inputPath });
}

// ── Advanced Connection API ──

export async function connectAdvanced(
  params: ConnectParams
): Promise<ConnectionInfo> {
  return invoke<ConnectionInfo>("connect_advanced", { params });
}

// ── Row Editing API ──

export async function insertRow(
  connectionId: string,
  tableName: string,
  columns: string[],
  values: string[]
): Promise<string> {
  return invoke<string>("insert_row", {
    connectionId,
    tableName,
    columns,
    values,
  });
}

export async function updateCell(
  connectionId: string,
  tableName: string,
  pkColumn: string,
  pkValue: string,
  column: string,
  newValue: string
): Promise<string> {
  return invoke<string>("update_cell", {
    connectionId,
    tableName,
    pkColumn,
    pkValue,
    column,
    newValue,
  });
}

export async function deleteRow(
  connectionId: string,
  tableName: string,
  pkColumn: string,
  pkValue: string
): Promise<string> {
  return invoke<string>("delete_row", {
    connectionId,
    tableName,
    pkColumn,
    pkValue,
  });
}

// ── Schema Management API ──

export async function truncateTable(
  connectionId: string,
  tableName: string
): Promise<string> {
  return invoke<string>("truncate_table", { connectionId, tableName });
}

export async function addColumn(
  connectionId: string,
  tableName: string,
  columnName: string,
  columnType: string
): Promise<string> {
  return invoke<string>("add_column", {
    connectionId,
    tableName,
    columnName,
    columnType,
  });
}

export async function dropColumn(
  connectionId: string,
  tableName: string,
  columnName: string
): Promise<string> {
  return invoke<string>("drop_column", { connectionId, tableName, columnName });
}

export async function renameTable(
  connectionId: string,
  oldName: string,
  newName: string
): Promise<string> {
  return invoke<string>("rename_table", { connectionId, oldName, newName });
}

// ── Engine Info / Status API ──

export async function engineInfo(
  connectionId: string
): Promise<EngineInfo> {
  return invoke<EngineInfo>("engine_info", { connectionId });
}

export async function detectEngine(): Promise<EngineDetectResult> {
  return invoke<EngineDetectResult>("detect_engine");
}

export async function listUsers(
  connectionId: string
): Promise<UserInfo[]> {
  return invoke<UserInfo[]>("list_users", { connectionId });
}
