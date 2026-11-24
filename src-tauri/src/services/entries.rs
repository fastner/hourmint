//! Time entries and the running timer.
//!
//! The running timer is simply the (single) entry whose `ended_at` is NULL; its
//! elapsed time is always derived from `started_at`, never from a counter.

use chrono::{DateTime, Utc};
use rusqlite::{params, types::ToSql, Connection, OptionalExtension, Row};

use crate::error::{validation, AppError, AppResult};
use crate::models::{
    Entry, EntryFilter, EntryInput, RecentCombo, RunningPatch, SaveEntryResult, StartTimerInput,
};
use crate::services::clients::get_project;
use crate::time::{self, parse_iso, seconds_between, to_iso};

pub const ENTRY_SELECT: &str = "
    SELECT e.id, e.project_id, p.name, p.color, c.id, c.name, c.currency, e.description,
           e.started_at, e.ended_at, e.duration_seconds, e.billable, e.source, e.invoice_id,
           i.number, i.status, COALESCE(e.invoice_rate, p.rate_override, c.default_rate)
    FROM time_entries e
    JOIN projects p ON p.id = e.project_id
    JOIN clients c ON c.id = p.client_id
    LEFT JOIN invoices i ON i.id = e.invoice_id AND i.deleted_at IS NULL
    WHERE e.deleted_at IS NULL";

pub fn map_entry(r: &Row) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: r.get(0)?,
        project_id: r.get(1)?,
        project_name: r.get(2)?,
        project_color: r.get(3)?,
        client_id: r.get(4)?,
        client_name: r.get(5)?,
        currency: r.get(6)?,
        description: r.get(7)?,
        started_at: r.get(8)?,
        ended_at: r.get(9)?,
        duration_seconds: r.get(10)?,
        billable: r.get(11)?,
        source: r.get(12)?,
        invoice_id: r.get(13)?,
        invoice_number: r.get(14)?,
        invoice_status: r.get(15)?,
        rate: r.get(16)?,
    })
}

pub fn get_entry(conn: &Connection, id: &str) -> AppResult<Entry> {
    let sql = format!("{ENTRY_SELECT} AND e.id = ?1");
    conn.query_row(&sql, [id], map_entry)
        .optional()?
        .ok_or_else(|| AppError::NotFound("time entry".into()))
}

pub fn get_running(conn: &Connection) -> AppResult<Option<Entry>> {
    let sql = format!("{ENTRY_SELECT} AND e.ended_at IS NULL LIMIT 1");
    Ok(conn.query_row(&sql, [], map_entry).optional()?)
}

fn ensure_active_project(conn: &Connection, project_id: &str) -> AppResult<()> {
    let p = get_project(conn, project_id)?;
    if p.archived_at.is_some() || p.client_archived {
        return validation("This project or its client is archived.");
    }
    Ok(())
}

fn ensure_unlocked(e: &Entry) -> AppResult<()> {
    if e.invoice_id.is_some() {
        return Err(AppError::Conflict(format!(
            "This entry is on invoice {} and is locked. Remove it from the draft invoice or delete the invoice first.",
            e.invoice_number.clone().unwrap_or_default()
        )));
    }
    Ok(())
}

/// Stop the running timer (if any) at `at`. Returns the finished entry.
pub fn stop_timer(conn: &Connection, at: DateTime<Utc>) -> AppResult<Option<Entry>> {
    let Some(running) = get_running(conn)? else {
        return Ok(None);
    };
    let start = parse_iso(&running.started_at)?;
    let end = at.max(start);
    conn.execute(
        "UPDATE time_entries SET ended_at = ?2, duration_seconds = ?3, updated_at = ?4 WHERE id = ?1",
        params![running.id, to_iso(end), seconds_between(start, end), time::now_iso()],
    )?;
    get_entry(conn, &running.id).map(Some)
}

/// Start a new timer at `at`, stopping and saving any running one first.
pub fn start_timer(
    conn: &Connection,
    input: &StartTimerInput,
    at: DateTime<Utc>,
) -> AppResult<Entry> {
    restart_timer(conn, input, at, at)
}

/// Stop any running timer at `stop_at` and start a new one at `start_at`, atomically.
/// Opens its own transaction, so callers must not wrap it in one.
pub fn restart_timer(
    conn: &Connection,
    input: &StartTimerInput,
    stop_at: DateTime<Utc>,
    start_at: DateTime<Utc>,
) -> AppResult<Entry> {
    ensure_active_project(conn, &input.project_id)?;
    let tx = conn.unchecked_transaction()?;
    stop_timer(&tx, stop_at)?;
    let id = time::new_id();
    let now = time::now_iso();
    tx.execute(
        "INSERT INTO time_entries (id, project_id, description, started_at, billable, source, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'timer', ?6, ?6)",
        params![id, input.project_id, input.description.trim(), to_iso(start_at), input.billable, now],
    )?;
    tx.commit()?;
    get_entry(conn, &id)
}

/// Change client/project/description of the running timer.
pub fn update_running(conn: &Connection, patch: &RunningPatch) -> AppResult<Option<Entry>> {
    let Some(running) = get_running(conn)? else {
        return Ok(None);
    };
    if let Some(pid) = &patch.project_id {
        if pid != &running.project_id {
            ensure_active_project(conn, pid)?;
        }
    }
    conn.execute(
        "UPDATE time_entries SET project_id = ?2, description = ?3, billable = ?4, updated_at = ?5 WHERE id = ?1",
        params![
            running.id,
            patch.project_id.as_ref().unwrap_or(&running.project_id),
            patch.description.as_deref().map(str::trim).unwrap_or(&running.description),
            patch.billable.unwrap_or(running.billable),
            time::now_iso()
        ],
    )?;
    get_entry(conn, &running.id).map(Some)
}

/// "Continue" a past entry: new timer, same project/description/billable.
pub fn continue_entry(conn: &Connection, id: &str, at: DateTime<Utc>) -> AppResult<Entry> {
    let e = get_entry(conn, id)?;
    start_timer(
        conn,
        &StartTimerInput {
            project_id: e.project_id,
            description: e.description,
            billable: e.billable,
        },
        at,
    )
}

/// The project used for the most recent entry (for the global shortcut).
pub fn last_used_project(conn: &Connection) -> AppResult<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT e.project_id FROM time_entries e
             JOIN projects p ON p.id = e.project_id JOIN clients c ON c.id = p.client_id
             WHERE e.deleted_at IS NULL AND p.archived_at IS NULL AND p.deleted_at IS NULL
               AND c.archived_at IS NULL AND c.deleted_at IS NULL
             ORDER BY e.started_at DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

/// Finished entries overlapping [start, end), excluding `exclude_id`.
pub fn find_overlaps(
    conn: &Connection,
    start: &str,
    end: &str,
    exclude_id: Option<&str>,
) -> AppResult<Vec<Entry>> {
    let sql = format!(
        "{ENTRY_SELECT} AND e.ended_at IS NOT NULL AND e.started_at < ?1 AND e.ended_at > ?2 AND e.id != ?3
         ORDER BY e.started_at"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![end, start, exclude_id.unwrap_or("")], map_entry)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

struct ValidRange {
    start: String,
    end: String,
    seconds: i64,
}

/// End after start, not in the future, no overlap with the running timer.
fn validate_range(
    conn: &Connection,
    started_at: &str,
    ended_at: &str,
    now: DateTime<Utc>,
) -> AppResult<ValidRange> {
    let start = parse_iso(started_at)?;
    let end = parse_iso(ended_at)?;
    if end <= start {
        return validation("End time must be after start time.");
    }
    if end > now + chrono::Duration::minutes(1) {
        return validation("Entries cannot end in the future.");
    }
    if (end - start).num_hours() >= 24 * 7 {
        return validation("An entry cannot be longer than 7 days.");
    }
    if let Some(running) = get_running(conn)? {
        let rs = parse_iso(&running.started_at)?;
        if end > rs {
            return validation(
                "This entry overlaps the running timer. Stop the timer or pick another time.",
            );
        }
    }
    Ok(ValidRange {
        start: to_iso(start),
        end: to_iso(end),
        seconds: seconds_between(start, end),
    })
}

/// Add a manual entry.
pub fn create_entry(
    conn: &Connection,
    input: &EntryInput,
    now: DateTime<Utc>,
) -> AppResult<SaveEntryResult> {
    ensure_active_project(conn, &input.project_id)?;
    let r = validate_range(conn, &input.started_at, &input.ended_at, now)?;
    let id = time::new_id();
    let ts = time::now_iso();
    conn.execute(
        "INSERT INTO time_entries (id, project_id, description, started_at, ended_at, duration_seconds, billable, source, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'manual', ?8, ?8)",
        params![id, input.project_id, input.description.trim(), r.start, r.end, r.seconds, input.billable, ts],
    )?;
    let overlaps = find_overlaps(conn, &r.start, &r.end, Some(&id))?;
    Ok(SaveEntryResult {
        entry: get_entry(conn, &id)?,
        overlaps,
    })
}

/// Edit a finished, unbilled entry. Changing a timer entry's times marks it `timer_edited`.
pub fn update_entry(
    conn: &Connection,
    id: &str,
    input: &EntryInput,
    now: DateTime<Utc>,
) -> AppResult<SaveEntryResult> {
    let existing = get_entry(conn, id)?;
    ensure_unlocked(&existing)?;
    if existing.ended_at.is_none() {
        return validation("Stop the timer before editing its times.");
    }
    if input.project_id != existing.project_id {
        ensure_active_project(conn, &input.project_id)?;
    }
    let r = validate_range(conn, &input.started_at, &input.ended_at, now)?;
    let times_changed =
        r.start != existing.started_at || Some(&r.end) != existing.ended_at.as_ref();
    let source = if existing.source == "timer" && times_changed {
        "timer_edited"
    } else {
        existing.source.as_str()
    };
    conn.execute(
        "UPDATE time_entries SET project_id = ?2, description = ?3, started_at = ?4, ended_at = ?5,
                duration_seconds = ?6, billable = ?7, source = ?8, updated_at = ?9
         WHERE id = ?1",
        params![
            id,
            input.project_id,
            input.description.trim(),
            r.start,
            r.end,
            r.seconds,
            input.billable,
            source,
            time::now_iso()
        ],
    )?;
    let overlaps = find_overlaps(conn, &r.start, &r.end, Some(id))?;
    Ok(SaveEntryResult {
        entry: get_entry(conn, id)?,
        overlaps,
    })
}

/// Toggle billable on an unbilled entry without touching its times.
pub fn set_billable(conn: &Connection, id: &str, billable: bool) -> AppResult<Entry> {
    let e = get_entry(conn, id)?;
    ensure_unlocked(&e)?;
    conn.execute(
        "UPDATE time_entries SET billable = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, billable, time::now_iso()],
    )?;
    get_entry(conn, id)
}

pub fn delete_entry(conn: &Connection, id: &str) -> AppResult<()> {
    let e = get_entry(conn, id)?;
    ensure_unlocked(&e)?;
    let now = time::now_iso();
    conn.execute(
        "UPDATE time_entries SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

/// Split a finished entry into two at `at`.
pub fn split_entry(conn: &Connection, id: &str, at: &str) -> AppResult<(Entry, Entry)> {
    let e = get_entry(conn, id)?;
    ensure_unlocked(&e)?;
    let Some(ended_at) = &e.ended_at else {
        return validation("Stop the timer before splitting the entry.");
    };
    let start = parse_iso(&e.started_at)?;
    let end = parse_iso(ended_at)?;
    let split = parse_iso(at)?;
    if split <= start || split >= end {
        return validation("The split time must be between the entry's start and end.");
    }
    let source = if e.source == "timer" {
        "timer_edited"
    } else {
        e.source.as_str()
    };
    let tx = conn.unchecked_transaction()?;
    let now = time::now_iso();
    tx.execute(
        "UPDATE time_entries SET ended_at = ?2, duration_seconds = ?3, source = ?4, updated_at = ?5 WHERE id = ?1",
        params![id, to_iso(split), seconds_between(start, split), source, now],
    )?;
    let new_id = time::new_id();
    tx.execute(
        "INSERT INTO time_entries (id, project_id, description, started_at, ended_at, duration_seconds, billable, source, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        params![new_id, e.project_id, e.description, to_iso(split), ended_at, seconds_between(split, end), e.billable, source, now],
    )?;
    tx.commit()?;
    Ok((get_entry(conn, id)?, get_entry(conn, &new_id)?))
}

/// Build the WHERE clause shared by the log, summaries and CSV export.
pub fn filter_sql(f: &EntryFilter) -> (String, Vec<Box<dyn ToSql>>) {
    let mut sql = String::new();
    let mut vals: Vec<Box<dyn ToSql>> = Vec::new();
    let mut push = |clause: &str, v: Box<dyn ToSql>, sql: &mut String| {
        vals.push(v);
        sql.push_str(&clause.replace("?", &format!("?{}", vals.len())));
    };
    if let Some(from) = &f.from {
        push(" AND e.started_at >= ?", Box::new(from.clone()), &mut sql);
    }
    if let Some(to) = &f.to {
        push(" AND e.started_at < ?", Box::new(to.clone()), &mut sql);
    }
    if let Some(cid) = &f.client_id {
        push(" AND c.id = ?", Box::new(cid.clone()), &mut sql);
    }
    if let Some(pid) = &f.project_id {
        push(" AND e.project_id = ?", Box::new(pid.clone()), &mut sql);
    }
    if let Some(b) = f.billable {
        push(" AND e.billable = ?", Box::new(b), &mut sql);
    }
    if let Some(s) = f.search.as_ref().filter(|s| !s.trim().is_empty()) {
        let like = format!(
            "%{}%",
            s.trim()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        push(
            " AND e.description LIKE ? ESCAPE '\\'",
            Box::new(like),
            &mut sql,
        );
    }
    match f.billed {
        Some(true) => sql.push_str(" AND e.invoice_id IS NOT NULL"),
        Some(false) => sql.push_str(" AND e.invoice_id IS NULL"),
        None => {}
    }
    if !f.include_running.unwrap_or(false) {
        sql.push_str(" AND e.ended_at IS NOT NULL");
    }
    (sql, vals)
}

pub fn list_entries(conn: &Connection, f: &EntryFilter) -> AppResult<Vec<Entry>> {
    let (where_sql, vals) = filter_sql(f);
    let limit = f.limit.unwrap_or(500).clamp(1, 1_000_000);
    let offset = f.offset.unwrap_or(0).max(0);
    let sql = format!(
        "{ENTRY_SELECT}{where_sql} ORDER BY e.started_at DESC LIMIT {limit} OFFSET {offset}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn ToSql> = vals.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(params.as_slice(), map_entry)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Recent distinct project + description combinations for quick start.
pub fn recent_combos(conn: &Connection, limit: i64) -> AppResult<Vec<RecentCombo>> {
    let mut stmt = conn.prepare(
        "SELECT e.project_id, p.name, p.color, c.id, c.name, e.description, MAX(e.started_at) AS last_used
         FROM time_entries e
         JOIN projects p ON p.id = e.project_id
         JOIN clients c ON c.id = p.client_id
         WHERE e.deleted_at IS NULL AND p.deleted_at IS NULL AND p.archived_at IS NULL
           AND c.deleted_at IS NULL AND c.archived_at IS NULL
           AND e.started_at >= ?1
         GROUP BY e.project_id, e.description
         ORDER BY last_used DESC
         LIMIT ?2",
    )?;
    // Looking back 90 days keeps this fast on large logs.
    let since = to_iso(Utc::now() - chrono::Duration::days(90));
    let rows = stmt
        .query_map(params![since, limit], |r| {
            Ok(RecentCombo {
                project_id: r.get(0)?,
                project_name: r.get(1)?,
                project_color: r.get(2)?,
                client_id: r.get(3)?,
                client_name: r.get(4)?,
                description: r.get(5)?,
                last_used: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::db;
    use crate::models::{ClientInput, ProjectInput};
    use crate::services::clients;

    pub fn setup() -> (Connection, String) {
        let conn = db::open_in_memory().unwrap();
        let c = clients::create_client(
            &conn,
            &ClientInput {
                name: "Acme".into(),
                contact_name: String::new(),
                email: String::new(),
                billing_address: String::new(),
                currency: "USD".into(),
                default_rate: 6_000,
                notes: String::new(),
            },
        )
        .unwrap();
        let p = clients::create_project(
            &conn,
            &ProjectInput {
                client_id: c.id,
                name: "Web".into(),
                rate_override: None,
                color: None,
            },
        )
        .unwrap();
        (conn, p.id)
    }

    pub fn ts(s: &str) -> DateTime<Utc> {
        parse_iso(s).unwrap()
    }

    pub fn manual(conn: &Connection, pid: &str, start: &str, end: &str) -> Entry {
        create_entry(
            conn,
            &EntryInput {
                project_id: pid.into(),
                description: "work".into(),
                started_at: start.into(),
                ended_at: end.into(),
                billable: true,
            },
            ts("2030-01-01T00:00:00Z"),
        )
        .unwrap()
        .entry
    }

    #[test]
    fn starting_a_timer_stops_the_running_one() {
        let (conn, pid) = setup();
        let input = StartTimerInput {
            project_id: pid.clone(),
            description: "a".into(),
            billable: true,
        };
        let first = start_timer(&conn, &input, ts("2026-01-01T09:00:00Z")).unwrap();
        let second = start_timer(&conn, &input, ts("2026-01-01T10:30:00Z")).unwrap();
        let first = get_entry(&conn, &first.id).unwrap();
        assert_eq!(first.ended_at.as_deref(), Some("2026-01-01T10:30:00.000Z"));
        assert_eq!(first.duration_seconds, 5400);
        assert_eq!(get_running(&conn).unwrap().unwrap().id, second.id);
        let stopped = stop_timer(&conn, ts("2026-01-01T10:45:00Z"))
            .unwrap()
            .unwrap();
        assert_eq!(stopped.duration_seconds, 900);
        assert!(get_running(&conn).unwrap().is_none());
    }

    #[test]
    fn editing_timer_times_marks_timer_edited() {
        let (conn, pid) = setup();
        let input = StartTimerInput {
            project_id: pid.clone(),
            description: "a".into(),
            billable: true,
        };
        let e = start_timer(&conn, &input, ts("2026-01-01T09:00:00Z")).unwrap();
        stop_timer(&conn, ts("2026-01-01T10:00:00Z")).unwrap();
        let mut ei = EntryInput {
            project_id: pid.clone(),
            description: "changed".into(),
            started_at: "2026-01-01T09:00:00.000Z".into(),
            ended_at: "2026-01-01T10:00:00.000Z".into(),
            billable: true,
        };
        let r = update_entry(&conn, &e.id, &ei, ts("2030-01-01T00:00:00Z")).unwrap();
        assert_eq!(
            r.entry.source, "timer",
            "description-only edit keeps source"
        );
        ei.ended_at = "2026-01-01T10:15:00Z".into();
        let r = update_entry(&conn, &e.id, &ei, ts("2030-01-01T00:00:00Z")).unwrap();
        assert_eq!(r.entry.source, "timer_edited");
        assert_eq!(r.entry.duration_seconds, 4500);
    }

    #[test]
    fn validates_ranges_and_reports_overlaps() {
        let (conn, pid) = setup();
        let now = ts("2030-01-01T00:00:00Z");
        let bad = EntryInput {
            project_id: pid.clone(),
            description: String::new(),
            started_at: "2026-01-01T10:00:00Z".into(),
            ended_at: "2026-01-01T09:00:00Z".into(),
            billable: true,
        };
        assert!(create_entry(&conn, &bad, now).is_err());
        manual(&conn, &pid, "2026-01-01T09:00:00Z", "2026-01-01T10:00:00Z");
        let r = create_entry(
            &conn,
            &EntryInput {
                started_at: "2026-01-01T09:30:00Z".into(),
                ended_at: "2026-01-01T11:00:00Z".into(),
                ..bad.clone()
            },
            now,
        )
        .unwrap();
        assert_eq!(r.overlaps.len(), 1, "overlap warns but saves");
        assert_eq!(r.entry.source, "manual");
        // Overlapping the running timer is blocked.
        start_timer(
            &conn,
            &StartTimerInput {
                project_id: pid.clone(),
                description: String::new(),
                billable: true,
            },
            ts("2026-01-02T09:00:00Z"),
        )
        .unwrap();
        let clash = EntryInput {
            started_at: "2026-01-02T08:00:00Z".into(),
            ended_at: "2026-01-02T09:30:00Z".into(),
            ..bad
        };
        assert!(create_entry(&conn, &clash, now).is_err());
    }

    #[test]
    fn split_and_filters() {
        let (conn, pid) = setup();
        let e = manual(&conn, &pid, "2026-01-01T09:00:00Z", "2026-01-01T11:00:00Z");
        let (a, b) = split_entry(&conn, &e.id, "2026-01-01T10:00:00Z").unwrap();
        assert_eq!(a.duration_seconds, 3600);
        assert_eq!(b.duration_seconds, 3600);
        assert_eq!(b.source, "manual");
        let all = list_entries(&conn, &EntryFilter::default()).unwrap();
        assert_eq!(all.len(), 2);
        let ranged = list_entries(
            &conn,
            &EntryFilter {
                from: Some("2026-01-01T09:30:00.000Z".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(ranged.len(), 1);
        let searched = list_entries(
            &conn,
            &EntryFilter {
                search: Some("wor".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(searched.len(), 2);
    }
}
