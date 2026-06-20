//! A one-command summary of a database file: every table, its row count, and the
//! on-disk size. A small operator's tool, the rough equivalent of `\dt+`.
//!
//! ```text
//! cargo run --release --bin dbstat -- mydb.db
//! ```
//!
//! It opens the database as the bootstrap user (which bypasses row-level
//! security), so the counts are the true totals across every tenant, not one
//! tenant's fenced view.

use std::process::ExitCode;

use picklejar::{Database, QueryOutcome};

/// The row count of a table, read through the engine. The count is taken via the
/// value's text form and parsed, so this does not depend on which numeric
/// `Value` variant `COUNT(*)` happens to produce.
fn row_count(db: &mut Database, table: &str) -> Option<i64> {
    match db.execute(&format!("SELECT COUNT(*) FROM {table}")) {
        Ok(QueryOutcome::Rows { rows, .. }) => rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.to_string().parse::<i64>().ok()),
        _ => None,
    }
}

/// A byte count rendered in whichever unit reads cleanly.
#[allow(clippy::cast_precision_loss)]
fn human_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes >= 1 << 20 {
        format!("{:.1} MiB", b / (1u64 << 20) as f64)
    } else if bytes >= 1 << 10 {
        format!("{:.1} KiB", b / (1u64 << 10) as f64)
    } else {
        format!("{bytes} B")
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: dbstat <db_path>");
        return ExitCode::FAILURE;
    };

    let mut db = match Database::open(std::path::Path::new(path)) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("could not open {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let tables = db.table_names();
    let name_width = tables.iter().map(String::len).max().unwrap_or(5).max(5);

    println!("\ndatabase: {path}");
    let file_size = std::fs::metadata(path).map_or(0, |m| m.len());
    println!("heap file: {}", human_bytes(file_size));
    println!();

    if tables.is_empty() {
        println!("(no user tables)");
        return ExitCode::SUCCESS;
    }

    println!("  {:<width$}   {:>12}", "TABLE", "ROWS", width = name_width);
    println!("  {:-<width$}   {:->12}", "", "", width = name_width);

    let mut total: i64 = 0;
    let mut unknown = 0usize;
    for table in &tables {
        if let Some(n) = row_count(&mut db, table) {
            total += n;
            println!("  {table:<name_width$}   {n:>12}");
        } else {
            unknown += 1;
            println!("  {table:<name_width$}   {:>12}", "?");
        }
    }
    println!("  {:-<width$}   {:->12}", "", "", width = name_width);
    println!("  {:<width$}   {total:>12}", "TOTAL", width = name_width);
    println!("\n{} table(s), {total} row(s) counted.", tables.len());
    if unknown > 0 {
        println!("({unknown} table(s) could not be counted.)");
    }
    ExitCode::SUCCESS
}
