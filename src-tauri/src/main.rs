// Hide the extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use tauri::Manager;
use twinpane_core::api::Api;

/// The single bridge between the UI and the engine. All logic (and all path
/// validation) lives in `twinpane_core::api`, which is covered by tests.
#[tauri::command(async)]
fn call(
    api: tauri::State<'_, Arc<Api>>,
    cmd: String,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    api.call(&cmd, args)
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let api = Api::new(dir.join("holding"), Some(dir.join("undo.json")), None)?;
            app.manage(Arc::new(api));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![call])
        .run(tauri::generate_context!())
        .expect("error while running Twinpane");
}
