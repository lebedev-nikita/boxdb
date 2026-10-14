use boxdb::{Database, Format, Value};

#[path = "support/sqlite.rs"]
mod sqlite;

// The standalone stock SQLite executable includes this same workload source.
include!("support/workload.rs");

enum Backend {
    Sqlite(sqlite::Sqlite),
    Boxdb(Box<Database>),
}

impl Backend {
    fn open(engine: Engine, path: &std::path::Path, config: &Config) -> Result<Self> {
        Ok(match engine {
            Engine::Sqlite => Self::Sqlite(sqlite::Sqlite::open(engine, path, config)?),
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
            Self::Sqlite(db) => db.execute(sql),
            Self::Boxdb(db) => {
                black_box(db.execute(sql)?);
                Ok(())
            }
        }
    }

    fn query(&mut self, sql: &str) -> Result<Rows> {
        match self {
            Self::Sqlite(db) => db.query(sql),
            Self::Boxdb(db) => Ok(db.execute(sql)?.remove(0).rows),
        }
    }
}
