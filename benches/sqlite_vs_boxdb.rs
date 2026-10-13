use boxdb::{Database, Format, Value};
use clap::Parser;
use rusqlite::{Connection, types::ValueRef};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{hint::black_box, path::PathBuf, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type Rows = Vec<Vec<Value>>;
const SCHEMA: &str = "CREATE TABLE records (
    id INTEGER PRIMARY KEY, category INTEGER NOT NULL,
    score INTEGER NOT NULL, payload TEXT NOT NULL) STRICT";
const ALL_ROWS: &str = "SELECT id,category,score,payload FROM records ORDER BY id";

#[derive(Parser, Serialize)]
#[command(about = "Compare file-backed SQLite and BoxDB binary or YAML storage (release profile)")]
struct Config {
    /// Number of deterministic rows.
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..=i32::MAX as i64))]
    rows: u32,
    /// Measured samples per engine, after one discarded warmup sample.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..))]
    samples: u32,
    /// Queries per read workload.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
    reads: u32,
    /// Repeat the warmed query set to reduce noise in short query timings.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..))]
    read_repetitions: u32,
    /// Rows per multi-value INSERT; all INSERTs share one transaction.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
    batch_size: u32,
    /// Point updates/deletes per workload, capped at --rows.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..))]
    writes: u32,
    /// Match BoxDB's hardware flush on macOS (off uses SQLite's weaker flush).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    sqlite_fullfsync: bool,
    /// Reuse SQLite prepared statements (BoxDB also uses prepare_cached).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    sqlite_cached: bool,
    /// Hold SQLite's lock for the connection lifetime (off matches BoxDB).
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    sqlite_exclusive: bool,
    /// Validate payload as VARCHAR(100); SQLite uses its equivalent CHECK.
    #[arg(long)]
    typed: bool,
    /// BoxDB storage to compare with the direct SQLite client.
    #[arg(long, default_value = "binary", value_parser = ["binary", "yaml"])]
    boxdb_format: String,
    /// Existing parent directory for temporary databases (selects the filesystem).
    #[arg(long)]
    directory: Option<PathBuf>,
    /// Write configuration, versions, raw samples and medians as JSON.
    #[arg(long)]
    json: Option<PathBuf>,
    // Cargo passes this to custom benchmark harnesses.
    #[arg(long, hide = true)]
    #[serde(skip)]
    bench: bool,
}

#[derive(Clone, Copy)]
enum Engine {
    Sqlite,
    Boxdb,
    Yaml,
}

impl Engine {
    fn name(self) -> &'static str {
        match self {
            Self::Sqlite => "SQLite",
            Self::Boxdb => "BoxDB binary",
            Self::Yaml => "BoxDB YAML",
        }
    }
}

enum Backend {
    Sqlite(Box<Connection>, bool),
    Boxdb(Box<Database>),
}

impl Backend {
    fn open(engine: Engine, path: &std::path::Path, config: &Config) -> Result<Self> {
        Ok(match engine {
            Engine::Sqlite => {
                let db = Connection::open(path)?;
                // Both clients use SQLite WAL, FULL synchronization and normal locks.
                db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL")?;
                db.pragma_update(None, "fullfsync", config.sqlite_fullfsync)?;
                db.pragma_update(None, "foreign_keys", true)?;
                db.busy_timeout(std::time::Duration::from_secs(5))?;
                db.pragma_update(
                    None,
                    "locking_mode",
                    if config.sqlite_exclusive {
                        "EXCLUSIVE"
                    } else {
                        "NORMAL"
                    },
                )?;
                db.set_prepared_statement_cache_capacity(512);
                Self::Sqlite(Box::new(db), config.sqlite_cached)
            }
            Engine::Boxdb | Engine::Yaml => Self::Boxdb(Box::new(Database::open(
                path,
                if matches!(engine, Engine::Yaml) {
                    Format::Readable
                } else {
                    Format::Binary
                },
            )?)),
        })
    }

    fn execute(&mut self, sql: &str) -> Result<()> {
        match self {
            Self::Sqlite(db, cached) => {
                if *cached {
                    db.prepare_cached(sql)?.execute([])?;
                } else {
                    db.execute_batch(sql)?;
                }
            }
            Self::Boxdb(db) => {
                black_box(db.execute(sql)?);
            }
        }
        Ok(())
    }

    fn query(&mut self, sql: &str) -> Result<Rows> {
        match self {
            Self::Boxdb(db) => Ok(db.execute(sql)?.remove(0).rows),
            Self::Sqlite(db, cached) => {
                if *cached {
                    let mut statement = db.prepare_cached(sql)?;
                    sqlite_rows(&mut statement)
                } else {
                    sqlite_rows(&mut db.prepare(sql)?)
                }
            }
        }
    }
}

fn sqlite_rows(statement: &mut rusqlite::Statement<'_>) -> Result<Rows> {
    let columns = statement.column_count();
    let mut cursor = statement.query([])?;
    let mut rows = Vec::new();
    while let Some(row) = cursor.next()? {
        let mut values = Vec::with_capacity(columns);
        for column in 0..columns {
            values.push(match row.get_ref(column)? {
                ValueRef::Integer(n) => Value::Integer(n),
                ValueRef::Text(s) => Value::Text(std::str::from_utf8(s)?.into()),
                other => {
                    return Err(format!("unexpected SQLite value: {other:?}").into());
                }
            });
        }
        rows.push(values);
    }
    Ok(rows)
}

struct ReadWorkload {
    name: &'static str,
    queries: Vec<(String, Rows)>,
}

struct Workload {
    rows: Rows,
    inserts: Vec<String>,
    reads: Vec<ReadWorkload>,
    updates: Vec<String>,
    deletes: Vec<String>,
}

impl Workload {
    fn new(config: &Config) -> Self {
        // SQL, payloads and expected results are generated outside all timers.
        let rows: Rows = (1..=config.rows)
            .map(|id| {
                vec![
                    Value::Integer(id.into()),
                    Value::Integer((id % 10).into()),
                    Value::Integer((id % 1000).into()),
                    Value::Text(format!(
                        "record-{id:08}-{}",
                        hex::encode(Sha256::digest(id.to_le_bytes()))
                    )),
                ]
            })
            .collect();
        let inserts = rows
            .chunks(config.batch_size as usize)
            .map(|chunk| {
                let values = chunk
                    .iter()
                    .map(|row| format!("({},{},{},'{}')", row[0], row[1], row[2], row[3]))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("INSERT INTO records VALUES {values}")
            })
            .collect();
        let mut points = Vec::new();
        let mut ranges = Vec::new();
        for i in 0..config.reads {
            let index = ((u64::from(i) * 7919 + 17) % u64::from(config.rows)) as usize;
            let id = index + 1;
            points.push((
                format!("SELECT id,category,score,payload FROM records WHERE id={id}"),
                vec![rows[index].clone()],
            ));
            let end = (index + 50).min(rows.len());
            ranges.push((
                format!("SELECT id,category,score,payload FROM records WHERE id>={id} AND id<={} ORDER BY id", end),
                rows[index..end].to_vec(),
            ));
        }
        let aggregate: Rows = (0..10)
            .filter_map(|category| {
                let scores: Vec<i64> = (1..=config.rows)
                    .filter(|id| id % 10 == category)
                    .map(|id| i64::from(id % 1000))
                    .collect();
                (!scores.is_empty()).then(|| {
                    vec![
                        Value::Integer(category.into()),
                        Value::Integer(scores.len() as i64),
                        Value::Integer(scores.iter().sum()),
                    ]
                })
            })
            .collect();
        let writes = config.writes.min(config.rows);
        Self {
            rows,
            inserts,
            reads: vec![
                ReadWorkload { name: "point_lookup", queries: points },
                ReadWorkload { name: "range_scan", queries: ranges },
                ReadWorkload {
                    name: "aggregate",
                    queries: vec![("SELECT category,COUNT(*),SUM(score) FROM records GROUP BY category ORDER BY category".into(), aggregate); config.reads as usize],
                },
            ],
            updates: (1..=writes).map(|id| format!("UPDATE records SET score=score+1 WHERE id={id}")).collect(),
            deletes: (1..=writes).map(|id| format!("DELETE FROM records WHERE id={id}")).collect(),
        }
    }
}

#[derive(Serialize)]
struct Timing {
    workload: &'static str,
    unit: &'static str,
    units: usize,
    elapsed_ms: f64,
}

fn measure(
    timings: &mut Vec<Timing>,
    workload: &'static str,
    unit: &'static str,
    units: usize,
    run: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let start = Instant::now();
    run()?;
    timings.push(Timing {
        workload,
        unit,
        units,
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
    });
    Ok(())
}

fn transaction(db: &mut Backend, statements: &[String]) -> Result<()> {
    db.execute("BEGIN")?;
    for sql in statements {
        db.execute(sql)?;
    }
    db.execute("COMMIT")
}

fn verify(db: &mut Backend, sql: &str, expected: &Rows) -> Result<()> {
    if db.query(sql)? != *expected {
        return Err(format!("result mismatch for {sql}").into());
    }
    Ok(())
}

#[derive(Serialize)]
struct Sample {
    engine: &'static str,
    database_bytes: u64,
    wal_bytes: u64,
    total_storage_bytes: u64,
    timings: Vec<Timing>,
}

fn sample(engine: Engine, config: &Config, workload: &Workload) -> Result<Sample> {
    let temp = match &config.directory {
        Some(parent) => tempfile::tempdir_in(parent)?,
        None => tempfile::tempdir()?,
    };
    let path = temp.path().join(if matches!(engine, Engine::Yaml) {
        "data.yaml"
    } else {
        "data.db"
    });
    let mut db = Backend::open(engine, &path, config)?;
    if config.typed {
        let schema = match engine {
            Engine::Boxdb | Engine::Yaml => {
                SCHEMA.replace("payload TEXT NOT NULL", "payload VARCHAR(100) NOT NULL")
            }
            Engine::Sqlite => SCHEMA.replace(
                "payload TEXT NOT NULL",
                "payload TEXT NOT NULL CHECK(length(payload)<=100)",
            ),
        };
        db.execute(&schema)?;
    } else {
        db.execute(SCHEMA)?;
    }
    // Both clients have identical schemas, including the explicit ID index.
    db.execute("CREATE INDEX records_id ON records(id)")?;
    let mut timings = Vec::new();
    measure(
        &mut timings,
        "insert_transaction",
        "row",
        workload.rows.len(),
        || transaction(&mut db, &workload.inserts),
    )?;
    verify(&mut db, ALL_ROWS, &workload.rows)?;
    let database_bytes = std::fs::metadata(&path)?.len();
    let wal_bytes = std::fs::metadata(path.with_file_name("data.db-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    for read in &workload.reads {
        // Verify and warm each query before timing; both engines materialize rows.
        for (sql, expected) in &read.queries {
            verify(&mut db, sql, expected)?;
        }
        measure(
            &mut timings,
            read.name,
            "query",
            read.queries.len() * config.read_repetitions as usize,
            || {
                for _ in 0..config.read_repetitions {
                    for (sql, _) in &read.queries {
                        black_box(db.query(sql)?);
                    }
                }
                Ok(())
            },
        )?;
    }
    let mut expected = workload.rows.clone();
    for (name, in_transaction) in [("update_transaction", true), ("update_autocommit", false)] {
        measure(&mut timings, name, "row", workload.updates.len(), || {
            if in_transaction {
                transaction(&mut db, &workload.updates)
            } else {
                for sql in &workload.updates {
                    db.execute(sql)?;
                }
                Ok(())
            }
        })?;
        for row in expected.iter_mut().take(workload.updates.len()) {
            if let Value::Integer(score) = &mut row[2] {
                *score += 1;
            }
        }
        verify(&mut db, ALL_ROWS, &expected)?;
    }
    measure(
        &mut timings,
        "delete_transaction",
        "row",
        workload.deletes.len(),
        || transaction(&mut db, &workload.deletes),
    )?;
    expected.drain(..workload.deletes.len());
    verify(&mut db, ALL_ROWS, &expected)?;
    drop(db);
    // SQLite opens lazily: include a full scan to actually load persisted data.
    let mut reopened = None;
    let mut persisted = None;
    measure(&mut timings, "reopen_scan", "database", 1, || {
        let mut db = Backend::open(engine, &path, config)?;
        persisted = Some(black_box(db.query(ALL_ROWS)?));
        reopened = Some(db);
        Ok(())
    })?;
    if persisted.as_ref() != Some(&expected) {
        return Err("reopened database differs from committed rows".into());
    }
    // Close both clients before deleting the temporary directory.
    drop(reopened);
    Ok(Sample {
        engine: engine.name(),
        database_bytes,
        wal_bytes,
        total_storage_bytes: database_bytes + wal_bytes,
        timings,
    })
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

#[derive(Serialize)]
struct Summary {
    engine: &'static str,
    workload: &'static str,
    unit: &'static str,
    median_ms: f64,
    median_us_per_unit: f64,
}

#[derive(Serialize)]
struct Comparison {
    workload: &'static str,
    unit: &'static str,
    sqlite_median_us_per_unit: f64,
    boxdb_median_us_per_unit: f64,
    boxdb_over_sqlite: f64,
}

#[derive(Serialize)]
struct Report {
    boxdb_version: &'static str,
    sqlite_version: &'static str,
    os: &'static str,
    arch: &'static str,
    sqlite_settings: String,
    config: Config,
    samples: Vec<Sample>,
    summaries: Vec<Summary>,
    comparisons: Vec<Comparison>,
}

fn main() -> Result<()> {
    let config = Config::parse();
    let workload = Workload::new(&config);
    println!(
        "BoxDB {} ({}) vs SQLite {} | {}/{}",
        env!("CARGO_PKG_VERSION"),
        config.boxdb_format,
        rusqlite::version(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "rows={} samples={} reads={} read_repetitions={} batch_size={} writes={} | SQLite WAL/FULL fullfsync={} cached={} exclusive={} typed={}",
        config.rows,
        config.samples,
        config.reads,
        config.read_repetitions,
        config.batch_size,
        workload.updates.len(),
        config.sqlite_fullfsync,
        config.sqlite_cached,
        config.sqlite_exclusive,
        config.typed
    );
    let mut samples = Vec::new();
    let boxdb = if config.boxdb_format == "yaml" {
        Engine::Yaml
    } else {
        Engine::Boxdb
    };
    for iteration in 0..=config.samples {
        eprintln!(
            "{} {iteration}/{}",
            if iteration == 0 { "Warmup" } else { "Sample" },
            config.samples
        );
        // Alternate order to reduce systematic filesystem/cache/thermal bias.
        let engines = if iteration % 2 == 0 {
            [Engine::Sqlite, boxdb]
        } else {
            [boxdb, Engine::Sqlite]
        };
        for engine in engines {
            let result = sample(engine, &config, &workload)?;
            if iteration != 0 {
                samples.push(result);
            }
        }
    }
    println!(
        "\n{:<23} {:>13} {:>13} {:>12}  unit",
        "workload", "SQLite us/unit", "BoxDB us/unit", "BoxDB/SQLite"
    );
    let mut summaries = Vec::new();
    let mut comparisons = Vec::new();
    for (index, timing) in samples[0].timings.iter().enumerate() {
        let mut per_unit = Vec::new();
        for engine in [Engine::Sqlite, boxdb] {
            let elapsed = samples
                .iter()
                .filter(|s| s.engine == engine.name())
                .map(|s| s.timings[index].elapsed_ms)
                .collect();
            let median_ms = median(elapsed);
            let median_us_per_unit = median_ms * 1000.0 / timing.units as f64;
            per_unit.push(median_us_per_unit);
            summaries.push(Summary {
                engine: engine.name(),
                workload: timing.workload,
                unit: timing.unit,
                median_ms,
                median_us_per_unit,
            });
        }
        let comparison = Comparison {
            workload: timing.workload,
            unit: timing.unit,
            sqlite_median_us_per_unit: per_unit[0],
            boxdb_median_us_per_unit: per_unit[1],
            boxdb_over_sqlite: per_unit[1] / per_unit[0],
        };
        println!(
            "{:<23} {:>13.3} {:>13.3} {:>11.2}x  {}",
            timing.workload, per_unit[0], per_unit[1], comparison.boxdb_over_sqlite, timing.unit
        );
        comparisons.push(comparison);
    }
    for engine in [Engine::Sqlite, boxdb] {
        let bytes = median(
            samples
                .iter()
                .filter(|s| s.engine == engine.name())
                .map(|s| s.total_storage_bytes as f64)
                .collect(),
        );
        println!(
            "{} storage after insert: {bytes:.0} bytes (main + WAL, SHM excluded)",
            engine.name()
        );
    }
    if let Some(path) = config.json.clone() {
        let report = Report {
            boxdb_version: env!("CARGO_PKG_VERSION"),
            sqlite_version: rusqlite::version(),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            sqlite_settings: format!(
                "journal_mode=WAL; synchronous=FULL; foreign_keys=ON; busy_timeout=5000; fullfsync={}; locking_mode={}",
                if config.sqlite_fullfsync { "ON" } else { "OFF" },
                if config.sqlite_exclusive {
                    "EXCLUSIVE"
                } else {
                    "NORMAL"
                }
            ),
            config,
            samples,
            summaries,
            comparisons,
        };
        std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
        println!("JSON report: {}", path.display());
    }
    Ok(())
}
