//! LogScope 桌面应用（Tauri 层）。

mod commands;
mod state;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::import::open_files,
            commands::import::get_task_status,
            commands::import::cancel_import,
            commands::import::preview_parse,
            commands::query::list_files,
            commands::query::get_timeline,
            commands::query::search_logs,
            commands::query::get_search_status,
            commands::query::search_page,
            commands::query::cancel_search,
            commands::query::close_search,
            commands::detail::get_log_detail,
            commands::detail::get_log_context,
            commands::detail::close_files,
        ])
        .run(tauri::generate_context!())
        .expect("LogScope 启动失败");
}
