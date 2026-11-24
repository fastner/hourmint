//! Export everything as JSON (one file) or CSV (one file per table).

use std::path::Path;

use rusqlite::{types::ValueRef, Connection};
use serde_json::{Map, Value};

use crate::error::AppResult;

pub const TABLES: &[&str] = &[
    "settings",
    "clients",
    "projects",
    "time_entries",
    "invoices",
    "invoice_lines",
];

fn table_rows(conn: &Connection, table: &str) -> AppResult<(Vec<String>, Vec<Vec<Value>>)> {
    // Table names come from the fixed list above, never from user input.
    let mut stmt = conn.prepare(&format!(
        "SELECT * FROM {table} WHERE deleted_at IS NULL ORDER BY created_at"
    ))?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let n = cols.len();
    let rows = stmt
        .query_map([], |r| {
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                out.push(match r.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => Value::from(v),
                    ValueRef::Real(v) => Value::from(v),
                    ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
                    ValueRef::Blob(_) => Value::Null,
                });
            }
            Ok(out)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((cols, rows))
}

pub fn export_json(conn: &Connection) -> AppResult<Vec<u8>> {
    let mut root = Map::new();
    root.insert("app".into(), Value::from("Hourmint"));
    root.insert(
        "schemaVersion".into(),
        Value::from(crate::db::schema_version()),
    );
    root.insert("exportedAt".into(), Value::from(crate::time::now_iso()));
    for t in TABLES {
        let (cols, rows) = table_rows(conn, t)?;
        let arr: Vec<Value> = rows
            .into_iter()
            .map(|r| Value::Object(cols.iter().cloned().zip(r).collect()))
            .collect();
        root.insert((*t).into(), Value::Array(arr));
    }
    Ok(serde_json::to_vec_pretty(&Value::Object(root))?)
}

pub fn export_csv_dir(conn: &Connection, dir: &Path) -> AppResult<Vec<String>> {
    std::fs::create_dir_all(dir)?;
    let mut written = Vec::new();
    for t in TABLES {
        let (cols, rows) = table_rows(conn, t)?;
        let path = dir.join(format!("{t}.csv"));
        let mut w = csv::Writer::from_path(&path)?;
        w.write_record(&cols)?;
        for r in rows {
            w.write_record(r.iter().map(|v| match v {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            }))?;
        }
        w.flush()?;
        written.push(path.to_string_lossy().into_owned());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::entries::tests::{manual, setup};

    #[test]
    fn exports_all_tables() {
        let (conn, pid) = setup();
        manual(&conn, &pid, "2026-01-01T09:00:00Z", "2026-01-01T10:00:00Z");
        let json: Value = serde_json::from_slice(&export_json(&conn).unwrap()).unwrap();
        assert_eq!(json["time_entries"].as_array().unwrap().len(), 1);
        assert_eq!(json["clients"][0]["name"], "Acme");
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            export_csv_dir(&conn, dir.path()).unwrap().len(),
            TABLES.len()
        );
    }
}
