//! SQLite access: opening the database, pragmas, schema migrations and backups.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};

use crate::error::{AppError, AppResult};
use crate::time;

pub const DB_FILE: &str = "hourmint.sqlite";
const DAILY_PREFIX: &str = "hourmint-daily-";
const PRE_MIGRATION_PREFIX: &str = "hourmint-pre-migration-";
const DAILY_KEEP: usize = 7;
const PRE_MIGRATION_KEEP: usize = 5;

/// Ordered schema migrations. `PRAGMA user_version` records how many have run.
/// Never edit a shipped migration; append a new one instead.
const MIGRATIONS: &[&str] = &[
    // 1 — initial schema
    r#"
    CREATE TABLE settings (
        id TEXT PRIMARY KEY,
        singleton INTEGER NOT NULL DEFAULT 1 UNIQUE CHECK (singleton = 1),
        business_name TEXT NOT NULL DEFAULT '',
        address TEXT NOT NULL DEFAULT '',
        email TEXT NOT NULL DEFAULT '',
        phone TEXT NOT NULL DEFAULT '',
        tax_id TEXT NOT NULL DEFAULT '',
        logo_path TEXT,
        default_currency TEXT NOT NULL DEFAULT 'USD',
        default_rate INTEGER NOT NULL DEFAULT 0,
        payment_terms_days INTEGER NOT NULL DEFAULT 14,
        tax_label TEXT NOT NULL DEFAULT '',
        tax_rate REAL NOT NULL DEFAULT 0,
        rounding_minutes INTEGER NOT NULL DEFAULT 0,
        invoice_number_pattern TEXT NOT NULL DEFAULT 'INV-{YYYY}-{0001}',
        next_invoice_seq INTEGER NOT NULL DEFAULT 1,
        default_invoice_notes TEXT NOT NULL DEFAULT '',
        date_format TEXT NOT NULL DEFAULT 'YYYY-MM-DD',
        number_locale TEXT NOT NULL DEFAULT '',
        time_format TEXT NOT NULL DEFAULT '24h',
        week_start INTEGER NOT NULL DEFAULT 1,
        theme TEXT NOT NULL DEFAULT 'system',
        idle_threshold_minutes INTEGER NOT NULL DEFAULT 10,
        long_timer_hours REAL NOT NULL DEFAULT 8,
        global_shortcut TEXT NOT NULL DEFAULT 'CommandOrControl+Alt+T',
        launch_at_login INTEGER NOT NULL DEFAULT 0,
        reminder_enabled INTEGER NOT NULL DEFAULT 0,
        reminder_time TEXT NOT NULL DEFAULT '09:30',
        reminder_days TEXT NOT NULL DEFAULT '1,2,3,4,5',
        timesheet_show_source INTEGER NOT NULL DEFAULT 1,
        onboarded INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        deleted_at TEXT
    );

    CREATE TABLE clients (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        contact_name TEXT NOT NULL DEFAULT '',
        email TEXT NOT NULL DEFAULT '',
        billing_address TEXT NOT NULL DEFAULT '',
        currency TEXT NOT NULL,
        default_rate INTEGER NOT NULL DEFAULT 0,
        notes TEXT NOT NULL DEFAULT '',
        archived_at TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        deleted_at TEXT
    );

    CREATE TABLE projects (
        id TEXT PRIMARY KEY,
        client_id TEXT NOT NULL REFERENCES clients(id),
        name TEXT NOT NULL,
        rate_override INTEGER,
        color TEXT,
        archived_at TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        deleted_at TEXT
    );
    CREATE INDEX idx_projects_client ON projects(client_id);

    CREATE TABLE invoices (
        id TEXT PRIMARY KEY,
        client_id TEXT NOT NULL REFERENCES clients(id),
        number TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'sent', 'paid')),
        issue_date TEXT NOT NULL,
        due_date TEXT NOT NULL,
        period_start TEXT,
        period_end TEXT,
        currency TEXT NOT NULL,
        grouping TEXT NOT NULL DEFAULT 'entry' CHECK (grouping IN ('entry', 'day', 'project')),
        rounding_minutes INTEGER NOT NULL DEFAULT 0,
        subtotal INTEGER NOT NULL DEFAULT 0,
        discount_type TEXT NOT NULL DEFAULT 'none' CHECK (discount_type IN ('none', 'percent', 'fixed')),
        discount_value REAL NOT NULL DEFAULT 0,
        discount_amount INTEGER NOT NULL DEFAULT 0,
        tax_label TEXT NOT NULL DEFAULT '',
        tax_rate REAL NOT NULL DEFAULT 0,
        tax_amount INTEGER NOT NULL DEFAULT 0,
        total INTEGER NOT NULL DEFAULT 0,
        notes TEXT NOT NULL DEFAULT '',
        include_timesheet INTEGER NOT NULL DEFAULT 1,
        sender_snapshot TEXT NOT NULL DEFAULT '{}',
        client_snapshot TEXT NOT NULL DEFAULT '{}',
        sent_at TEXT,
        paid_at TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        deleted_at TEXT
    );
    CREATE UNIQUE INDEX idx_invoices_number ON invoices(number) WHERE deleted_at IS NULL;
    CREATE INDEX idx_invoices_client ON invoices(client_id);

    CREATE TABLE invoice_lines (
        id TEXT PRIMARY KEY,
        invoice_id TEXT NOT NULL REFERENCES invoices(id),
        position INTEGER NOT NULL,
        description TEXT NOT NULL,
        quantity_seconds INTEGER NOT NULL,
        rate INTEGER NOT NULL,
        amount INTEGER NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        deleted_at TEXT
    );
    CREATE INDEX idx_invoice_lines_invoice ON invoice_lines(invoice_id);

    CREATE TABLE time_entries (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id),
        description TEXT NOT NULL DEFAULT '',
        started_at TEXT NOT NULL,
        ended_at TEXT,
        duration_seconds INTEGER NOT NULL DEFAULT 0,
        billable INTEGER NOT NULL DEFAULT 1,
        source TEXT NOT NULL CHECK (source IN ('timer', 'manual', 'timer_edited')),
        invoice_id TEXT REFERENCES invoices(id),
        invoice_rate INTEGER,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        deleted_at TEXT,
        CHECK (ended_at IS NULL OR ended_at >= started_at)
    );
    CREATE INDEX idx_entries_started ON time_entries(started_at);
    CREATE INDEX idx_entries_project ON time_entries(project_id);
    CREATE INDEX idx_entries_invoice ON time_entries(invoice_id);
    -- At most one running timer.
    CREATE UNIQUE INDEX idx_entries_one_running ON time_entries(ended_at IS NULL)
        WHERE ended_at IS NULL AND deleted_at IS NULL;
    "#,
    // 2 — the global shortcut is opt-in (set it in Settings → Timer)
    r#"
    UPDATE settings SET global_shortcut = '' WHERE global_shortcut = 'CommandOrControl+Alt+T';
    "#,
];

pub fn schema_version() -> i64 {
    MIGRATIONS.len() as i64
}

fn apply_pragmas(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

pub fn user_version(conn: &Connection) -> AppResult<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Open (creating if needed) the database at `path`, back it up if a migration
/// is pending, run migrations and make sure the settings row exists.
pub fn open(path: &Path, backup_dir: &Path) -> AppResult<Connection> {
    let existed = path.exists();
    let conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    let version = user_version(&conn)?;
    if version > schema_version() {
        return Err(AppError::Other(format!(
            "This database was created by a newer version of Hourmint (schema {version}). Please update the app."
        )));
    }
    if existed && version > 0 && version < schema_version() {
        std::fs::create_dir_all(backup_dir)?;
        let name = format!(
            "{PRE_MIGRATION_PREFIX}v{version}-{}.sqlite",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        backup_to(&conn, &backup_dir.join(name))?;
        prune(backup_dir, PRE_MIGRATION_PREFIX, PRE_MIGRATION_KEEP)?;
    }
    migrate(&conn)?;
    Ok(conn)
}

#[cfg(test)]
pub fn open_in_memory() -> AppResult<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn migrate(conn: &Connection) -> AppResult<()> {
    let version = user_version(conn)?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    ensure_settings(conn)?;
    Ok(())
}

fn ensure_settings(conn: &Connection) -> AppResult<()> {
    let exists: Option<String> = conn
        .query_row("SELECT id FROM settings LIMIT 1", [], |r| r.get(0))
        .optional()?;
    if exists.is_none() {
        let now = time::now_iso();
        conn.execute(
            "INSERT INTO settings (id, global_shortcut, created_at, updated_at) VALUES (?1, '', ?2, ?2)",
            (time::new_id(), now),
        )?;
    }
    Ok(())
}

/// Write a consistent copy of the live database to `dest` (must not exist).
pub fn backup_to(conn: &Connection, dest: &Path) -> AppResult<()> {
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }
    let dest_str = dest
        .to_str()
        .ok_or_else(|| AppError::Other("Backup path is not valid UTF-8".into()))?;
    conn.execute("VACUUM INTO ?1", [dest_str])?;
    Ok(())
}

/// Create today's automatic backup if it does not exist yet, keeping the last 7.
pub fn daily_backup(conn: &Connection, backup_dir: &Path) -> AppResult<Option<PathBuf>> {
    std::fs::create_dir_all(backup_dir)?;
    let name = format!(
        "{DAILY_PREFIX}{}.sqlite",
        chrono::Local::now().format("%Y-%m-%d")
    );
    let dest = backup_dir.join(name);
    if dest.exists() {
        return Ok(None);
    }
    backup_to(conn, &dest)?;
    prune(backup_dir, DAILY_PREFIX, DAILY_KEEP)?;
    Ok(Some(dest))
}

fn prune(dir: &Path, prefix: &str, keep: usize) -> AppResult<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix) && n.ends_with(".sqlite"))
                .unwrap_or(false)
        })
        .collect();
    // Names embed a sortable date, so lexical order is chronological.
    files.sort();
    if files.len() > keep {
        for old in &files[..files.len() - keep] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFile {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub modified: Option<String>,
}

pub fn list_backups(dir: &Path) -> Vec<BackupFile> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut out: Vec<BackupFile> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "sqlite").unwrap_or(false))
        .map(|e| {
            let meta = e.metadata().ok();
            BackupFile {
                path: e.path().to_string_lossy().into_owned(),
                name: e.file_name().to_string_lossy().into_owned(),
                size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                modified: meta
                    .and_then(|m| m.modified().ok())
                    .map(|t| time::to_iso(chrono::DateTime::<chrono::Utc>::from(t))),
            }
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

/// Check that `path` is a Hourmint database this version can open.
pub fn validate_backup_file(path: &Path) -> AppResult<()> {
    let src = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| AppError::Validation("The file could not be opened as a database.".into()))?;
    let version = user_version(&src)
        .map_err(|_| AppError::Validation("The file is not a SQLite database.".into()))?;
    if version < 1 {
        return Err(AppError::Validation(
            "The file is not a Hourmint backup.".into(),
        ));
    }
    if version > schema_version() {
        return Err(AppError::Validation(
            "This backup was made by a newer version of Hourmint.".into(),
        ));
    }
    let has_tables: i64 = src.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('settings','clients','projects','time_entries','invoices','invoice_lines')",
        [],
        |r| r.get(0),
    )?;
    if has_tables != 6 {
        return Err(AppError::Validation(
            "The file is not a Hourmint backup.".into(),
        ));
    }
    Ok(())
}

/// Replace the live database's contents with the backup at `src_path`
/// (using SQLite's online backup API so WAL state stays consistent),
/// then bring the schema up to date.
pub fn restore_from(conn: &mut Connection, src_path: &Path) -> AppResult<()> {
    validate_backup_file(src_path)?;
    let src = Connection::open_with_flags(src_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    {
        let backup = rusqlite::backup::Backup::new(&src, conn)?;
        backup.run_to_completion(256, std::time::Duration::from_millis(0), None)?;
    }
    apply_pragmas(conn)?;
    migrate(conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_shortcut_is_off_by_default_and_cleared_on_upgrade() {
        let conn = open_in_memory().unwrap();
        let sc: String = conn
            .query_row("SELECT global_shortcut FROM settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sc, "");
        // A v1 database with the old default gets it cleared by migration 2.
        let old = Connection::open_in_memory().unwrap();
        old.execute_batch(MIGRATIONS[0]).unwrap();
        old.pragma_update(None, "user_version", 1).unwrap();
        old.execute(
            "INSERT INTO settings (id, created_at, updated_at) VALUES ('s', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        migrate(&old).unwrap();
        let sc: String = old
            .query_row("SELECT global_shortcut FROM settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sc, "");
    }

    #[test]
    fn migrates_and_enforces_single_running_timer() {
        let conn = open_in_memory().unwrap();
        assert_eq!(user_version(&conn).unwrap(), schema_version());
        let now = time::now_iso();
        conn.execute(
            "INSERT INTO clients (id, name, currency, created_at, updated_at) VALUES ('c','C','USD',?1,?1)",
            [&now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO projects (id, client_id, name, created_at, updated_at) VALUES ('p','c','P',?1,?1)",
            [&now],
        )
        .unwrap();
        let ins = "INSERT INTO time_entries (id, project_id, started_at, source, created_at, updated_at) VALUES (?1,'p',?2,'timer',?2,?2)";
        conn.execute(ins, ["e1", &now]).unwrap();
        assert!(
            conn.execute(ins, ["e2", &now]).is_err(),
            "second running timer must be rejected"
        );
    }

    #[test]
    fn backup_and_restore_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join(DB_FILE);
        let backups = dir.path().join("backups");
        let mut conn = open(&db_path, &backups).unwrap();
        conn.execute("UPDATE settings SET business_name = 'Before'", [])
            .unwrap();
        let copy = dir.path().join("copy.sqlite");
        backup_to(&conn, &copy).unwrap();
        conn.execute("UPDATE settings SET business_name = 'After'", [])
            .unwrap();
        restore_from(&mut conn, &copy).unwrap();
        let name: String = conn
            .query_row("SELECT business_name FROM settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Before");
        assert!(daily_backup(&conn, &backups).unwrap().is_some());
        assert!(
            daily_backup(&conn, &backups).unwrap().is_none(),
            "only one per day"
        );
    }

    #[test]
    fn rejects_non_hourmint_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("other.sqlite");
        let c = Connection::open(&p).unwrap();
        c.execute_batch("CREATE TABLE x (a); PRAGMA user_version = 1;")
            .unwrap();
        drop(c);
        assert!(validate_backup_file(&p).is_err());
        let junk = dir.path().join("junk.sqlite");
        std::fs::write(&junk, b"not a database at all").unwrap();
        assert!(validate_backup_file(&junk).is_err());
    }
}
