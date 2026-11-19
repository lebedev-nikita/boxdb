//! Recovery of independently delimited documents, rows and commits.
use super::*;

/// Recover only independently delimited documents and rows. Metadata is never inferred.
pub(crate) fn salvage(bytes: &[u8]) -> Result<(Connection, Vec<Rejected>)> {
    if bytes.len() > MAX_SNAPSHOT {
        return Err(Error::new("readable database exceeds 256 MiB limit".into()));
    }
    enum Content<'a> {
        Record(Record),
        Rows {
            table: String,
            head: &'a [u8],
            items: Vec<(usize, &'a [u8])>,
        },
    }
    struct Part<'a> {
        document: usize,
        line: usize,
        fragment: &'a [u8],
        content: Content<'a>,
    }
    let reject = |document: usize, line: usize, error: &Error, message: &str, fragment: &[u8]| {
        (
            crate::RejectedDocument {
                document,
                line,
                code: error.code().into(),
                message: message.into(),
            },
            fragment.to_vec(),
        )
    };
    let mut parts = vec![];
    let mut rejected = vec![];
    let extent = extent(bytes);
    let fragments = documents(&bytes[..extent.committed]);
    if extent.torn {
        rejected.push((
            crate::RejectedDocument {
                document: fragments.len() + 1,
                line: bytes[..extent.committed]
                    .iter()
                    .filter(|b| **b == b'\n')
                    .count()
                    + 1,
                code: "BOXDB_INCOMPLETE_COMMIT".into(),
                message: "an interrupted write left an incomplete commit".into(),
            },
            bytes[extent.committed..].to_vec(),
        ));
    }
    for (index, (line, fragment)) in fragments.into_iter().enumerate() {
        let document = index + 1;
        let content = match rows_section(fragment) {
            Some(RowsSection::Block { head, items }) => {
                rows_head(&fragment[..head]).map(|table| Content::Rows {
                    table,
                    head: &fragment[..items],
                    items: row_items(
                        &fragment[items..],
                        line + fragment[..items].iter().filter(|b| **b == b'\n').count(),
                    ),
                })
            }
            Some(RowsSection::Inline) => {
                Err(Error::new("rows must use a block sequence".into()).with_code("BOXDB_YAML"))
            }
            None => parse_document(fragment, &mut |_| Ok(None)).and_then(|record| match record {
                Record::Rows { .. } => {
                    Err(Error::new("rows must use a block sequence".into()).with_code("BOXDB_YAML"))
                }
                record => Ok(Content::Record(record)),
            }),
        };
        match content {
            Ok(content) => parts.push(Part {
                document,
                line,
                fragment,
                content,
            }),
            Err(_) => rejected.push((
                crate::RejectedDocument {
                    document,
                    line,
                    code: "BOXDB_YAML".into(),
                    message: "invalid YAML document".into(),
                },
                fragment.to_vec(),
            )),
        }
    }
    let header = match parts.first() {
        Some(Part {
            document: 1,
            content: Content::Record(record),
            ..
        }) => Some(record),
        _ => None,
    };
    if !matches!(header, Some(Record::Database { .. })) {
        return Err(
            Error::new("recovery requires an intact readable header".into())
                .with_code("BOXDB_FORMAT"),
        );
    }
    check_header(header)?;
    if parts
        .iter()
        .skip(1)
        .any(|part| matches!(part.content, Content::Record(Record::Database { .. })))
    {
        return Err(Error::new("duplicate readable header".into()).with_code("BOXDB_FORMAT"));
    }
    let mut base = parts[0].fragment.to_vec();
    for part in &parts[1..] {
        if matches!(&part.content, Content::Record(Record::Schema { object_type, .. }) if object_type == "table")
        {
            base.extend_from_slice(part.fragment);
        }
    }
    for part in &parts {
        if matches!(&part.content, Content::Rows { table, .. } if table.starts_with(METADATA_PREFIX))
        {
            base.extend_from_slice(part.fragment);
        }
    }
    let db = decode(&base)
        .map_err(|error| error.with_context("recover readable schema and metadata"))?;
    let types = schema::load(&db)?;
    schema::install(&db, &types)?;
    db.pragma_update(None, "foreign_keys", false)?;
    db.execute_batch("BEGIN")?;
    let mut layouts = Layouts::new(&db, Some(types));
    for part in &parts {
        let Content::Rows { table, head, items } = &part.content else {
            continue;
        };
        if table.starts_with(METADATA_PREFIX) {
            continue;
        }
        let layout = match layouts.get(table) {
            Ok(layout) => layout,
            Err(error) => {
                rejected.push(reject(
                    part.document,
                    part.line,
                    &error,
                    "row document could not be restored",
                    part.fragment,
                ));
                continue;
            }
        };
        for (line, item) in items {
            let result = (|| -> Result<()> {
                for row in parse_items(item, &layout)? {
                    layout.check(&row)?;
                    layout.insert(&db, row)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                rejected.push(reject(
                    part.document,
                    *line,
                    &error,
                    "row could not be restored",
                    &[*head, *item].concat(),
                ));
            }
        }
    }
    let mut rejected_part = |part: &Part, error: &Error, message: &str| {
        rejected.push(reject(
            part.document,
            part.line,
            error,
            message,
            part.fragment,
        ))
    };
    for part in &parts {
        if let Content::Record(record @ Record::Sequence { .. }) = &part.content
            && let Err(error) = apply_deferred(&db, record)
        {
            rejected_part(
                part,
                &error,
                "schema or sequence document could not be restored",
            );
        }
    }
    // Each commit is atomic: a damaged commit is rejected as a whole.
    let mut resolve = |table: &str| layouts.get(table).map(Some);
    for part in &parts {
        if !matches!(part.content, Content::Record(Record::Commit { .. })) {
            continue;
        }
        db.execute_batch("SAVEPOINT __boxdb_commit")?;
        let result = (|| -> Result<()> {
            let Record::Commit { tables, sequences } = parse_document(part.fragment, &mut resolve)?
            else {
                unreachable!()
            };
            for changes in &tables {
                let layout = changes.layout.as_ref().unwrap();
                for row in &changes.put {
                    layout.check(row)?;
                }
            }
            apply_commit(&db, tables, &sequences)
        })();
        match result {
            Ok(()) => db.execute_batch("RELEASE __boxdb_commit")?,
            Err(error) => {
                db.execute_batch("ROLLBACK TO __boxdb_commit; RELEASE __boxdb_commit")?;
                rejected_part(part, &error, "commit could not be restored");
            }
        }
    }
    // Application triggers are installed after rows, so recovery does not replay writes.
    for part in &parts {
        if let Content::Record(record @ Record::Schema { .. }) = &part.content
            && let Err(error) = apply_deferred(&db, record)
        {
            rejected_part(
                part,
                &error,
                "schema or sequence document could not be restored",
            );
        }
    }
    db.execute_batch("COMMIT")?;
    rejected.sort_by_key(|(document, _)| (document.document, document.line));
    Ok((db, rejected))
}
