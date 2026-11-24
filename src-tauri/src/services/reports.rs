//! Summaries and CSV export of entries.

use std::collections::BTreeMap;

use rusqlite::{types::ToSql, Connection};

use crate::error::AppResult;
use crate::models::{CurrencyTotal, EntryFilter, Summary, SummaryRow};
use crate::services::entries::{filter_sql, list_entries};

const RATE: &str = "COALESCE(e.invoice_rate, p.rate_override, c.default_rate)";

fn grouped(conn: &Connection, f: &EntryFilter, by_project: bool) -> AppResult<Vec<SummaryRow>> {
    let (where_sql, vals) = filter_sql(f);
    let (id, name, color, group) = if by_project {
        ("p.id", "p.name", "p.color", "p.id")
    } else {
        ("c.id", "c.name", "NULL", "c.id")
    };
    let sql = format!(
        "SELECT {id}, {name}, c.id, c.name, {color}, c.currency,
                SUM(e.duration_seconds),
                SUM(CASE WHEN e.billable = 1 THEN e.duration_seconds ELSE 0 END),
                CAST(ROUND(SUM(CASE WHEN e.billable = 1 THEN e.duration_seconds * {RATE} ELSE 0 END) / 3600.0) AS INTEGER),
                COUNT(*)
         FROM time_entries e
         JOIN projects p ON p.id = e.project_id
         JOIN clients c ON c.id = p.client_id
         WHERE e.deleted_at IS NULL{where_sql}
         GROUP BY {group}
         ORDER BY SUM(e.duration_seconds) DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn ToSql> = vals.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(params.as_slice(), |r| {
            Ok(SummaryRow {
                id: r.get(0)?,
                name: r.get(1)?,
                client_id: r.get(2)?,
                client_name: r.get(3)?,
                color: r.get(4)?,
                currency: r.get(5)?,
                seconds: r.get(6)?,
                billable_seconds: r.get(7)?,
                amount: r.get(8)?,
                entry_count: r.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn summary(conn: &Connection, f: &EntryFilter) -> AppResult<Summary> {
    let by_client = grouped(conn, f, false)?;
    let by_project = grouped(conn, f, true)?;
    let mut amounts: BTreeMap<String, i64> = BTreeMap::new();
    for r in &by_client {
        *amounts.entry(r.currency.clone()).or_default() += r.amount;
    }
    Ok(Summary {
        total_seconds: by_client.iter().map(|r| r.seconds).sum(),
        billable_seconds: by_client.iter().map(|r| r.billable_seconds).sum(),
        entry_count: by_client.iter().map(|r| r.entry_count).sum(),
        amounts: amounts
            .into_iter()
            .map(|(currency, amount)| CurrencyTotal { currency, amount })
            .collect(),
        by_client,
        by_project,
    })
}

fn hours(seconds: i64) -> String {
    format!("{:.2}", seconds as f64 / 3600.0)
}

/// Amount in minor units → decimal string using the currency's minor-unit exponent.
pub fn decimal(amount: i64, currency: &str) -> String {
    let exp = currency_exponent(currency);
    if exp == 0 {
        return amount.to_string();
    }
    let div = 10_i64.pow(exp);
    let sign = if amount < 0 { "-" } else { "" };
    let a = amount.abs();
    format!(
        "{sign}{}.{:0width$}",
        a / div,
        a % div,
        width = exp as usize
    )
}

/// ISO 4217 minor-unit exponent for the few currencies that differ from 2.
pub fn currency_exponent(code: &str) -> u32 {
    match code {
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX"
        | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        _ => 2,
    }
}

pub fn entries_csv(conn: &Connection, f: &EntryFilter) -> AppResult<Vec<u8>> {
    let mut all = EntryFilter {
        limit: Some(1_000_000),
        offset: Some(0),
        ..f.clone()
    };
    all.include_running = Some(false);
    let entries = list_entries(conn, &all)?;
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record([
        "date",
        "start_utc",
        "end_utc",
        "start_local",
        "end_local",
        "duration_seconds",
        "hours",
        "client",
        "project",
        "description",
        "billable",
        "source",
        "rate",
        "amount",
        "currency",
        "invoice",
    ])?;
    for e in entries.iter().rev() {
        let start = crate::time::parse_iso(&e.started_at)?;
        let end = e
            .ended_at
            .as_deref()
            .map(crate::time::parse_iso)
            .transpose()?;
        let local_fmt = "%Y-%m-%d %H:%M:%S";
        let amount = if e.billable {
            (e.duration_seconds as f64 * e.rate as f64 / 3600.0).round() as i64
        } else {
            0
        };
        w.write_record([
            crate::time::local_date(start)
                .format("%Y-%m-%d")
                .to_string(),
            e.started_at.clone(),
            e.ended_at.clone().unwrap_or_default(),
            start
                .with_timezone(&chrono::Local)
                .format(local_fmt)
                .to_string(),
            end.map(|d| {
                d.with_timezone(&chrono::Local)
                    .format(local_fmt)
                    .to_string()
            })
            .unwrap_or_default(),
            e.duration_seconds.to_string(),
            hours(e.duration_seconds),
            e.client_name.clone(),
            e.project_name.clone(),
            e.description.clone(),
            if e.billable {
                "yes".into()
            } else {
                "no".into()
            },
            e.source.clone(),
            decimal(e.rate, &e.currency),
            decimal(amount, &e.currency),
            e.currency.clone(),
            e.invoice_number.clone().unwrap_or_default(),
        ])?;
    }
    w.into_inner()
        .map_err(|e| crate::error::AppError::Other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::entries::tests::{manual, setup};

    #[test]
    fn summary_amounts_use_rates() {
        let (conn, pid) = setup();
        manual(&conn, &pid, "2026-01-01T09:00:00Z", "2026-01-01T10:30:00Z");
        let s = summary(&conn, &EntryFilter::default()).unwrap();
        assert_eq!(s.total_seconds, 5400);
        // 1.5 h × 60.00
        assert_eq!(s.amounts[0].amount, 9_000);
        assert_eq!(s.by_project.len(), 1);
        let csv = String::from_utf8(entries_csv(&conn, &EntryFilter::default()).unwrap()).unwrap();
        assert!(csv.lines().count() == 2 && csv.contains("90.00"));
    }

    #[test]
    fn decimals_follow_currency() {
        assert_eq!(decimal(12345, "USD"), "123.45");
        assert_eq!(decimal(12345, "JPY"), "12345");
        assert_eq!(decimal(12345, "KWD"), "12.345");
        assert_eq!(decimal(-5, "EUR"), "-0.05");
    }
}

#[cfg(test)]
mod perf {
    use super::*;
    use crate::services::entries::tests::setup;
    use rusqlite::params;

    /// `cargo test --release perf -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn hundred_thousand_entries() {
        let (conn, pid) = setup();
        let tx = conn.unchecked_transaction().unwrap();
        let base = chrono::Utc::now() - chrono::Duration::minutes(100_000 * 20 + 60);
        for i in 0..100_000i64 {
            let s = base + chrono::Duration::minutes(i * 20);
            let e = s + chrono::Duration::minutes(15);
            tx.execute(
                "INSERT INTO time_entries (id, project_id, description, started_at, ended_at, duration_seconds, billable, source, created_at, updated_at)
                 VALUES (?1, ?2, 'x', ?3, ?4, 900, 1, 'timer', ?3, ?3)",
                params![format!("e{i}"), pid, crate::time::to_iso(s), crate::time::to_iso(e)],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let week = EntryFilter {
            from: Some(crate::time::to_iso(
                chrono::Utc::now() - chrono::Duration::days(7),
            )),
            to: Some(crate::time::now_iso()),
            ..Default::default()
        };
        let t = std::time::Instant::now();
        let n = crate::services::entries::list_entries(&conn, &week)
            .unwrap()
            .len();
        println!("week list: {n} rows in {:?}", t.elapsed());
        let t = std::time::Instant::now();
        summary(&conn, &week).unwrap();
        println!("week summary: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        summary(&conn, &EntryFilter::default()).unwrap();
        println!("all-time summary: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        crate::services::clients::list_clients(&conn, true).unwrap();
        println!("clients with unbilled totals: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        crate::services::entries::recent_combos(&conn, 8).unwrap();
        println!("recent combos: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        crate::services::entries::get_running(&conn).unwrap();
        println!("get running: {:?}", t.elapsed());
    }
}
