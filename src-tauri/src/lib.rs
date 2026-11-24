mod commands;
mod db;
mod error;
mod idle;
mod login_item;
mod models;
mod services;
mod shortcut;
mod state;
mod time;
mod tray;
mod windows;

use std::sync::Mutex;

use tauri::{Manager, RunEvent, WindowEvent};
use tauri_plugin_global_shortcut::ShortcutState;

use state::{AppState, Paths};

pub fn run() {
    let builder = tauri::Builder::default();
    // Direct-download build only: the App Store build is sandboxed, can't update itself,
    // and macOS already keeps App Store apps to a single instance.
    #[cfg(feature = "direct")]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            windows::show_main(app, None);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());
    let app = builder
        // Local log file only (~/Library/Logs/app.hourmint.desktop/), for support requests.
        .plugin(
            // Default targets: stdout and the log directory.
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .max_file_size(1_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        shortcut::toggle_timer(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let paths = Paths {
                db_path: data_dir.join(db::DB_FILE),
                backup_dir: data_dir.join("backups"),
                logo_dir: data_dir.join("logos"),
                data_dir,
            };
            let conn = db::open(&paths.db_path, &paths.backup_dir)
                .map_err(|e| Box::<dyn std::error::Error>::from(e.to_string()))?;
            log::info!(
                "Hourmint {} ({}) starting; data at {}",
                app.package_info().version,
                if cfg!(feature = "direct") {
                    "direct"
                } else {
                    "app store"
                },
                paths.data_dir.display()
            );
            let mut settings = services::settings::get(&conn).map_err(|e| e.to_string())?;
            // Launch at login: migrate the old LaunchAgent, then trust what macOS reports
            // (the user can also change it in System Settings → Login Items).
            #[cfg(all(target_os = "macos", feature = "direct"))]
            if login_item::remove_legacy_launch_agent() && settings.launch_at_login {
                if let Err(e) = login_item::set(true) {
                    log::warn!("launch at login: {e}");
                }
            }
            let at_login = login_item::is_enabled();
            if at_login != settings.launch_at_login {
                let patch = models::SettingsPatch {
                    launch_at_login: Some(at_login),
                    ..Default::default()
                };
                settings = services::settings::update(&conn, &patch).map_err(|e| e.to_string())?;
            }
            let launched_at_login = login_item::launched_at_login();
            app.manage(AppState {
                db: Mutex::new(conn),
                paths,
                idle: Mutex::new(Default::default()),
                monitor: Mutex::new(Default::default()),
                running: Mutex::new(None),
                popover_hidden_at: Mutex::new(None),
            });
            let handle = app.handle().clone();
            app.state::<AppState>()
                .refresh_running()
                .map_err(|e| e.to_string())?;

            tray::create(&handle)?;
            windows::create_popover(&handle)?;
            if let Err(e) = shortcut::register(&handle, &settings.global_shortcut, None) {
                log::warn!("global shortcut: {e}");
            }
            tray::spawn_ticker(handle.clone());
            idle::spawn_monitor(handle.clone());

            // Closing the main window keeps the app running in the menu bar.
            if let Some(main) = app.get_webview_window("main") {
                let w = main.clone();
                main.on_window_event(move |e| {
                    if let WindowEvent::CloseRequested { api, .. } = e {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
                let hidden = launched_at_login || std::env::args().any(|a| a == "--hidden");
                if !hidden {
                    let _ = main.show();
                    let _ = main.set_focus();
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::update_settings,
            commands::pick_logo,
            commands::clear_logo,
            commands::read_logo,
            commands::list_clients,
            commands::get_client,
            commands::create_client,
            commands::update_client,
            commands::set_client_archived,
            commands::delete_client,
            commands::list_projects,
            commands::create_project,
            commands::update_project,
            commands::set_project_archived,
            commands::delete_project,
            commands::get_running,
            commands::start_timer,
            commands::stop_timer,
            commands::update_running,
            commands::continue_entry,
            commands::create_entry,
            commands::update_entry,
            commands::set_entry_billable,
            commands::delete_entry,
            commands::split_entry,
            commands::find_overlaps,
            commands::list_entries,
            commands::recent_combos,
            commands::get_summary,
            commands::get_idle_state,
            commands::resolve_idle,
            commands::list_invoices,
            commands::invoice_totals,
            commands::get_invoice,
            commands::invoice_candidates,
            commands::preview_invoice,
            commands::next_invoice_number,
            commands::save_invoice,
            commands::set_invoice_status,
            commands::delete_invoice,
            commands::timesheet_entries,
            commands::save_file,
            commands::export_entries_csv,
            commands::export_all,
            commands::data_info,
            commands::reveal_path,
            commands::backup_to_file,
            commands::backup_now,
            commands::restore_backup,
            commands::show_main_window,
            commands::hide_popover,
            commands::quit_app,
            commands::app_info,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Hourmint");

    app.run(|handle, event| {
        // Clicking the Dock icon re-opens the main window.
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            windows::show_main(handle, None);
        }
        let _ = (handle, &event);
    });
}
