//! Menu bar icon with elapsed time, left-click popover, right-click menu.

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

pub const TRAY_ID: &str = "hourmint-tray";

/// Draw a simple clock glyph as RGBA (used as a macOS template image).
fn clock_icon(size: u32, running: bool) -> Vec<u8> {
    let mut px = vec![0u8; (size * size * 4) as usize];
    let c = size as f32 / 2.0;
    let r_outer = size as f32 * 0.46;
    let stroke = size as f32 * 0.085;
    let hand = |x: f32, y: f32, ax: f32, ay: f32, len: f32| -> f32 {
        // Distance from point to the segment centre→(centre + a*len).
        let (bx, by) = (c + ax * len, c + ay * len);
        let (vx, vy) = (bx - c, by - c);
        let t = (((x - c) * vx + (y - c) * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
        let (qx, qy) = (c + vx * t, c + vy * t);
        ((x - qx).powi(2) + (y - qy).powi(2)).sqrt()
    };
    for yi in 0..size {
        for xi in 0..size {
            let (x, y) = (xi as f32 + 0.5, yi as f32 + 0.5);
            let d = ((x - c).powi(2) + (y - c).powi(2)).sqrt();
            let mut a: f32 = if running {
                // Filled disc when running; hands are cut out below.
                (r_outer - d + 0.5).clamp(0.0, 1.0)
            } else {
                (stroke / 2.0 - (d - (r_outer - stroke / 2.0)).abs() + 0.5).clamp(0.0, 1.0)
            };
            let h1 = hand(x, y, 0.0, -1.0, size as f32 * 0.28);
            let h2 = hand(
                x,
                y,
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
                size as f32 * 0.2,
            );
            let hands = (stroke / 2.0 - h1.min(h2) + 0.5).clamp(0.0, 1.0);
            a = if running {
                a * (1.0 - hands)
            } else {
                a.max(hands)
            };
            let i = ((yi * size + xi) * 4) as usize;
            px[i + 3] = (a * 255.0) as u8;
        }
    }
    px
}

fn icon(running: bool) -> Image<'static> {
    let size = 36;
    Image::new_owned(clock_icon(size, running), size, size)
}

pub fn format_elapsed(secs: i64) -> String {
    let s = secs.max(0);
    format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Hourmint", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Start / Stop Timer", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Hourmint", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&open, &toggle, &PredefinedMenuItem::separator(app)?, &quit],
    )?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon(false))
        .icon_as_template(true)
        .tooltip("Hourmint")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => crate::windows::show_main(app, None),
            "toggle" => crate::shortcut::toggle_timer(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                crate::windows::toggle_popover(tray.app_handle(), Some(rect));
            }
        })
        .build(app)?;
    Ok(())
}

/// Update icon + title from the cached running timer. Called every second and after changes.
static LAST_TITLE: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let running = app.state::<AppState>().running.lock().unwrap().clone();
    let (title, tooltip) = match running {
        Some(r) => (
            format_elapsed((chrono::Utc::now() - r.started_at).num_seconds()),
            format!("Hourmint — {}", r.label),
        ),
        None => (String::new(), "Hourmint — no timer running".to_string()),
    };
    let mut last = LAST_TITLE.lock().unwrap();
    if last.as_ref() == Some(&(title.clone(), tooltip.clone())) {
        return;
    }
    let _ = tray.set_title(if title.is_empty() {
        None
    } else {
        Some(title.as_str())
    });
    let _ = tray.set_tooltip(Some(tooltip.as_str()));
    *last = Some((title, tooltip));
}

/// Swap the icon only when the running state flips (cheap per-second refresh otherwise).
pub fn refresh_icon(app: &AppHandle, running: bool) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_icon(Some(icon(running)));
        let _ = tray.set_icon_as_template(true);
    }
}

pub fn spawn_ticker(app: AppHandle) {
    std::thread::spawn(move || {
        let mut was_running: Option<bool> = None;
        loop {
            let running = app.state::<AppState>().running.lock().unwrap().is_some();
            if was_running != Some(running) {
                refresh_icon(&app, running);
                was_running = Some(running);
            }
            refresh(&app);
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_elapsed() {
        assert_eq!(super::format_elapsed(0), "0:00:00");
        assert_eq!(super::format_elapsed(3725), "1:02:05");
    }
}
