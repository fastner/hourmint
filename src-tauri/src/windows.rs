//! Window helpers: main window, menu bar popover and idle prompt.

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Rect, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};

use crate::state::AppState;

pub const POPOVER_W: f64 = 340.0;
pub const POPOVER_H: f64 = 500.0;

pub fn show_main(app: &AppHandle, route: Option<&str>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        if let Some(r) = route {
            let _ = w.emit("navigate", r.to_string());
        }
    }
    if let Some(p) = app.get_webview_window("popover") {
        let _ = p.hide();
    }
}

pub fn create_popover(app: &AppHandle) -> tauri::Result<()> {
    let win = WebviewWindowBuilder::new(app, "popover", WebviewUrl::App("index.html".into()))
        .title("Hourmint")
        .inner_size(POPOVER_W, POPOVER_H)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .shadow(true)
        .build()?;
    let handle = app.clone();
    win.on_window_event(move |e| {
        if let WindowEvent::Focused(false) = e {
            if let Some(w) = handle.get_webview_window("popover") {
                let _ = w.hide();
                *handle.state::<AppState>().popover_hidden_at.lock().unwrap() =
                    Some(std::time::Instant::now());
            }
        }
    });
    Ok(())
}

/// Toggle the popover just below the tray icon.
pub fn toggle_popover(app: &AppHandle, anchor: Option<Rect>) {
    let Some(w) = app.get_webview_window("popover") else {
        return;
    };
    if w.is_visible().unwrap_or(false) {
        let _ = w.hide();
        return;
    }
    // The click that blurred the popover should not immediately re-open it.
    let recently_hidden = app
        .state::<AppState>()
        .popover_hidden_at
        .lock()
        .unwrap()
        .map(|t| t.elapsed().as_millis() < 250)
        .unwrap_or(false);
    if recently_hidden {
        return;
    }
    if let Some(rect) = anchor {
        let scale = w.scale_factor().unwrap_or(1.0);
        let pos = rect.position.to_physical::<f64>(scale);
        let size = rect.size.to_physical::<f64>(scale);
        let width = POPOVER_W * scale;
        let mut x = pos.x + size.width / 2.0 - width / 2.0;
        if let Ok(Some(m)) = w.current_monitor() {
            let mx = m.position().x as f64;
            let mw = m.size().width as f64;
            x = x.clamp(mx + 8.0, mx + mw - width - 8.0);
        }
        let y = pos.y + size.height + 6.0;
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
    let _ = w.show();
    let _ = w.set_focus();
    let _ = w.emit("popover-shown", ());
}

pub fn show_idle_prompt(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("idle") {
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.emit("idle-detected", ());
        return;
    }
    let built = WebviewWindowBuilder::new(app, "idle", WebviewUrl::App("index.html".into()))
        .title("You were idle")
        .inner_size(440.0, 260.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .always_on_top(true)
        .center()
        .focused(true)
        .build();
    match built {
        Ok(w) => {
            let handle = app.clone();
            w.on_window_event(move |e| {
                // Closing the prompt without choosing keeps the idle time.
                if let WindowEvent::Destroyed = e {
                    let state = handle.state::<AppState>();
                    let mut idle = state.idle.lock().unwrap();
                    if idle.prompt_open {
                        *idle = Default::default();
                    }
                }
            });
        }
        Err(e) => log::error!("could not open idle prompt: {e}"),
    }
}
