//! Global start/stop shortcut.

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use crate::error::{AppError, AppResult};
use crate::models::StartTimerInput;
use crate::services::entries;
use crate::state::{notify_changed, AppState};
use crate::time;

/// Replace the registered shortcut with `accelerator`. On failure the previous
/// one (if given) is restored and an error is returned. Empty = disabled.
pub fn register(app: &AppHandle, accelerator: &str, previous: Option<&str>) -> AppResult<()> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if accelerator.trim().is_empty() {
        return Ok(());
    }
    let parsed: Shortcut = accelerator
        .parse()
        .map_err(|_| AppError::Validation(format!("\"{accelerator}\" is not a valid shortcut.")))?;
    if let Err(e) = gs.register(parsed) {
        if let Some(prev) = previous.filter(|p| !p.is_empty()) {
            if let Ok(p) = prev.parse::<Shortcut>() {
                let _ = gs.register(p);
            }
        }
        return Err(AppError::Validation(format!(
            "Could not register {accelerator}; another app may be using it. ({e})"
        )));
    }
    Ok(())
}

/// Stop the running timer, or start one on the last used project.
pub fn toggle_timer(app: &AppHandle) {
    let state = app.state::<AppState>();
    let result = state.with_db(|c| {
        if entries::get_running(c)?.is_some() {
            entries::stop_timer(c, time::now())?;
            return Ok(true);
        }
        match entries::last_used_project(c)? {
            Some(project_id) => {
                entries::start_timer(
                    c,
                    &StartTimerInput {
                        project_id,
                        description: String::new(),
                        billable: true,
                    },
                    time::now(),
                )?;
                Ok(true)
            }
            None => Ok(false),
        }
    });
    match result {
        Ok(true) => notify_changed(app),
        Ok(false) => crate::windows::show_main(app, Some("/")),
        Err(e) => log::error!("shortcut toggle failed: {e}"),
    }
}
