//! Tauri commands: thin wrappers that lock the database, call a service and
//! broadcast `data-changed` after mutations.

use std::path::PathBuf;

use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::*;
use crate::services::{clients, entries, export, invoices, reports, settings};
use crate::state::{notify_changed, AppState};
use crate::time;

type S<'a> = State<'a, AppState>;

// ------------------------------------------------------------------ settings

#[tauri::command]
pub fn get_settings(window: tauri::Window, state: S) -> AppResult<Settings> {
    // Every window fetches settings first, so this records that its UI loaded.
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    if let Ok(mut seen) = SEEN.lock() {
        if !seen.iter().any(|l| l == window.label()) {
            seen.push(window.label().to_string());
            log::info!("{} window loaded", window.label());
        }
    }
    state.with_db(settings::get)
}

#[tauri::command]
pub fn update_settings(app: AppHandle, state: S, patch: SettingsPatch) -> AppResult<Settings> {
    let before = state.with_db(settings::get)?;
    if let Some(sc) = &patch.global_shortcut {
        if sc != &before.global_shortcut {
            crate::shortcut::register(&app, sc, Some(&before.global_shortcut))?;
        }
    }
    if let Some(enabled) = patch.launch_at_login {
        if enabled != before.launch_at_login {
            crate::login_item::set(enabled)
                .map_err(|e| AppError::Other(format!("Could not change launch at login: {e}")))?;
        }
    }
    let after = state.with_db(|c| settings::update(c, &patch))?;
    notify_changed(&app);
    Ok(after)
}

#[tauri::command]
pub async fn pick_logo(app: AppHandle, state: S<'_>) -> AppResult<Option<Settings>> {
    let Some(file) = app
        .dialog()
        .file()
        .add_filter("Images", &["png", "jpg", "jpeg"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let src = file
        .into_path()
        .map_err(|e| AppError::Other(e.to_string()))?;
    let meta = std::fs::metadata(&src)?;
    if meta.len() > 5 * 1024 * 1024 {
        return Err(AppError::Validation(
            "The logo must be smaller than 5 MB.".into(),
        ));
    }
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_else(|| "png".into());
    std::fs::create_dir_all(&state.paths.logo_dir)?;
    // Never overwrite: old invoices keep pointing at the logo they were created with.
    let dest = state
        .paths
        .logo_dir
        .join(format!("{}.{ext}", time::new_id()));
    std::fs::copy(&src, &dest)?;
    let s = state.with_db(|c| settings::set_logo_path(c, Some(&dest.to_string_lossy())))?;
    notify_changed(&app);
    Ok(Some(s))
}

#[tauri::command]
pub fn clear_logo(app: AppHandle, state: S) -> AppResult<Settings> {
    let s = state.with_db(|c| settings::set_logo_path(c, None))?;
    notify_changed(&app);
    Ok(s)
}

/// Read a stored logo as a data URL (only files inside the app's logo folder).
#[tauri::command]
pub fn read_logo(state: S, path: String) -> AppResult<Option<String>> {
    let p = PathBuf::from(&path);
    let Ok(canon) = p.canonicalize() else {
        return Ok(None);
    };
    let logo_dir = state
        .paths
        .logo_dir
        .canonicalize()
        .unwrap_or(state.paths.logo_dir.clone());
    if !canon.starts_with(&logo_dir) {
        return Err(AppError::Validation(
            "Logo path is outside the app folder.".into(),
        ));
    }
    let bytes = std::fs::read(&canon)?;
    let mime = match canon
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "image/png",
    };
    Ok(Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )))
}

// ------------------------------------------------------------------ clients & projects

#[tauri::command]
pub fn list_clients(state: S, include_archived: bool) -> AppResult<Vec<Client>> {
    state.with_db(|c| clients::list_clients(c, include_archived))
}

#[tauri::command]
pub fn get_client(state: S, id: String) -> AppResult<Client> {
    state.with_db(|c| clients::get_client(c, &id))
}

#[tauri::command]
pub fn create_client(app: AppHandle, state: S, input: ClientInput) -> AppResult<Client> {
    let r = state.with_db(|c| clients::create_client(c, &input))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn update_client(
    app: AppHandle,
    state: S,
    id: String,
    input: ClientInput,
) -> AppResult<Client> {
    let r = state.with_db(|c| clients::update_client(c, &id, &input))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn set_client_archived(
    app: AppHandle,
    state: S,
    id: String,
    archived: bool,
) -> AppResult<Client> {
    let r = state.with_db(|c| clients::set_client_archived(c, &id, archived))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn delete_client(app: AppHandle, state: S, id: String) -> AppResult<()> {
    state.with_db(|c| clients::delete_client(c, &id))?;
    notify_changed(&app);
    Ok(())
}

#[tauri::command]
pub fn list_projects(
    state: S,
    client_id: Option<String>,
    include_archived: bool,
) -> AppResult<Vec<Project>> {
    state.with_db(|c| clients::list_projects(c, client_id.as_deref(), include_archived))
}

#[tauri::command]
pub fn create_project(app: AppHandle, state: S, input: ProjectInput) -> AppResult<Project> {
    let r = state.with_db(|c| clients::create_project(c, &input))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn update_project(
    app: AppHandle,
    state: S,
    id: String,
    input: ProjectInput,
) -> AppResult<Project> {
    let r = state.with_db(|c| clients::update_project(c, &id, &input))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn set_project_archived(
    app: AppHandle,
    state: S,
    id: String,
    archived: bool,
) -> AppResult<Project> {
    let r = state.with_db(|c| clients::set_project_archived(c, &id, archived))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn delete_project(app: AppHandle, state: S, id: String) -> AppResult<()> {
    state.with_db(|c| clients::delete_project(c, &id))?;
    notify_changed(&app);
    Ok(())
}

// ------------------------------------------------------------------ timer & entries

#[tauri::command]
pub fn get_running(state: S) -> AppResult<Option<Entry>> {
    state.with_db(entries::get_running)
}

#[tauri::command]
pub fn start_timer(app: AppHandle, state: S, input: StartTimerInput) -> AppResult<Entry> {
    let r = state.with_db(|c| entries::start_timer(c, &input, time::now()))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn stop_timer(app: AppHandle, state: S) -> AppResult<Option<Entry>> {
    let r = state.with_db(|c| entries::stop_timer(c, time::now()))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn update_running(app: AppHandle, state: S, patch: RunningPatch) -> AppResult<Option<Entry>> {
    let r = state.with_db(|c| entries::update_running(c, &patch))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn continue_entry(app: AppHandle, state: S, id: String) -> AppResult<Entry> {
    let r = state.with_db(|c| entries::continue_entry(c, &id, time::now()))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn create_entry(app: AppHandle, state: S, input: EntryInput) -> AppResult<SaveEntryResult> {
    let r = state.with_db(|c| entries::create_entry(c, &input, time::now()))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn update_entry(
    app: AppHandle,
    state: S,
    id: String,
    input: EntryInput,
) -> AppResult<SaveEntryResult> {
    let r = state.with_db(|c| entries::update_entry(c, &id, &input, time::now()))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn set_entry_billable(
    app: AppHandle,
    state: S,
    id: String,
    billable: bool,
) -> AppResult<Entry> {
    let r = state.with_db(|c| entries::set_billable(c, &id, billable))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn delete_entry(app: AppHandle, state: S, id: String) -> AppResult<()> {
    state.with_db(|c| entries::delete_entry(c, &id))?;
    notify_changed(&app);
    Ok(())
}

#[tauri::command]
pub fn split_entry(app: AppHandle, state: S, id: String, at: String) -> AppResult<Vec<Entry>> {
    let (a, b) = state.with_db(|c| entries::split_entry(c, &id, &at))?;
    notify_changed(&app);
    Ok(vec![a, b])
}

#[tauri::command]
pub fn find_overlaps(
    state: S,
    started_at: String,
    ended_at: String,
    exclude_id: Option<String>,
) -> AppResult<Vec<Entry>> {
    let start = time::normalise_iso(&started_at)?;
    let end = time::normalise_iso(&ended_at)?;
    state.with_db(|c| entries::find_overlaps(c, &start, &end, exclude_id.as_deref()))
}

#[tauri::command]
pub fn list_entries(state: S, filter: EntryFilter) -> AppResult<Vec<Entry>> {
    state.with_db(|c| entries::list_entries(c, &filter))
}

#[tauri::command]
pub fn recent_combos(state: S, limit: Option<i64>) -> AppResult<Vec<RecentCombo>> {
    state.with_db(|c| entries::recent_combos(c, limit.unwrap_or(8)))
}

#[tauri::command]
pub fn get_summary(state: S, filter: EntryFilter) -> AppResult<Summary> {
    state.with_db(|c| reports::summary(c, &filter))
}

// ------------------------------------------------------------------ idle

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdleInfo {
    pub idle_since: String,
    pub entry: Option<Entry>,
}

#[tauri::command]
pub fn get_idle_state(state: S) -> AppResult<Option<IdleInfo>> {
    let (since, entry_id) = {
        let idle = state.idle.lock().unwrap();
        (idle.idle_since, idle.entry_id.clone())
    };
    let Some(since) = since else { return Ok(None) };
    let entry = match entry_id {
        Some(id) => state.with_db(|c| entries::get_entry(c, &id)).ok(),
        None => None,
    };
    Ok(Some(IdleInfo {
        idle_since: time::to_iso(since),
        entry,
    }))
}

#[tauri::command]
pub fn resolve_idle(app: AppHandle, state: S, action: String) -> AppResult<()> {
    crate::idle::resolve(&state, &action)?;
    notify_changed(&app);
    if let Some(w) = app.get_webview_window("idle") {
        let _ = w.close();
    }
    Ok(())
}

// ------------------------------------------------------------------ invoices

#[tauri::command]
pub fn list_invoices(state: S, filter: InvoiceFilter) -> AppResult<Vec<Invoice>> {
    state.with_db(|c| invoices::list(c, &filter))
}

#[tauri::command]
pub fn invoice_totals(state: S, client_id: Option<String>) -> AppResult<Vec<InvoiceTotals>> {
    state.with_db(|c| invoices::totals(c, client_id.as_deref()))
}

#[tauri::command]
pub fn get_invoice(state: S, id: String) -> AppResult<InvoiceDetail> {
    state.with_db(|c| invoices::get_detail(c, &id))
}

#[tauri::command]
pub fn invoice_candidates(
    state: S,
    client_id: String,
    from: Option<String>,
    to: Option<String>,
    invoice_id: Option<String>,
) -> AppResult<Vec<Entry>> {
    state.with_db(|c| {
        invoices::candidate_entries(
            c,
            &client_id,
            from.as_deref(),
            to.as_deref(),
            invoice_id.as_deref(),
        )
    })
}

#[tauri::command]
pub fn preview_invoice(state: S, input: InvoiceInput) -> AppResult<InvoiceComputation> {
    state.with_db(|c| invoices::preview(c, &input))
}

#[tauri::command]
pub fn next_invoice_number(state: S, issue_date: String) -> AppResult<String> {
    let d = time::parse_date(&issue_date)?;
    state.with_db(|c| invoices::next_number(c, d).map(|(n, _)| n))
}

#[tauri::command]
pub fn save_invoice(app: AppHandle, state: S, input: InvoiceInput) -> AppResult<InvoiceDetail> {
    let r = state.with_db(|c| invoices::save_draft(c, &input))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn set_invoice_status(
    app: AppHandle,
    state: S,
    id: String,
    status: String,
) -> AppResult<Invoice> {
    let r = state.with_db(|c| invoices::set_status(c, &id, &status))?;
    notify_changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn delete_invoice(app: AppHandle, state: S, id: String) -> AppResult<()> {
    state.with_db(|c| invoices::delete(c, &id))?;
    notify_changed(&app);
    Ok(())
}

#[tauri::command]
pub fn timesheet_entries(
    state: S,
    client_id: String,
    from: Option<String>,
    to: Option<String>,
    include_non_billable: bool,
) -> AppResult<Vec<Entry>> {
    state.with_db(|c| {
        invoices::timesheet_entries(
            c,
            &client_id,
            from.as_deref(),
            to.as_deref(),
            include_non_billable,
        )
    })
}

// ------------------------------------------------------------------ files & data

fn save_dialog(
    app: &AppHandle,
    default_name: &str,
    filter_name: &str,
    ext: &str,
) -> AppResult<Option<PathBuf>> {
    let picked = app
        .dialog()
        .file()
        .set_file_name(default_name)
        .add_filter(filter_name, &[ext])
        .blocking_save_file();
    match picked {
        Some(fp) => Ok(Some(
            fp.into_path().map_err(|e| AppError::Other(e.to_string()))?,
        )),
        None => Ok(None),
    }
}

/// Ask where to save, then write the given base64 bytes there. Returns the path, or None if cancelled.
#[tauri::command]
pub async fn save_file(
    app: AppHandle,
    default_name: String,
    filter_name: String,
    extension: String,
    base64_data: String,
) -> AppResult<Option<String>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .map_err(|e| AppError::Other(e.to_string()))?;
    let Some(path) = save_dialog(&app, &default_name, &filter_name, &extension)? else {
        return Ok(None);
    };
    std::fs::write(&path, bytes)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn export_entries_csv(
    app: AppHandle,
    state: S<'_>,
    filter: EntryFilter,
    default_name: String,
) -> AppResult<Option<String>> {
    let bytes = state.with_db(|c| reports::entries_csv(c, &filter))?;
    let Some(path) = save_dialog(&app, &default_name, "CSV", "csv")? else {
        return Ok(None);
    };
    std::fs::write(&path, bytes)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn export_all(app: AppHandle, state: S<'_>, format: String) -> AppResult<Option<String>> {
    let stamp = chrono::Local::now().format("%Y-%m-%d").to_string();
    match format.as_str() {
        "json" => {
            let bytes = state.with_db(export::export_json)?;
            let Some(path) = save_dialog(
                &app,
                &format!("hourmint-export-{stamp}.json"),
                "JSON",
                "json",
            )?
            else {
                return Ok(None);
            };
            std::fs::write(&path, bytes)?;
            Ok(Some(path.to_string_lossy().into_owned()))
        }
        "csv" => {
            let Some(dir) = app
                .dialog()
                .file()
                .set_title("Choose a folder for the CSV files")
                .blocking_pick_folder()
            else {
                return Ok(None);
            };
            let dir = dir
                .into_path()
                .map_err(|e| AppError::Other(e.to_string()))?;
            let target = dir.join(format!("hourmint-export-{stamp}"));
            state.with_db(|c| export::export_csv_dir(c, &target))?;
            Ok(Some(target.to_string_lossy().into_owned()))
        }
        _ => Err(AppError::Validation("Unknown export format.".into())),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataInfo {
    pub db_path: String,
    pub backup_dir: String,
    pub backups: Vec<db::BackupFile>,
    pub schema_version: i64,
    pub app_version: String,
}

#[tauri::command]
pub fn data_info(app: AppHandle, state: S) -> AppResult<DataInfo> {
    Ok(DataInfo {
        db_path: state.paths.db_path.to_string_lossy().into_owned(),
        backup_dir: state.paths.backup_dir.to_string_lossy().into_owned(),
        backups: db::list_backups(&state.paths.backup_dir),
        schema_version: db::schema_version(),
        app_version: app.package_info().version.to_string(),
    })
}

#[tauri::command]
pub fn reveal_path(path: String) -> AppResult<()> {
    tauri_plugin_opener::reveal_item_in_dir(PathBuf::from(path))
        .map_err(|e| AppError::Other(e.to_string()))
}

#[tauri::command]
pub async fn backup_to_file(app: AppHandle, state: S<'_>) -> AppResult<Option<String>> {
    let name = format!(
        "hourmint-backup-{}.sqlite",
        chrono::Local::now().format("%Y-%m-%d-%H%M")
    );
    let Some(path) = save_dialog(&app, &name, "Hourmint backup", "sqlite")? else {
        return Ok(None);
    };
    state.with_db(|c| db::backup_to(c, &path))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub fn backup_now(state: S) -> AppResult<String> {
    std::fs::create_dir_all(&state.paths.backup_dir)?;
    let path = state.paths.backup_dir.join(format!(
        "hourmint-manual-{}.sqlite",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    state.with_db(|c| db::backup_to(c, &path))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Restore from a backup file. `path` = None asks the user to pick one.
#[tauri::command]
pub async fn restore_backup(app: AppHandle, state: S<'_>, path: Option<String>) -> AppResult<bool> {
    let src = match path {
        Some(p) => PathBuf::from(p),
        None => {
            let Some(fp) = app
                .dialog()
                .file()
                .add_filter("Hourmint backup", &["sqlite", "db"])
                .blocking_pick_file()
            else {
                return Ok(false);
            };
            fp.into_path().map_err(|e| AppError::Other(e.to_string()))?
        }
    };
    db::validate_backup_file(&src)?;
    // Safety copy of the current data before overwriting it.
    std::fs::create_dir_all(&state.paths.backup_dir)?;
    let safety = state.paths.backup_dir.join(format!(
        "hourmint-pre-restore-{}.sqlite",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    state.with_db(|c| db::backup_to(c, &safety))?;
    state.with_db_mut(|c| db::restore_from(c, &src))?;
    *state.idle.lock().unwrap() = Default::default();
    let shortcut = state.with_db(settings::get)?.global_shortcut;
    let _ = crate::shortcut::register(&app, &shortcut, None);
    notify_changed(&app);
    Ok(true)
}

// ------------------------------------------------------------------ windows

#[tauri::command]
pub fn show_main_window(app: AppHandle, route: Option<String>) -> AppResult<()> {
    crate::windows::show_main(&app, route.as_deref());
    Ok(())
}

#[tauri::command]
pub fn hide_popover(app: AppHandle) -> AppResult<()> {
    if let Some(w) = app.get_webview_window("popover") {
        let _ = w.hide();
    }
    Ok(())
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// "direct" (GitHub download, self-updating) or "appstore".
    pub distribution: &'static str,
    pub updates: bool,
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    let direct = cfg!(feature = "direct");
    AppInfo {
        distribution: if direct { "direct" } else { "appstore" },
        updates: direct,
    }
}
