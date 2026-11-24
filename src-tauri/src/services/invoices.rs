//! Invoices: line generation (grouping + rounding), totals, numbering, locking
//! and statuses.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use rusqlite::{params, types::ToSql, Connection, OptionalExtension, Row};

use crate::error::{validation, AppError, AppResult};
use crate::models::{
    Entry, Invoice, InvoiceComputation, InvoiceDetail, InvoiceFilter, InvoiceInput, InvoiceLine,
    InvoiceTotals,
};
use crate::services::{clients, entries, settings, settings::ROUNDING_CHOICES};
use crate::time::{self, format_date, local_date, parse_date, parse_iso};

// ---------------------------------------------------------------- numbering

/// True when the pattern contains a sequence token such as `{0001}`.
pub fn pattern_has_sequence(pattern: &str) -> bool {
    let mut rest = pattern;
    while let Some(i) = rest.find('{') {
        let after = &rest[i + 1..];
        if let Some(j) = after.find('}') {
            let tok = &after[..j];
            if !tok.is_empty() && tok.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
            rest = &after[j..];
        } else {
            break;
        }
    }
    false
}

/// Expand `{YYYY}`, `{YY}`, `{MM}`, `{DD}` and a zero-padded sequence token
/// whose width is the token's length (`{0001}` → `0042`).
pub fn format_number(pattern: &str, seq: i64, date: NaiveDate) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(j) = after.find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let tok = &after[..j];
        match tok {
            "YYYY" => out.push_str(&format!("{:04}", date.year())),
            "YY" => out.push_str(&format!("{:02}", date.year() % 100)),
            "MM" => out.push_str(&format!("{:02}", date.month())),
            "DD" => out.push_str(&format!("{:02}", date.day())),
            t if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) => {
                out.push_str(&format!("{:0width$}", seq, width = t.len()))
            }
            other => {
                out.push('{');
                out.push_str(other);
                out.push('}');
            }
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    out
}

fn number_taken(conn: &Connection, number: &str, exclude_id: Option<&str>) -> AppResult<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM invoices WHERE number = ?1 AND deleted_at IS NULL AND id != ?2",
        params![number, exclude_id.unwrap_or("")],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Next free number from the pattern, plus the sequence value it used.
pub fn next_number(conn: &Connection, date: NaiveDate) -> AppResult<(String, i64)> {
    let s = settings::get(conn)?;
    let mut seq = s.next_invoice_seq.max(1);
    loop {
        let n = format_number(&s.invoice_number_pattern, seq, date);
        if !number_taken(conn, &n, None)? {
            return Ok((n, seq));
        }
        seq += 1;
    }
}

// ---------------------------------------------------------------- computing

/// Round seconds for billing. 0 = exact minutes (nearest minute); otherwise up to the next N minutes.
pub fn round_seconds(seconds: i64, rounding_minutes: i64) -> i64 {
    if rounding_minutes <= 0 {
        ((seconds + 30) / 60) * 60
    } else {
        let step = rounding_minutes * 60;
        ((seconds + step - 1) / step) * step
    }
}

pub fn line_amount(quantity_seconds: i64, rate: i64) -> i64 {
    // Integer half-up rounding of seconds × rate / 3600.
    let num = quantity_seconds as i128 * rate as i128;
    ((num + 1800) / 3600) as i64
}

pub struct ComputeOptions<'a> {
    pub grouping: &'a str,
    pub rounding_minutes: i64,
    pub discount_type: &'a str,
    pub discount_value: f64,
    pub tax_rate: f64,
    pub date_format: &'a str,
}

fn join_unique(items: impl Iterator<Item = String>) -> String {
    let mut seen: Vec<String> = Vec::new();
    for i in items {
        let t = i.trim().to_string();
        if !t.is_empty() && !seen.contains(&t) {
            seen.push(t);
        }
    }
    seen.join("; ")
}

/// Derive invoice lines and totals from entries. Each entry's `rate` is the
/// rate to bill (already snapshotted for invoiced entries).
pub fn compute(entries: &[Entry], o: &ComputeOptions) -> AppResult<InvoiceComputation> {
    let mut sorted: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.billable && e.ended_at.is_some())
        .collect();
    sorted.sort_by(|a, b| a.started_at.cmp(&b.started_at));

    // Group key → (sort key, description parts, seconds, rate)
    struct Group<'e> {
        order: String,
        label: String,
        items: Vec<&'e Entry>,
        rate: i64,
    }
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for e in &sorted {
        let date = local_date(parse_iso(&e.started_at)?);
        let (key, order, label) = match o.grouping {
            "entry" => (
                e.id.clone(),
                format!("{}|{}", e.started_at, e.id),
                format_date(date, o.date_format),
            ),
            "day" => (
                format!("{}|{}", date, e.rate),
                format!("{}|{:020}", date, e.rate),
                format_date(date, o.date_format),
            ),
            "project" => (
                format!("{}|{}", e.project_id, e.rate),
                // Order projects by first appearance.
                String::new(),
                e.project_name.clone(),
            ),
            _ => return validation("Unknown grouping."),
        };
        let g = groups.entry(key).or_insert_with(|| Group {
            order: if order.is_empty() {
                format!("{}|{}", e.started_at, e.project_id)
            } else {
                order
            },
            label,
            items: Vec::new(),
            rate: e.rate,
        });
        g.items.push(e);
    }
    let mut groups: Vec<Group> = groups.into_values().collect();
    groups.sort_by(|a, b| a.order.cmp(&b.order));

    let mut lines = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        let raw: i64 = g.items.iter().map(|e| e.duration_seconds).sum();
        let qty = round_seconds(raw, o.rounding_minutes);
        let description = match o.grouping {
            "entry" => {
                let e = g.items[0];
                let what = if e.description.trim().is_empty() {
                    e.project_name.clone()
                } else {
                    format!("{} — {}", e.project_name, e.description.trim())
                };
                format!("{} · {}", g.label, what)
            }
            "day" => {
                let projects = join_unique(g.items.iter().map(|e| e.project_name.clone()));
                let descs = join_unique(g.items.iter().map(|e| e.description.clone()));
                if descs.is_empty() {
                    format!("{} · {}", g.label, projects)
                } else {
                    format!("{} · {}: {}", g.label, projects, descs)
                }
            }
            _ => {
                let first = local_date(parse_iso(&g.items[0].started_at)?);
                let last = local_date(parse_iso(&g.items[g.items.len() - 1].started_at)?);
                if first == last {
                    format!("{} ({})", g.label, format_date(first, o.date_format))
                } else {
                    format!(
                        "{} ({} – {})",
                        g.label,
                        format_date(first, o.date_format),
                        format_date(last, o.date_format)
                    )
                }
            }
        };
        lines.push(InvoiceLine {
            id: String::new(),
            position: i as i64 + 1,
            description,
            quantity_seconds: qty,
            rate: g.rate,
            amount: line_amount(qty, g.rate),
        });
    }

    let subtotal: i64 = lines.iter().map(|l| l.amount).sum();
    let discount_amount = match o.discount_type {
        "percent" => {
            ((subtotal as f64) * o.discount_value.clamp(0.0, 100.0) / 100.0).round() as i64
        }
        "fixed" => (o.discount_value.max(0.0).round() as i64).min(subtotal),
        _ => 0,
    };
    let taxable = subtotal - discount_amount;
    let tax_amount = ((taxable as f64) * o.tax_rate.max(0.0) / 100.0).round() as i64;
    Ok(InvoiceComputation {
        total_seconds: lines.iter().map(|l| l.quantity_seconds).sum(),
        lines,
        subtotal,
        discount_amount,
        tax_amount,
        total: taxable + tax_amount,
    })
}

// ---------------------------------------------------------------- reading

const INVOICE_SELECT: &str = "
    SELECT i.id, i.client_id, c.name, i.number, i.status, i.issue_date, i.due_date, i.period_start,
           i.period_end, i.currency, i.grouping, i.rounding_minutes, i.subtotal, i.discount_type,
           i.discount_value, i.discount_amount, i.tax_label, i.tax_rate, i.tax_amount, i.total,
           i.notes, i.include_timesheet, i.sender_snapshot, i.client_snapshot, i.sent_at, i.paid_at,
           i.created_at, i.updated_at,
           (SELECT COALESCE(SUM(l.quantity_seconds), 0) FROM invoice_lines l WHERE l.invoice_id = i.id AND l.deleted_at IS NULL)
    FROM invoices i JOIN clients c ON c.id = i.client_id
    WHERE i.deleted_at IS NULL";

fn map_invoice(r: &Row) -> rusqlite::Result<Invoice> {
    let sender: String = r.get(22)?;
    let client: String = r.get(23)?;
    Ok(Invoice {
        id: r.get(0)?,
        client_id: r.get(1)?,
        client_name: r.get(2)?,
        number: r.get(3)?,
        status: r.get(4)?,
        issue_date: r.get(5)?,
        due_date: r.get(6)?,
        period_start: r.get(7)?,
        period_end: r.get(8)?,
        currency: r.get(9)?,
        grouping: r.get(10)?,
        rounding_minutes: r.get(11)?,
        subtotal: r.get(12)?,
        discount_type: r.get(13)?,
        discount_value: r.get(14)?,
        discount_amount: r.get(15)?,
        tax_label: r.get(16)?,
        tax_rate: r.get(17)?,
        tax_amount: r.get(18)?,
        total: r.get(19)?,
        notes: r.get(20)?,
        include_timesheet: r.get(21)?,
        sender_snapshot: serde_json::from_str(&sender).unwrap_or(serde_json::Value::Null),
        client_snapshot: serde_json::from_str(&client).unwrap_or(serde_json::Value::Null),
        sent_at: r.get(24)?,
        paid_at: r.get(25)?,
        created_at: r.get(26)?,
        updated_at: r.get(27)?,
        total_seconds: r.get(28)?,
    })
}

pub fn get_invoice(conn: &Connection, id: &str) -> AppResult<Invoice> {
    let sql = format!("{INVOICE_SELECT} AND i.id = ?1");
    conn.query_row(&sql, [id], map_invoice)
        .optional()?
        .ok_or_else(|| AppError::NotFound("invoice".into()))
}

fn invoice_entries(conn: &Connection, id: &str) -> AppResult<Vec<Entry>> {
    let sql = format!(
        "{} AND e.invoice_id = ?1 ORDER BY e.started_at",
        entries::ENTRY_SELECT
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([id], entries::map_entry)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_detail(conn: &Connection, id: &str) -> AppResult<InvoiceDetail> {
    let invoice = get_invoice(conn, id)?;
    let mut stmt = conn.prepare(
        "SELECT id, position, description, quantity_seconds, rate, amount FROM invoice_lines
         WHERE invoice_id = ?1 AND deleted_at IS NULL ORDER BY position",
    )?;
    let lines = stmt
        .query_map([id], |r| {
            Ok(InvoiceLine {
                id: r.get(0)?,
                position: r.get(1)?,
                description: r.get(2)?,
                quantity_seconds: r.get(3)?,
                rate: r.get(4)?,
                amount: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(InvoiceDetail {
        invoice,
        lines,
        entries: invoice_entries(conn, id)?,
    })
}

pub fn list(conn: &Connection, f: &InvoiceFilter) -> AppResult<Vec<Invoice>> {
    let mut sql = INVOICE_SELECT.to_string();
    let mut vals: Vec<Box<dyn ToSql>> = Vec::new();
    if let Some(c) = &f.client_id {
        vals.push(Box::new(c.clone()));
        sql.push_str(&format!(" AND i.client_id = ?{}", vals.len()));
    }
    if let Some(s) = &f.status {
        if s == "overdue" {
            vals.push(Box::new(chrono::Local::now().date_naive().to_string()));
            sql.push_str(&format!(
                " AND i.status = 'sent' AND i.due_date < ?{}",
                vals.len()
            ));
        } else {
            vals.push(Box::new(s.clone()));
            sql.push_str(&format!(" AND i.status = ?{}", vals.len()));
        }
    }
    sql.push_str(" ORDER BY i.issue_date DESC, i.created_at DESC");
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn ToSql> = vals.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(params.as_slice(), map_invoice)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Outstanding (sent), paid, draft and overdue totals per currency.
pub fn totals(conn: &Connection, client_id: Option<&str>) -> AppResult<Vec<InvoiceTotals>> {
    let today = chrono::Local::now().date_naive().to_string();
    let mut stmt = conn.prepare(
        "SELECT currency,
                SUM(CASE WHEN status = 'sent' THEN total ELSE 0 END),
                SUM(CASE WHEN status = 'paid' THEN total ELSE 0 END),
                SUM(CASE WHEN status = 'draft' THEN total ELSE 0 END),
                SUM(CASE WHEN status = 'sent' AND due_date < ?1 THEN total ELSE 0 END)
         FROM invoices WHERE deleted_at IS NULL AND (?2 IS NULL OR client_id = ?2)
         GROUP BY currency ORDER BY currency",
    )?;
    let rows = stmt
        .query_map(params![today, client_id], |r| {
            Ok(InvoiceTotals {
                currency: r.get(0)?,
                outstanding: r.get(1)?,
                paid: r.get(2)?,
                draft: r.get(3)?,
                overdue: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Unbilled, billable, finished entries for a client within [from, to) plus
/// anything already on `invoice_id` (when editing a draft).
pub fn candidate_entries(
    conn: &Connection,
    client_id: &str,
    from: Option<&str>,
    to: Option<&str>,
    invoice_id: Option<&str>,
) -> AppResult<Vec<Entry>> {
    let sql = format!(
        "{} AND c.id = ?1 AND e.billable = 1 AND e.ended_at IS NOT NULL
           AND ((e.invoice_id IS NULL AND (?2 IS NULL OR e.started_at >= ?2) AND (?3 IS NULL OR e.started_at < ?3))
                OR (?4 IS NOT NULL AND e.invoice_id = ?4))
         ORDER BY e.started_at",
        entries::ENTRY_SELECT
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![client_id, from, to, invoice_id], entries::map_entry)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------- writing

fn validate_input(i: &InvoiceInput) -> AppResult<(NaiveDate, NaiveDate)> {
    let issue = parse_date(&i.issue_date)?;
    let due = parse_date(&i.due_date)?;
    if due < issue {
        return validation("The due date cannot be before the issue date.");
    }
    if !["entry", "day", "project"].contains(&i.grouping.as_str()) {
        return validation("Unknown grouping.");
    }
    if !ROUNDING_CHOICES.contains(&i.rounding_minutes) {
        return validation("Rounding must be none, 5, 6, 15 or 30 minutes.");
    }
    if !["none", "percent", "fixed"].contains(&i.discount_type.as_str()) {
        return validation("Unknown discount type.");
    }
    if i.discount_value < 0.0 || (i.discount_type == "percent" && i.discount_value > 100.0) {
        return validation("Discount must be between 0 and 100%.");
    }
    if !(0.0..=100.0).contains(&i.tax_rate) {
        return validation("Tax rate must be between 0 and 100%.");
    }
    if i.entry_ids.is_empty() {
        return validation("Select at least one time entry.");
    }
    Ok((issue, due))
}

fn sender_snapshot(conn: &Connection) -> AppResult<serde_json::Value> {
    let s = settings::get(conn)?;
    Ok(serde_json::json!({
        "businessName": s.business_name,
        "address": s.address,
        "email": s.email,
        "phone": s.phone,
        "taxId": s.tax_id,
        "logoPath": s.logo_path,
    }))
}

fn client_snapshot(conn: &Connection, client_id: &str) -> AppResult<serde_json::Value> {
    let c = clients::get_client(conn, client_id)?;
    Ok(serde_json::json!({
        "name": c.name,
        "contactName": c.contact_name,
        "email": c.email,
        "billingAddress": c.billing_address,
    }))
}

/// Preview lines/totals for builder input without saving (live preview).
pub fn preview(conn: &Connection, input: &InvoiceInput) -> AppResult<InvoiceComputation> {
    let s = settings::get(conn)?;
    let candidates = candidate_entries(conn, &input.client_id, None, None, input.id.as_deref())?;
    let selected: Vec<Entry> = candidates
        .into_iter()
        .filter(|e| input.entry_ids.contains(&e.id))
        .collect();
    compute(
        &selected,
        &ComputeOptions {
            grouping: &input.grouping,
            rounding_minutes: input.rounding_minutes,
            discount_type: &input.discount_type,
            discount_value: input.discount_value,
            tax_rate: input.tax_rate,
            date_format: &s.date_format,
        },
    )
}

/// Create or update a draft invoice: link the selected entries (locking them),
/// snapshot rates and party details, and regenerate lines.
pub fn save_draft(conn: &Connection, input: &InvoiceInput) -> AppResult<InvoiceDetail> {
    let (issue, _) = validate_input(input)?;
    let client = clients::get_client(conn, &input.client_id)?;
    let s = settings::get(conn)?;
    let tx = conn.unchecked_transaction()?;

    let existing = match &input.id {
        Some(id) => {
            let inv = get_invoice(&tx, id)?;
            if inv.status != "draft" {
                return validation(
                    "Only draft invoices can be edited. Move it back to draft first.",
                );
            }
            if inv.client_id != input.client_id {
                return validation("The client of an existing invoice cannot change.");
            }
            Some(inv)
        }
        None => None,
    };

    // Number: explicit override or generated from the pattern.
    let requested = input
        .number
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let (number, used_seq) = match (requested, &existing) {
        (Some(n), _) => (n.to_string(), None),
        (None, Some(inv)) => (inv.number.clone(), None),
        (None, None) => {
            let (n, seq) = next_number(&tx, issue)?;
            (n, Some(seq))
        }
    };
    if number_taken(&tx, &number, input.id.as_deref())? {
        return Err(AppError::Conflict(format!(
            "Invoice number {number} is already used."
        )));
    }
    // If the override happens to match the next generated number, advance the sequence too.
    let used_seq = used_seq.or_else(|| {
        let (gen, seq) = next_number(&tx, issue).ok()?;
        (gen == number).then_some(seq)
    });

    let now = time::now_iso();
    let id = existing
        .as_ref()
        .map(|i| i.id.clone())
        .unwrap_or_else(time::new_id);
    let sender = sender_snapshot(&tx)?.to_string();
    let client_snap = client_snapshot(&tx, &input.client_id)?.to_string();

    if existing.is_none() {
        tx.execute(
            "INSERT INTO invoices (id, client_id, number, status, issue_date, due_date, currency, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'draft', ?4, ?5, ?6, ?7, ?7)",
            params![id, input.client_id, number, input.issue_date, input.due_date, client.currency, now],
        )?;
    }

    // Validate the selection: each entry must belong to the client, be billable,
    // finished, and be unbilled or already on this invoice.
    let candidates = candidate_entries(&tx, &input.client_id, None, None, Some(&id))?;
    let mut selected: Vec<Entry> = Vec::new();
    for eid in &input.entry_ids {
        match candidates.iter().find(|e| &e.id == eid) {
            Some(e) => selected.push(e.clone()),
            None => return validation("Some selected entries are no longer available (already invoiced, edited or deleted). Refresh and try again."),
        }
    }

    // Release entries removed from this draft; link and snapshot rates for new ones.
    for e in invoice_entries(&tx, &id)? {
        if !selected.iter().any(|s| s.id == e.id) {
            tx.execute(
                "UPDATE time_entries SET invoice_id = NULL, invoice_rate = NULL, updated_at = ?2 WHERE id = ?1",
                params![e.id, now],
            )?;
        }
    }
    for e in &selected {
        tx.execute(
            "UPDATE time_entries SET invoice_id = ?2, invoice_rate = COALESCE(invoice_rate, ?3), updated_at = ?4 WHERE id = ?1",
            params![e.id, id, e.rate, now],
        )?;
    }

    let linked = invoice_entries(&tx, &id)?;
    let comp = compute(
        &linked,
        &ComputeOptions {
            grouping: &input.grouping,
            rounding_minutes: input.rounding_minutes,
            discount_type: &input.discount_type,
            discount_value: input.discount_value,
            tax_rate: input.tax_rate,
            date_format: &s.date_format,
        },
    )?;

    // Lines are derived data for drafts, so they are replaced rather than soft-deleted.
    tx.execute("DELETE FROM invoice_lines WHERE invoice_id = ?1", [&id])?;
    for l in &comp.lines {
        tx.execute(
            "INSERT INTO invoice_lines (id, invoice_id, position, description, quantity_seconds, rate, amount, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![time::new_id(), id, l.position, l.description, l.quantity_seconds, l.rate, l.amount, now],
        )?;
    }

    tx.execute(
        "UPDATE invoices SET number = ?2, issue_date = ?3, due_date = ?4, period_start = ?5, period_end = ?6,
                grouping = ?7, rounding_minutes = ?8, subtotal = ?9, discount_type = ?10, discount_value = ?11,
                discount_amount = ?12, tax_label = ?13, tax_rate = ?14, tax_amount = ?15, total = ?16,
                notes = ?17, include_timesheet = ?18, sender_snapshot = ?19, client_snapshot = ?20, updated_at = ?21
         WHERE id = ?1",
        params![
            id,
            number,
            input.issue_date,
            input.due_date,
            input.period_start,
            input.period_end,
            input.grouping,
            input.rounding_minutes,
            comp.subtotal,
            input.discount_type,
            if input.discount_type == "none" { 0.0 } else { input.discount_value },
            comp.discount_amount,
            input.tax_label.trim(),
            input.tax_rate,
            comp.tax_amount,
            comp.total,
            input.notes,
            input.include_timesheet,
            sender,
            client_snap,
            now
        ],
    )?;
    if let Some(seq) = used_seq {
        tx.execute(
            "UPDATE settings SET next_invoice_seq = MAX(next_invoice_seq, ?1), updated_at = ?2",
            params![seq + 1, now],
        )?;
    }
    tx.commit()?;
    get_detail(conn, &id)
}

/// Change status by hand. Leaving draft finalises (entries stay locked as billed).
pub fn set_status(conn: &Connection, id: &str, status: &str) -> AppResult<Invoice> {
    let inv = get_invoice(conn, id)?;
    let now = time::now_iso();
    match status {
        "draft" => {
            conn.execute(
                "UPDATE invoices SET status = 'draft', sent_at = NULL, paid_at = NULL, updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
        }
        "sent" => {
            if inv.total_seconds == 0 && inv.subtotal == 0 {
                return validation("This invoice has no lines.");
            }
            conn.execute(
                "UPDATE invoices SET status = 'sent', sent_at = COALESCE(sent_at, ?2), paid_at = NULL, updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
        }
        "paid" => {
            conn.execute(
                "UPDATE invoices SET status = 'paid', sent_at = COALESCE(sent_at, ?2), paid_at = ?2, updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
        }
        _ => return validation("Unknown status."),
    }
    get_invoice(conn, id)
}

/// Delete an invoice (soft) and release its entries.
pub fn delete(conn: &Connection, id: &str) -> AppResult<()> {
    get_invoice(conn, id)?;
    let tx = conn.unchecked_transaction()?;
    let now = time::now_iso();
    tx.execute(
        "UPDATE time_entries SET invoice_id = NULL, invoice_rate = NULL, updated_at = ?2 WHERE invoice_id = ?1",
        params![id, now],
    )?;
    tx.execute(
        "UPDATE invoices SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    tx.commit()?;
    Ok(())
}

/// Entries for a standalone timesheet (any billed state, billable only unless `all`).
pub fn timesheet_entries(
    conn: &Connection,
    client_id: &str,
    from: Option<&str>,
    to: Option<&str>,
    include_non_billable: bool,
) -> AppResult<Vec<Entry>> {
    let sql = format!(
        "{} AND c.id = ?1 AND e.ended_at IS NOT NULL AND (?2 IS NULL OR e.started_at >= ?2)
           AND (?3 IS NULL OR e.started_at < ?3) AND (?4 = 1 OR e.billable = 1)
         ORDER BY e.started_at",
        entries::ENTRY_SELECT
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(
            params![client_id, from, to, include_non_billable],
            entries::map_entry,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::entries::tests::{manual, setup};

    fn input(client_id: &str, ids: Vec<String>) -> InvoiceInput {
        InvoiceInput {
            id: None,
            client_id: client_id.into(),
            number: None,
            issue_date: "2026-02-01".into(),
            due_date: "2026-02-15".into(),
            period_start: None,
            period_end: None,
            grouping: "entry".into(),
            rounding_minutes: 0,
            discount_type: "none".into(),
            discount_value: 0.0,
            tax_label: "VAT".into(),
            tax_rate: 0.0,
            notes: String::new(),
            include_timesheet: true,
            entry_ids: ids,
        }
    }

    #[test]
    fn number_patterns() {
        let d = NaiveDate::from_ymd_opt(2026, 3, 9).unwrap();
        assert_eq!(format_number("INV-{YYYY}-{0001}", 42, d), "INV-2026-0042");
        assert_eq!(format_number("{YY}{MM}-{00}", 7, d), "2603-07");
        assert_eq!(format_number("A{foo}{1}", 12345, d), "A{foo}12345");
        assert!(pattern_has_sequence("INV-{YYYY}-{0001}"));
        assert!(!pattern_has_sequence("INV-{YYYY}"));
    }

    #[test]
    fn rounding() {
        assert_eq!(round_seconds(61, 0), 60);
        assert_eq!(round_seconds(91, 0), 120);
        assert_eq!(round_seconds(1, 15), 900);
        assert_eq!(round_seconds(900, 15), 900);
        assert_eq!(round_seconds(901, 6), 1080);
        assert_eq!(line_amount(5400, 10_000), 15_000);
        assert_eq!(line_amount(4800, 10_000), 13_333);
    }

    #[test]
    fn invoice_lifecycle_locks_and_releases_entries() {
        let (conn, pid) = setup();
        let a = manual(&conn, &pid, "2026-01-05T09:00:00Z", "2026-01-05T10:00:00Z");
        let b = manual(&conn, &pid, "2026-01-05T13:00:00Z", "2026-01-05T13:20:00Z");
        let client_id = a.client_id.clone();

        let mut inp = input(&client_id, vec![a.id.clone(), b.id.clone()]);
        inp.rounding_minutes = 15;
        inp.tax_rate = 10.0;
        inp.discount_type = "percent".into();
        inp.discount_value = 50.0;
        let d = save_draft(&conn, &inp).unwrap();
        assert_eq!(d.invoice.number, "INV-2026-0001");
        assert_eq!(d.lines.len(), 2);
        // 1h + 30m (20m rounded up to 15-min steps) at 60.00 = 90.00; -50% = 45.00; +10% = 49.50
        assert_eq!(d.invoice.subtotal, 9_000);
        assert_eq!(d.invoice.discount_amount, 4_500);
        assert_eq!(d.invoice.tax_amount, 450);
        assert_eq!(d.invoice.total, 4_950);

        // Entries are locked and cannot be invoiced twice.
        assert!(crate::services::entries::delete_entry(&conn, &a.id).is_err());
        assert!(save_draft(&conn, &input(&client_id, vec![a.id.clone()])).is_err());

        // Rate changes do not alter invoiced entries.
        conn.execute("UPDATE clients SET default_rate = 99999", [])
            .unwrap();
        let mut edit = inp.clone();
        edit.id = Some(d.invoice.id.clone());
        edit.entry_ids = vec![a.id.clone()];
        edit.grouping = "day".into();
        let d2 = save_draft(&conn, &edit).unwrap();
        assert_eq!(d2.lines.len(), 1);
        assert_eq!(d2.lines[0].rate, 6_000);
        assert_eq!(d2.entries.len(), 1);
        // b was released by removing it from the draft.
        assert!(crate::services::entries::get_entry(&conn, &b.id)
            .unwrap()
            .invoice_id
            .is_none());

        set_status(&conn, &d.invoice.id, "sent").unwrap();
        assert!(
            save_draft(&conn, &edit).is_err(),
            "sent invoices are read-only"
        );

        // Second invoice gets the next number.
        let d3 = save_draft(&conn, &input(&client_id, vec![b.id.clone()])).unwrap();
        assert_eq!(d3.invoice.number, "INV-2026-0002");
        let mut dup = input(&client_id, vec![b.id.clone()]);
        dup.id = Some(d3.invoice.id.clone());
        dup.number = Some("INV-2026-0001".into());
        assert!(save_draft(&conn, &dup).is_err(), "numbers must be unique");

        delete(&conn, &d.invoice.id).unwrap();
        assert!(crate::services::entries::get_entry(&conn, &a.id)
            .unwrap()
            .invoice_id
            .is_none());
        let t = totals(&conn, None).unwrap();
        assert_eq!(t[0].draft, d3.invoice.total);
    }

    #[test]
    fn project_grouping_and_fixed_discount() {
        let (conn, pid) = setup();
        let a = manual(&conn, &pid, "2026-01-05T09:00:00Z", "2026-01-05T10:00:00Z");
        let b = manual(&conn, &pid, "2026-01-06T09:00:00Z", "2026-01-06T10:00:00Z");
        let mut inp = input(&a.client_id, vec![a.id.clone(), b.id.clone()]);
        inp.grouping = "project".into();
        inp.discount_type = "fixed".into();
        inp.discount_value = 1_000.0;
        let p = preview(&conn, &inp).unwrap();
        assert_eq!(p.lines.len(), 1);
        assert_eq!(p.lines[0].quantity_seconds, 7200);
        assert_eq!(p.total, 12_000 - 1_000);
    }
}
