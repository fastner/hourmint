//! Clients and projects.

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::{validation, AppError, AppResult};
use crate::models::{Client, ClientInput, Project, ProjectInput};
use crate::services::settings::is_currency_code;
use crate::time;

const CLIENT_SELECT: &str = "
    SELECT c.id, c.name, c.contact_name, c.email, c.billing_address, c.currency, c.default_rate,
           c.notes, c.archived_at, c.created_at,
           COALESCE(u.secs, 0), COALESCE(u.amount, 0), COALESCE(u.cnt, 0),
           (SELECT COUNT(*) FROM projects px WHERE px.client_id = c.id AND px.deleted_at IS NULL)
    FROM clients c
    LEFT JOIN (
        SELECT p.client_id AS client_id,
               SUM(CASE WHEN e.billable = 1 AND e.invoice_id IS NULL AND e.ended_at IS NOT NULL
                        THEN e.duration_seconds ELSE 0 END) AS secs,
               CAST(ROUND(SUM(CASE WHEN e.billable = 1 AND e.invoice_id IS NULL AND e.ended_at IS NOT NULL
                        THEN e.duration_seconds * COALESCE(p.rate_override, c2.default_rate) ELSE 0 END) / 3600.0) AS INTEGER) AS amount,
               COUNT(e.id) AS cnt
        FROM time_entries e
        JOIN projects p ON p.id = e.project_id
        JOIN clients c2 ON c2.id = p.client_id
        WHERE e.deleted_at IS NULL
        GROUP BY p.client_id
    ) u ON u.client_id = c.id
    WHERE c.deleted_at IS NULL";

fn map_client(r: &Row) -> rusqlite::Result<Client> {
    Ok(Client {
        id: r.get(0)?,
        name: r.get(1)?,
        contact_name: r.get(2)?,
        email: r.get(3)?,
        billing_address: r.get(4)?,
        currency: r.get(5)?,
        default_rate: r.get(6)?,
        notes: r.get(7)?,
        archived_at: r.get(8)?,
        created_at: r.get(9)?,
        unbilled_seconds: r.get(10)?,
        unbilled_amount: r.get(11)?,
        entry_count: r.get(12)?,
        project_count: r.get(13)?,
    })
}

pub fn list_clients(conn: &Connection, include_archived: bool) -> AppResult<Vec<Client>> {
    let sql = format!(
        "{CLIENT_SELECT} {} ORDER BY c.archived_at IS NOT NULL, c.name COLLATE NOCASE",
        if include_archived {
            ""
        } else {
            "AND c.archived_at IS NULL"
        }
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], map_client)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_client(conn: &Connection, id: &str) -> AppResult<Client> {
    let sql = format!("{CLIENT_SELECT} AND c.id = ?1");
    conn.query_row(&sql, [id], map_client)
        .optional()?
        .ok_or_else(|| AppError::NotFound("client".into()))
}

fn validate_client(input: &ClientInput) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return validation("Client name is required.");
    }
    if !is_currency_code(&input.currency) {
        return validation("Currency must be a three-letter ISO code, e.g. USD.");
    }
    if input.default_rate < 0 {
        return validation("Rate cannot be negative.");
    }
    Ok(())
}

pub fn create_client(conn: &Connection, input: &ClientInput) -> AppResult<Client> {
    validate_client(input)?;
    let id = time::new_id();
    let now = time::now_iso();
    conn.execute(
        "INSERT INTO clients (id, name, contact_name, email, billing_address, currency, default_rate, notes, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        params![
            id,
            input.name.trim(),
            input.contact_name.trim(),
            input.email.trim(),
            input.billing_address.trim(),
            input.currency,
            input.default_rate,
            input.notes,
            now
        ],
    )?;
    get_client(conn, &id)
}

pub fn update_client(conn: &Connection, id: &str, input: &ClientInput) -> AppResult<Client> {
    validate_client(input)?;
    let existing = get_client(conn, id)?;
    if existing.currency != input.currency {
        let invoiced: i64 = conn.query_row(
            "SELECT COUNT(*) FROM invoices WHERE client_id = ?1 AND deleted_at IS NULL",
            [id],
            |r| r.get(0),
        )?;
        if invoiced > 0 {
            return validation(
                "The currency cannot change once the client has invoices. Create a new client instead.",
            );
        }
    }
    conn.execute(
        "UPDATE clients SET name = ?2, contact_name = ?3, email = ?4, billing_address = ?5, currency = ?6,
                default_rate = ?7, notes = ?8, updated_at = ?9
         WHERE id = ?1 AND deleted_at IS NULL",
        params![
            id,
            input.name.trim(),
            input.contact_name.trim(),
            input.email.trim(),
            input.billing_address.trim(),
            input.currency,
            input.default_rate,
            input.notes,
            time::now_iso()
        ],
    )?;
    get_client(conn, id)
}

pub fn set_client_archived(conn: &Connection, id: &str, archived: bool) -> AppResult<Client> {
    let now = time::now_iso();
    if archived {
        let running: i64 = conn.query_row(
            "SELECT COUNT(*) FROM time_entries e JOIN projects p ON p.id = e.project_id
             WHERE p.client_id = ?1 AND e.ended_at IS NULL AND e.deleted_at IS NULL",
            [id],
            |r| r.get(0),
        )?;
        if running > 0 {
            return validation("Stop the running timer before archiving this client.");
        }
    }
    conn.execute(
        "UPDATE clients SET archived_at = ?2, updated_at = ?3 WHERE id = ?1 AND deleted_at IS NULL",
        params![id, if archived { Some(&now) } else { None }, now],
    )?;
    get_client(conn, id)
}

/// Delete is only allowed for clients that have never had entries or invoices.
pub fn delete_client(conn: &Connection, id: &str) -> AppResult<()> {
    let c = get_client(conn, id)?;
    let invoices: i64 = conn.query_row(
        "SELECT COUNT(*) FROM invoices WHERE client_id = ?1 AND deleted_at IS NULL",
        [id],
        |r| r.get(0),
    )?;
    if c.entry_count > 0 || invoices > 0 {
        return Err(AppError::Conflict(
            "This client has time entries or invoices, so it can only be archived.".into(),
        ));
    }
    let now = time::now_iso();
    conn.execute(
        "UPDATE projects SET deleted_at = ?2, updated_at = ?2 WHERE client_id = ?1 AND deleted_at IS NULL",
        params![id, now],
    )?;
    conn.execute(
        "UPDATE clients SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

const PROJECT_SELECT: &str = "
    SELECT p.id, p.client_id, c.name, c.archived_at IS NOT NULL, c.currency, p.name, p.rate_override,
           COALESCE(p.rate_override, c.default_rate), p.color, p.archived_at,
           (SELECT COUNT(*) FROM time_entries e WHERE e.project_id = p.id AND e.deleted_at IS NULL),
           (SELECT COALESCE(SUM(e.duration_seconds), 0) FROM time_entries e WHERE e.project_id = p.id AND e.deleted_at IS NULL)
    FROM projects p JOIN clients c ON c.id = p.client_id
    WHERE p.deleted_at IS NULL AND c.deleted_at IS NULL";

fn map_project(r: &Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        client_id: r.get(1)?,
        client_name: r.get(2)?,
        client_archived: r.get(3)?,
        currency: r.get(4)?,
        name: r.get(5)?,
        rate_override: r.get(6)?,
        effective_rate: r.get(7)?,
        color: r.get(8)?,
        archived_at: r.get(9)?,
        entry_count: r.get(10)?,
        total_seconds: r.get(11)?,
    })
}

pub fn list_projects(
    conn: &Connection,
    client_id: Option<&str>,
    include_archived: bool,
) -> AppResult<Vec<Project>> {
    let mut sql = PROJECT_SELECT.to_string();
    if !include_archived {
        sql.push_str(" AND p.archived_at IS NULL AND c.archived_at IS NULL");
    }
    if client_id.is_some() {
        sql.push_str(" AND p.client_id = ?1");
    }
    sql.push_str(
        " ORDER BY c.name COLLATE NOCASE, p.archived_at IS NOT NULL, p.name COLLATE NOCASE",
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = match client_id {
        Some(cid) => stmt
            .query_map([cid], map_project)?
            .collect::<Result<Vec<_>, _>>()?,
        None => stmt
            .query_map([], map_project)?
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(rows)
}

pub fn get_project(conn: &Connection, id: &str) -> AppResult<Project> {
    let sql = format!("{PROJECT_SELECT} AND p.id = ?1");
    conn.query_row(&sql, [id], map_project)
        .optional()?
        .ok_or_else(|| AppError::NotFound("project".into()))
}

fn validate_project(input: &ProjectInput) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return validation("Project name is required.");
    }
    if matches!(input.rate_override, Some(r) if r < 0) {
        return validation("Rate cannot be negative.");
    }
    Ok(())
}

pub fn create_project(conn: &Connection, input: &ProjectInput) -> AppResult<Project> {
    validate_project(input)?;
    get_client(conn, &input.client_id)?;
    let id = time::new_id();
    let now = time::now_iso();
    conn.execute(
        "INSERT INTO projects (id, client_id, name, rate_override, color, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![
            id,
            input.client_id,
            input.name.trim(),
            input.rate_override,
            input.color,
            now
        ],
    )?;
    get_project(conn, &id)
}

pub fn update_project(conn: &Connection, id: &str, input: &ProjectInput) -> AppResult<Project> {
    validate_project(input)?;
    let existing = get_project(conn, id)?;
    if existing.client_id != input.client_id && existing.entry_count > 0 {
        return validation("A project with time entries cannot move to another client.");
    }
    conn.execute(
        "UPDATE projects SET client_id = ?2, name = ?3, rate_override = ?4, color = ?5, updated_at = ?6
         WHERE id = ?1 AND deleted_at IS NULL",
        params![id, input.client_id, input.name.trim(), input.rate_override, input.color, time::now_iso()],
    )?;
    get_project(conn, id)
}

pub fn set_project_archived(conn: &Connection, id: &str, archived: bool) -> AppResult<Project> {
    let now = time::now_iso();
    if archived {
        let running: i64 = conn.query_row(
            "SELECT COUNT(*) FROM time_entries WHERE project_id = ?1 AND ended_at IS NULL AND deleted_at IS NULL",
            [id],
            |r| r.get(0),
        )?;
        if running > 0 {
            return validation("Stop the running timer before archiving this project.");
        }
    }
    conn.execute(
        "UPDATE projects SET archived_at = ?2, updated_at = ?3 WHERE id = ?1 AND deleted_at IS NULL",
        params![id, if archived { Some(&now) } else { None }, now],
    )?;
    get_project(conn, id)
}

pub fn delete_project(conn: &Connection, id: &str) -> AppResult<()> {
    let p = get_project(conn, id)?;
    if p.entry_count > 0 {
        return Err(AppError::Conflict(
            "This project has time entries, so it can only be archived.".into(),
        ));
    }
    let now = time::now_iso();
    conn.execute(
        "UPDATE projects SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    pub fn client_input(name: &str) -> ClientInput {
        ClientInput {
            name: name.into(),
            contact_name: String::new(),
            email: String::new(),
            billing_address: String::new(),
            currency: "USD".into(),
            default_rate: 10_000,
            notes: String::new(),
        }
    }

    #[test]
    fn archive_hides_from_pickers_and_rate_override_applies() {
        let conn = db::open_in_memory().unwrap();
        let c = create_client(&conn, &client_input("Acme")).unwrap();
        let p = create_project(
            &conn,
            &ProjectInput {
                client_id: c.id.clone(),
                name: "Site".into(),
                rate_override: Some(12_000),
                color: None,
            },
        )
        .unwrap();
        assert_eq!(p.effective_rate, 12_000);
        assert_eq!(list_projects(&conn, None, false).unwrap().len(), 1);
        set_client_archived(&conn, &c.id, true).unwrap();
        assert!(list_clients(&conn, false).unwrap().is_empty());
        assert!(list_projects(&conn, None, false).unwrap().is_empty());
        assert_eq!(list_clients(&conn, true).unwrap().len(), 1);
    }

    #[test]
    fn rejects_bad_input() {
        let conn = db::open_in_memory().unwrap();
        let mut i = client_input(" ");
        assert!(create_client(&conn, &i).is_err());
        i.name = "X".into();
        i.currency = "usd".into();
        assert!(create_client(&conn, &i).is_err());
    }
}
