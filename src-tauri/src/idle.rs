//! Background monitor: idle detection, long-timer warning,
//! start reminder and the daily automatic backup.

use chrono::{DateTime, Datelike, Duration, Local, Utc};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::error::{validation, AppResult};
use crate::models::StartTimerInput;
use crate::services::{entries, settings};
use crate::state::AppState;
use crate::time;

const TICK_SECS: u64 = 5;
/// Input within this many seconds counts as "the user is back".
const ACTIVE_SECS: f64 = 10.0;

/// Seconds since the last keyboard or mouse input, system-wide.
#[cfg(target_os = "macos")]
pub fn system_idle_seconds() -> f64 {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(source: i32, event_type: u32) -> f64;
    }
    // kCGEventSourceStateCombinedSessionState = 0, kCGAnyInputEventType = ~0
    unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) }
}

#[cfg(target_os = "windows")]
pub fn system_idle_seconds() -> f64 {
    #[repr(C)]
    struct LastInputInfo {
        cb_size: u32,
        dw_time: u32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetLastInputInfo(plii: *mut LastInputInfo) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetTickCount() -> u32;
    }
    let mut info = LastInputInfo {
        cb_size: std::mem::size_of::<LastInputInfo>() as u32,
        dw_time: 0,
    };
    unsafe {
        if GetLastInputInfo(&mut info) == 0 {
            return 0.0;
        }
        GetTickCount().wrapping_sub(info.dw_time) as f64 / 1000.0
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn system_idle_seconds() -> f64 {
    0.0
}

/// Pure idle-tracking step, separated from the OS for testing.
/// Returns true when the prompt should be shown.
pub fn idle_step(
    idle: &mut crate::state::IdleState,
    running: Option<(&str, DateTime<Utc>)>,
    threshold_secs: i64,
    idle_secs: f64,
    now: DateTime<Utc>,
    prev_tick: DateTime<Utc>,
) -> bool {
    let Some((entry_id, started_at)) = running else {
        *idle = Default::default();
        return false;
    };
    if threshold_secs <= 0 {
        *idle = Default::default();
        return false;
    }
    if idle.prompt_open {
        return false;
    }
    if idle.entry_id.as_deref().is_some_and(|id| id != entry_id) {
        *idle = Default::default();
    }
    let gap = (now - prev_tick).num_seconds();
    let mut candidate: Option<DateTime<Utc>> = None;
    // A long gap between ticks means the machine was asleep.
    if gap > threshold_secs.max(60) {
        candidate = Some(prev_tick);
    }
    if idle_secs >= threshold_secs as f64 {
        let since = now - Duration::milliseconds((idle_secs * 1000.0) as i64);
        candidate = Some(candidate.map_or(since, |c| c.min(since)));
    }
    if let Some(c) = candidate {
        let c = c.max(started_at);
        idle.idle_since = Some(idle.idle_since.map_or(c, |s| s.min(c)));
        idle.entry_id = Some(entry_id.to_string());
    }
    if idle.idle_since.is_some() && idle_secs < ACTIVE_SECS {
        idle.prompt_open = true;
        return true;
    }
    false
}

/// Apply the user's choice from the idle prompt.
pub fn resolve(state: &AppState, action: &str) -> AppResult<()> {
    let (since, entry_id) = {
        let idle = state.idle.lock().unwrap();
        (idle.idle_since, idle.entry_id.clone())
    };
    if let (Some(since), Some(entry_id)) = (since, entry_id) {
        state.with_db(|c| apply(c, action, &entry_id, since, time::now()))?;
    }
    // Clear only once the choice is saved, so a failure leaves the prompt answerable.
    *state.idle.lock().unwrap() = Default::default();
    Ok(())
}

/// The database side of an idle choice for the running entry `entry_id`.
fn apply(
    c: &rusqlite::Connection,
    action: &str,
    entry_id: &str,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let Some(running) = entries::get_running(c)? else {
        return Ok(());
    };
    if running.id != entry_id {
        return Ok(());
    }
    match action {
        "keep" => Ok(()),
        // Close the entry when idling began, then carry on from now.
        "discard" => entries::restart_timer(
            c,
            &StartTimerInput {
                project_id: running.project_id.clone(),
                description: running.description.clone(),
                billable: running.billable,
            },
            since,
            now,
        )
        .map(|_| ()),
        "discard_stop" => entries::stop_timer(c, since).map(|_| ()),
        _ => validation("Unknown idle action."),
    }
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        log::warn!("notification failed: {e}");
    }
}

pub fn spawn_monitor(app: AppHandle) {
    log::info!(
        "idle detection: system reports {:?}s since last input",
        system_idle_seconds()
    );
    std::thread::spawn(move || {
        let mut prev_tick = Utc::now();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(TICK_SECS));
            let now = Utc::now();
            if let Err(e) = tick(&app, now, prev_tick) {
                log::error!("monitor tick failed: {e}");
            }
            prev_tick = now;
        }
    });
}

fn tick(app: &AppHandle, now: DateTime<Utc>, prev_tick: DateTime<Utc>) -> AppResult<()> {
    let state = app.state::<AppState>();
    let s = state.with_db(settings::get)?;
    let running = state.running.lock().unwrap().clone();

    // Idle detection.
    let show = {
        let mut idle = state.idle.lock().unwrap();
        idle_step(
            &mut idle,
            running.as_ref().map(|r| (r.id.as_str(), r.started_at)),
            s.idle_threshold_minutes * 60,
            system_idle_seconds(),
            now,
            prev_tick,
        )
    };
    if show {
        crate::windows::show_idle_prompt(app);
    }

    let mut mon = state.monitor.lock().unwrap();

    // Long-running timer warning, once per entry.
    if let Some(r) = &running {
        let limit = (s.long_timer_hours * 3600.0) as i64;
        if limit > 0
            && (now - r.started_at).num_seconds() >= limit
            && mon.long_warned_entry.as_ref() != Some(&r.id)
        {
            mon.long_warned_entry = Some(r.id.clone());
            notify(
                app,
                "Timer still running",
                &format!(
                    "Your timer has been running for over {} hours. Forgot to stop it?",
                    s.long_timer_hours
                ),
            );
        }
    }

    // Reminder to start a timer on chosen weekdays (within 3 hours after the set time).
    if s.reminder_enabled && running.is_none() {
        let local = now.with_timezone(&Local);
        let today = local.date_naive();
        let weekday = local.weekday().number_from_monday().to_string();
        let on_day = s.reminder_days.split(',').any(|d| d.trim() == weekday);
        if on_day && mon.last_reminder != Some(today) {
            if let Ok(t) = chrono::NaiveTime::parse_from_str(&s.reminder_time, "%H:%M") {
                let mins = (local.time() - t).num_minutes();
                if (0..180).contains(&mins) {
                    let start = time::to_iso(time::local_day_start(today));
                    let started_today: i64 = state.with_db(|c| {
                        Ok(c.query_row(
                            "SELECT COUNT(*) FROM time_entries WHERE deleted_at IS NULL AND started_at >= ?1",
                            [start],
                            |r| r.get(0),
                        )?)
                    })?;
                    mon.last_reminder = Some(today);
                    if started_today == 0 {
                        notify(
                            app,
                            "Time to start tracking",
                            "No timer has been started today.",
                        );
                    }
                }
            }
        }
    }

    // Daily automatic backup, checked hourly.
    if mon
        .last_backup_check
        .map_or(true, |t| now - t > Duration::hours(1))
    {
        mon.last_backup_check = Some(now);
        let dir = state.paths.backup_dir.clone();
        if let Err(e) = state.with_db(|c| crate::db::daily_backup(c, &dir)) {
            log::error!("daily backup failed: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::IdleState;
    use crate::time::parse_iso;

    fn t(s: &str) -> DateTime<Utc> {
        parse_iso(s).unwrap()
    }

    #[test]
    fn detects_idle_then_prompts_on_return() {
        let mut st = IdleState::default();
        let start = t("2026-01-01T09:00:00Z");
        let run = Some(("e1", start));
        // 11 minutes idle, threshold 10: record but don't prompt while still idle.
        let now = t("2026-01-01T10:00:00Z");
        assert!(!idle_step(
            &mut st,
            run,
            600,
            660.0,
            now,
            now - Duration::seconds(5)
        ));
        assert_eq!(st.idle_since, Some(t("2026-01-01T09:49:00Z")));
        // User comes back.
        let later = t("2026-01-01T10:05:00Z");
        assert!(idle_step(
            &mut st,
            run,
            600,
            1.0,
            later,
            later - Duration::seconds(5)
        ));
        assert!(st.prompt_open);
        assert_eq!(st.idle_since, Some(t("2026-01-01T09:49:00Z")));
    }

    #[test]
    fn sleep_gap_counts_as_idle_and_disabled_threshold_resets() {
        let mut st = IdleState::default();
        let run = Some(("e1", t("2026-01-01T09:00:00Z")));
        let prev = t("2026-01-01T09:30:00Z");
        let now = t("2026-01-01T11:00:00Z");
        assert!(idle_step(&mut st, run, 600, 2.0, now, prev));
        assert_eq!(st.idle_since, Some(prev));
        let mut st2 = IdleState::default();
        assert!(!idle_step(&mut st2, run, 0, 5000.0, now, prev));
        assert!(st2.idle_since.is_none());
    }

    #[test]
    fn resolve_discard_splits_entry() {
        use crate::services::entries::tests::setup;
        let (conn, pid) = setup();
        let e = entries::start_timer(
            &conn,
            &StartTimerInput {
                project_id: pid,
                description: "x".into(),
                billable: true,
            },
            t("2026-01-01T09:00:00Z"),
        )
        .unwrap();
        let since = t("2026-01-01T09:30:00Z");
        let now = t("2026-01-01T10:00:00Z");
        apply(&conn, "discard", &e.id, since, now).unwrap();
        let first = entries::get_entry(&conn, &e.id).unwrap();
        assert_eq!(first.duration_seconds, 1800);
        let next = entries::get_running(&conn)
            .unwrap()
            .expect("a new timer runs");
        assert_ne!(next.id, e.id);
        assert_eq!(parse_iso(&next.started_at).unwrap(), now);
        assert_eq!(next.description, "x");
        assert!(conn.is_autocommit(), "no transaction left open");
    }

    #[test]
    fn resolve_discard_stop_ends_entry_at_idle_start() {
        use crate::services::entries::tests::setup;
        let (conn, pid) = setup();
        let e = entries::start_timer(
            &conn,
            &StartTimerInput {
                project_id: pid,
                description: String::new(),
                billable: true,
            },
            t("2026-01-01T09:00:00Z"),
        )
        .unwrap();
        apply(
            &conn,
            "discard_stop",
            &e.id,
            t("2026-01-01T09:20:00Z"),
            t("2026-01-01T10:00:00Z"),
        )
        .unwrap();
        assert_eq!(
            entries::get_entry(&conn, &e.id).unwrap().duration_seconds,
            1200
        );
        assert!(entries::get_running(&conn).unwrap().is_none());
    }
}
