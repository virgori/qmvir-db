mod commands;
mod state;

use state::AppState;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            #[cfg(feature = "devtools")]
            {
                let window = app.get_webview_window("main");
                if let Some(w) = window {
                    w.open_devtools();
                }
            }
            Ok(())
        })
        .manage(Arc::new(AppState::new()))
        .invoke_handler(tauri::generate_handler![
            commands::query::execute_sql,
            commands::query::cancel_query,
            commands::query::get_history,
            commands::schema::list_tables,
            commands::schema::table_detail,
            commands::schema::create_table,
            commands::schema::drop_table,
            commands::schema::create_index,
            commands::schema::drop_index,
            commands::schema::truncate_table,
            commands::schema::add_column,
            commands::schema::drop_column,
            commands::schema::rename_table,
            commands::connection::connect,
            commands::connection::connect_advanced,
            commands::connection::disconnect,
            commands::connection::list_connections,
            commands::connection::insert_row,
            commands::connection::update_cell,
            commands::connection::delete_row,
            commands::connection::engine_info,
            commands::connection::detect_engine,
            commands::connection::list_users,
            commands::backup::backup_database,
            commands::backup::restore_database,
            commands::backup::export_csv,
            commands::backup::import_csv,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
