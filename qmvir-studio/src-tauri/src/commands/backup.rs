use crate::state::AppState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Serialize)]
pub struct BackupResult {
    pub path: String,
    pub size_bytes: u64,
}

/// Backup a connection's data directory to a .tar.gz file.
#[tauri::command]
pub async fn backup_database(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    output_path: String,
) -> Result<BackupResult, String> {
    let instance = state.get_instance(&connection_id)?;
    let data_dir = instance
        .data_dir
        .as_ref()
        .ok_or("Cannot backup an in-memory database")?
        .clone();

    // Checkpoint before backup
    instance.engine.checkpoint();

    let out = PathBuf::from(&output_path);
    let data_dir_clone = data_dir.clone();

    tokio::task::spawn_blocking(move || {
        create_tar_gz(&data_dir_clone, &out)
    })
    .await
    .map_err(|e| format!("Task error: {}", e))??;

    let metadata = std::fs::metadata(&output_path)
        .map_err(|e| format!("Cannot stat backup: {}", e))?;

    Ok(BackupResult {
        path: output_path,
        size_bytes: metadata.len(),
    })
}

/// Restore a .tar.gz backup to a target directory, then reconnect.
#[tauri::command]
pub async fn restore_database(
    _state: tauri::State<'_, Arc<AppState>>,
    backup_path: String,
    target_dir: String,
) -> Result<String, String> {
    let bp = PathBuf::from(&backup_path);
    let td = PathBuf::from(&target_dir);

    if !bp.exists() {
        return Err("Backup file not found".to_string());
    }

    tokio::task::spawn_blocking(move || extract_tar_gz(&bp, &td))
        .await
        .map_err(|e| format!("Task error: {}", e))??;

    Ok(format!("Restored to {}", target_dir))
}

/// Export a table to CSV.
#[tauri::command]
pub async fn export_csv(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
    output_path: String,
) -> Result<u64, String> {
    // Validate table name
    if table_name.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == ';') {
        return Err("Invalid table name".to_string());
    }

    let instance = state.get_instance(&connection_id)?;
    let sql = format!("SELECT * FROM {}", table_name);
    let result = instance.engine.execute(&sql)?;

    let out_path = PathBuf::from(&output_path);
    let mut wtr = csv::Writer::from_writer(
        std::fs::File::create(&out_path).map_err(|e| format!("Cannot create file: {}", e))?,
    );

    // Header
    let headers: Vec<&str> = result.columns.iter().map(|(n, _, _)| n.as_str()).collect();
    wtr.write_record(&headers)
        .map_err(|e| format!("CSV write error: {}", e))?;

    // Rows
    let mut count: u64 = 0;
    for row in &result.rows {
        let cells: Vec<String> = row
            .iter()
            .map(|cell| match cell {
                None => String::new(),
                Some(b) => String::from_utf8_lossy(b).into_owned(),
            })
            .collect();
        wtr.write_record(&cells)
            .map_err(|e| format!("CSV write error: {}", e))?;
        count += 1;
    }

    wtr.flush().map_err(|e| format!("CSV flush error: {}", e))?;
    Ok(count)
}

/// Import CSV into a table (INSERT rows).
#[tauri::command]
pub async fn import_csv(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
    input_path: String,
) -> Result<u64, String> {
    if table_name.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == ';') {
        return Err("Invalid table name".to_string());
    }

    let instance = state.get_instance(&connection_id)?;
    let path = PathBuf::from(&input_path);
    let mut rdr = csv::Reader::from_path(&path)
        .map_err(|e| format!("Cannot open CSV: {}", e))?;

    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| format!("CSV header error: {}", e))?
        .iter()
        .map(|h| h.to_string())
        .collect();

    let mut count: u64 = 0;
    for record in rdr.records() {
        let record = record.map_err(|e| format!("CSV record error: {}", e))?;
        let values: Vec<String> = record
            .iter()
            .map(|v| format!("'{}'", v.replace('\'', "''")))
            .collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            table_name,
            headers.join(", "),
            values.join(", ")
        );
        instance.engine.execute(&sql)?;
        count += 1;
    }

    Ok(count)
}

// ── Helpers ──

fn create_tar_gz(src_dir: &Path, out_file: &Path) -> Result<(), String> {
    let file =
        std::fs::File::create(out_file).map_err(|e| format!("Cannot create backup file: {}", e))?;
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all("data", src_dir)
        .map_err(|e| format!("Tar error: {}", e))?;
    tar.finish().map_err(|e| format!("Tar finish error: {}", e))?;
    Ok(())
}

fn extract_tar_gz(archive_path: &Path, dest_dir: &Path) -> Result<(), String> {
    let file =
        std::fs::File::open(archive_path).map_err(|e| format!("Cannot open backup: {}", e))?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(dec);
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("Cannot create dir: {}", e))?;
    tar.unpack(dest_dir)
        .map_err(|e| format!("Unpack error: {}", e))?;
    Ok(())
}
