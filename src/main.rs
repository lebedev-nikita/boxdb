use boxdb::{Database, Error, Format, QueryResult, Result};
use clap::{Parser, Subcommand};
use std::{
    io::{self, BufRead, IsTerminal, Write},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    name = "boxdb",
    version,
    about = "Extended SQLite client with typed columns and readable JSONL storage",
    subcommand_precedence_over_arg = true,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Database file, or :memory: (the default).
    database: Option<PathBuf>,
    /// Format for a new file; existing files are detected automatically.
    #[arg(long, value_enum, default_value = "binary")]
    format: Format,
    /// Execute SQL and exit.
    #[arg(short = 'e', long)]
    execute: Option<String>,
    /// Emit one typed JSON result per query.
    #[arg(long)]
    json: bool,
}
#[derive(Subcommand)]
enum Command {
    /// Copy a database into a different storage format. Destination must be new.
    Convert {
        source: PathBuf,
        destination: PathBuf,
        #[arg(long, value_enum)]
        format: Format,
    },
}
fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("boxdb: {error}");
        std::process::exit(1);
    }
}
fn run(cli: Cli) -> Result<()> {
    if let Some(command) = cli.command {
        return match command {
            Command::Convert {
                source,
                destination,
                format,
            } => {
                if !source.is_file() {
                    return Err(Error("source database not found".into()));
                }
                let db = Database::open(&source, Format::Binary)?;
                Database::restore(destination, &db.export(format)?)?;
                Ok(())
            }
        };
    }
    let path = cli.database.unwrap_or_else(|| PathBuf::from(":memory:"));
    let mut db = if path.to_str() == Some(":memory:") {
        Database::memory()
    } else {
        Database::open(path, cli.format)?
    };
    if let Some(sql) = cli.execute {
        return execute(&mut db, &sql, cli.json);
    }
    let interactive = io::stdin().is_terminal();
    let mut pending = String::new();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    loop {
        if interactive {
            print!(
                "{}",
                if pending.is_empty() {
                    "boxdb> "
                } else {
                    "   ...> "
                }
            );
            io::stdout().flush().map_err(|e| Error(e.to_string()))?;
        }
        let mut line = String::new();
        if input
            .read_line(&mut line)
            .map_err(|e| Error(e.to_string()))?
            == 0
        {
            break;
        }
        if pending.is_empty() && line.trim_start().starts_with('.') {
            match line.trim() {
                ".quit" | ".exit" => break,
                ".help" => println!(
                    ".tables  .schema  .format  .help  .quit\nTerminate SQL statements with a semicolon."
                ),
                ".format" => println!("{:?}", db.format()),
                ".tables" => execute(
                    &mut db,
                    "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT GLOB '__boxdb_*' AND name NOT LIKE 'sqlite_%' ORDER BY name",
                    cli.json,
                )?,
                ".schema" => execute(
                    &mut db,
                    "SELECT sql FROM sqlite_schema ORDER BY type,name",
                    cli.json,
                )?,
                _ if interactive => eprintln!("unknown command; use .help"),
                _ => return Err(Error("unknown shell command".into())),
            }
            continue;
        }
        pending.push_str(&line);
        if complete(&pending) {
            if let Err(e) = execute(&mut db, &pending, cli.json) {
                if interactive {
                    eprintln!("boxdb: {e}");
                } else {
                    return Err(e);
                }
            }
            pending.clear();
        }
    }
    if !pending.trim().is_empty() {
        execute(&mut db, &pending, cli.json)?;
    }
    Ok(())
}
fn complete(sql: &str) -> bool {
    let Ok(sql) = std::ffi::CString::new(sql) else {
        return false;
    };
    // SAFETY: CString remains alive and provides a NUL-terminated SQL string.
    unsafe { rusqlite::ffi::sqlite3_complete(sql.as_ptr()) != 0 }
}
fn execute(db: &mut Database, sql: &str, json: bool) -> Result<()> {
    for result in db.execute(sql)? {
        print_result(&result, json)?;
    }
    Ok(())
}
fn print_result(result: &QueryResult, json: bool) -> Result<()> {
    if result.columns.is_empty() {
        return Ok(());
    }
    if json {
        println!(
            "{}",
            serde_json::to_string(result).map_err(|e| Error(e.to_string()))?
        );
    } else {
        println!("{}", result.columns.join("\t"));
        for row in &result.rows {
            println!(
                "{}",
                row.iter()
                    .map(|v| v
                        .to_string()
                        .replace('\\', "\\\\")
                        .replace('\t', "\\t")
                        .replace('\n', "\\n"))
                    .collect::<Vec<_>>()
                    .join("\t")
            );
        }
    }
    Ok(())
}
