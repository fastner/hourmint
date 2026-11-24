use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Connection;
use tauri::{AppHandle, Emitter, Manager};

use crate::error::{AppError, AppResult};
use crate::services::entries;
use crate::time::parse_iso;

pub struct Paths {
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub backup_dir: PathBuf,
    pub logo_dir: PathBuf,
}

#[derive(Default)]
pub struct IdleState {
    /// When the current idle period began (UTC).
    pub idle_since: Option<DateTime<Utc>>,
    /// The running entry the idle period belongs to.
    pub entry_id: Option<String>,
    /// The idle prompt is on screen, waiting for an answer.
    pub prompt_open: bool,
}

#[derive(Default)]
pub struct MonitorState {
    pub long_warned_entry: Option<String>,
    pub last_reminder: Option<NaiveDate>,
    pub last_backup_check: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct RunningCache {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub label: String,
}

pub struct AppState {
    pub db: Mutex<Connection>,
    pub paths: Paths,
    pub idle: Mutex<IdleState>,
    pub monitor: Mutex<MonitorState>,
    pub running: Mutex<Option<RunningCache>>,
    /// Last time the popover lost focus, to avoid re-opening it on the same tray click.
    pub popover_hidden_at: Mutex<Option<std::time::Instant>>,
}

impl AppState {
    pub fn with_db<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        let conn = self
            .db
            .lock()
            .map_err(|_| AppError::Other("Database lock poisoned".into()))?;
        f(&conn)
    }

    pub fn with_db_mut<T>(&self, f: impl FnOnce(&mut Connection) -> AppResult<T>) -> AppResult<T> {
        let mut conn = self
            .db
            .lock()
            .map_err(|_| AppError::Other("Database lock poisoned".into()))?;
        f(&mut conn)
    }

    pub fn refresh_running(&self) -> AppResult<()> {
        let running = self.with_db(entries::get_running)?;
        let cache = match running {
            Some(e) => Some(RunningCache {
                id: e.id,
                started_at: parse_iso(&e.started_at)?,
                label: if e.description.is_empty() {
                    format!("{} · {}", e.client_name, e.project_name)
                } else {
                    format!("{} · {}", e.project_name, e.description)
                },
            }),
            None => None,
        };
        *self.running.lock().unwrap() = cache;
        Ok(())
    }
}

/// Call after any mutation: refresh the tray's cached timer and tell every window to refetch.
pub fn notify_changed(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Err(e) = state.refresh_running() {
        log::error!("refresh running failed: {e}");
    }
    crate::tray::refresh(app);
    let _ = app.emit("data-changed", ());
}
