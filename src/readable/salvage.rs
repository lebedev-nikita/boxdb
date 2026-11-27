//! Recovery of a damaged log: each line is a transaction, so a line that
//! cannot be read or applied is rejected as a whole and later lines continue.
use super::*;

pub(crate) fn salvage(bytes: &[u8]) -> Result<(Connection, Vec<Rejected>)> {
    if bytes.len() > MAX_SNAPSHOT {
        return Err(Error::new("readable database exceeds 256 MiB limit".into()));
    }
    let mut rejected = vec![];
    let extent = extent(bytes);
    let committed = &bytes[..extent.committed];
    let text = String::from_utf8_lossy(committed);
    let lines = replay::lines(&text);
    if extent.torn {
        rejected.push((
            crate::RejectedDocument {
                document: lines.len() + 1,
                line: lines.len() + 1,
                code: "BOXDB_INCOMPLETE_COMMIT".into(),
                message: "an interrupted write left an incomplete line".into(),
            },
            bytes[extent.committed..].to_vec(),
        ));
    }
    let db = replay::Applier::connection()?;
    let mut applier = replay::Applier::new(&db);
    applier.validate = true;
    let mut header = false;
    for (index, (number, line)) in lines.iter().enumerate() {
        let entry = match replay::parse(*number, line) {
            Ok(Some(entry)) => entry,
            Ok(None) => continue,
            Err(error) => {
                rejected.push(reject(
                    index + 1,
                    *number,
                    &error,
                    "invalid YAML line",
                    line,
                ));
                continue;
            }
        };
        if !header {
            replay::check_header(&entry).map_err(|error| {
                Error::new(format!(
                    "recovery requires an intact readable header: {error}"
                ))
                .with_code("BOXDB_FORMAT")
            })?;
            header = true;
            continue;
        }
        db.execute_batch("SAVEPOINT __boxdb_line")?;
        match applier.entry(entry.steps) {
            Ok(()) => db.execute_batch("RELEASE __boxdb_line")?,
            Err(error) => {
                db.execute_batch("ROLLBACK TO __boxdb_line; RELEASE __boxdb_line")?;
                applier.forget();
                rejected.push(reject(
                    index + 1,
                    *number,
                    &error,
                    "line could not be restored",
                    line,
                ));
            }
        }
    }
    if !header {
        return Err(
            Error::new("recovery requires an intact readable header".into())
                .with_code("BOXDB_FORMAT"),
        );
    }
    db.execute_batch("COMMIT")?;
    db.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER,
        true,
    )?;
    db.flush_prepared_statement_cache();
    rejected.sort_by_key(|(document, _)| (document.document, document.line));
    Ok((db, rejected))
}

fn reject(document: usize, line: usize, error: &Error, message: &str, text: &str) -> Rejected {
    (
        crate::RejectedDocument {
            document,
            line,
            code: error.code().into(),
            message: format!("{message}: {error}"),
        },
        format!("{text}\n").into_bytes(),
    )
}
