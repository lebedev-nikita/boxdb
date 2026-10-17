//! Revision comparison using independent processes and durable binary storage.
use boxdb::{Database, Format, Value};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    time::Instant,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
struct Config {
    #[arg(long, default_value_t = 7)]
    samples: usize,
    #[arg(long, default_value_t = 400)]
    writes: usize,
    #[arg(long, default_value_t = 50)]
    batch: usize,
    #[arg(long, default_value_t = 10000)]
    reads: usize,
    #[arg(long)]
    json: Option<PathBuf>,
    #[arg(long)]
    directory: Option<PathBuf>,
    #[arg(long, hide = true)]
    worker: Option<PathBuf>,
    #[arg(long, hide = true, default_value = "autocommit")]
    mode: String,
    #[arg(long, hide = true)]
    bench: bool,
}

#[derive(Serialize, Deserialize)]
struct Worker {
    elapsed_us: f64,
    latency_us: Vec<f64>,
    busy_retries: usize,
}

#[derive(Serialize)]
struct Sample {
    elapsed_us: f64,
    operations: usize,
    operations_per_second: f64,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    latency_us: Vec<f64>,
    busy_retries: usize,
}

#[derive(Serialize)]
struct Case {
    mode: String,
    processes: usize,
    tables: usize,
    samples: Vec<Sample>,
}

fn worker(config: &Config, path: &PathBuf) -> Result<()> {
    let mut db = Database::open(path, Format::Binary)?;
    let read = config.mode == "read";
    let batch = if config.mode == "transaction" {
        config.batch
    } else {
        1
    };
    let operations = if read { config.reads } else { config.writes };
    // Warm preparation/caches before the process barrier, without changing data.
    db.execute("SELECT n,payload FROM counter")?;
    println!("ready");
    std::io::stdout().flush()?;
    let mut start = String::new();
    std::io::stdin().read_line(&mut start)?;
    if start.trim() != "go" {
        return Err("missing start signal".into());
    }
    let mut latency_us = Vec::new();
    let mut busy_retries = 0;
    let mut execute = |db: &mut Database, sql: &str| -> Result<()> {
        let deadline = Instant::now();
        loop {
            match db.execute(sql) {
                Ok(_) => return Ok(()),
                Err(error)
                    if error.result_code() == Some(5)
                        && !db.in_transaction()
                        && deadline.elapsed().as_secs() < 30 =>
                {
                    // Only retry a standalone statement or BEGIN rejected by
                    // SQLite's busy lock. Never replay a transaction body.
                    busy_retries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => return Err(error.into()),
            }
        }
    };
    let begin_work = Instant::now();
    let mut done = 0;
    while done < operations {
        let count = batch.min(operations - done);
        let begin = Instant::now();
        if config.mode == "transaction" {
            execute(&mut db, "BEGIN IMMEDIATE")?;
        }
        for _ in 0..count {
            if read {
                std::hint::black_box(db.execute("SELECT n,payload FROM counter")?);
            } else {
                execute(&mut db, "UPDATE counter SET n=n+1")?;
            }
        }
        if config.mode == "transaction" {
            db.execute("COMMIT")?;
        }
        // Transaction latency includes waiting and commit; divide by its writes.
        latency_us.push(begin.elapsed().as_secs_f64() * 1e6 / count as f64);
        done += count;
    }
    serde_json::to_writer(
        std::io::stdout(),
        &Worker {
            elapsed_us: begin_work.elapsed().as_secs_f64() * 1e6,
            latency_us,
            busy_retries,
        },
    )?;
    Ok(())
}

fn sample(config: &Config, mode: &str, processes: usize, tables: usize) -> Result<Sample> {
    let directory = match &config.directory {
        Some(path) => tempfile::tempdir_in(path)?,
        None => tempfile::tempdir()?,
    };
    let path = directory.path().join("data.db");
    let mut db = Database::open(&path, Format::Binary)?;
    db.execute("CREATE TABLE counter(n INT64 NOT NULL,payload JSON NOT NULL); INSERT INTO counter VALUES(0,'{}')")?;
    for i in 1..tables {
        db.execute(&format!(
            "CREATE TABLE extra_{i}(n INT64 NOT NULL,payload JSON NOT NULL)"
        ))?;
    }
    let executable = std::env::current_exe()?;
    let mut children = Vec::new();
    for _ in 0..processes {
        let mut child = Command::new(&executable)
            .arg("--worker")
            .arg(&path)
            .args([
                "--mode",
                mode,
                "--writes",
                &config.writes.to_string(),
                "--batch",
                &config.batch.to_string(),
                "--reads",
                &config.reads.to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        output.read_line(&mut ready)?;
        if ready.trim() != "ready" {
            return Err(format!("worker failed before barrier: {ready}").into());
        }
        children.push((child, output));
    }
    for (child, _) in &mut children {
        child.stdin.take().unwrap().write_all(b"go\n")?;
    }
    let mut latency_us = Vec::new();
    let mut elapsed_us = 0.0_f64;
    let mut busy_retries = 0;
    let mut failure = None;
    for (mut child, output) in children {
        let report: std::result::Result<Worker, _> = serde_json::from_reader(output);
        let status = child.wait()?;
        match report {
            Ok(report) if status.success() => {
                elapsed_us = elapsed_us.max(report.elapsed_us);
                latency_us.extend(report.latency_us);
                busy_retries += report.busy_retries;
            }
            _ => failure = Some("writer process failed"),
        }
    }
    if let Some(error) = failure {
        return Err(error.into());
    }
    let operations = processes
        * if mode == "read" {
            config.reads
        } else {
            config.writes
        };
    let expected = if mode == "read" { 0 } else { operations as i64 };
    if db.execute("SELECT n FROM counter")?[0].rows != vec![vec![Value::Integer(expected)]] {
        return Err("lost or duplicated updates".into());
    }
    if db.execute("PRAGMA quick_check")?[0].rows != vec![vec![Value::Text("ok".into())]] {
        return Err("quick_check failed".into());
    }
    latency_us.sort_by(f64::total_cmp);
    let percentile = |p: f64| latency_us[(p * latency_us.len() as f64).ceil() as usize - 1];
    Ok(Sample {
        elapsed_us,
        operations,
        operations_per_second: operations as f64 * 1e6 / elapsed_us,
        p50_us: percentile(0.50),
        p95_us: percentile(0.95),
        p99_us: percentile(0.99),
        latency_us,
        busy_retries,
    })
}

fn main() -> Result<()> {
    let config = Config::parse();
    if config.samples == 0 || config.writes == 0 || config.reads == 0 || config.batch == 0 {
        return Err("sample and operation counts must be positive".into());
    }
    if let Some(path) = &config.worker {
        return worker(&config, path);
    }
    let mut cases = Vec::new();
    for tables in [1, 16] {
        for (mode, processes) in [
            ("read", 1),
            ("autocommit", 1),
            ("transaction", 1),
            ("autocommit", 4),
            ("transaction", 4),
        ] {
            sample(&config, mode, processes, tables)?; // discarded warmup
            let mut samples = Vec::new();
            for _ in 0..config.samples {
                samples.push(sample(&config, mode, processes, tables)?);
            }
            eprintln!("{mode}: {processes} process(es), {tables} table(s): complete");
            cases.push(Case {
                mode: mode.into(),
                processes,
                tables,
                samples,
            });
        }
    }
    let report = serde_json::json!({
        "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        "writes_per_process": config.writes,
        "reads_per_process": config.reads,
        "batch_size": config.batch,
        "samples": config.samples,
        "cases": cases,
    });
    let output = config.json.ok_or("--json is required")?;
    std::fs::write(output, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
