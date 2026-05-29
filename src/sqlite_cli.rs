use crate::Result;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const COLUMN_SEPARATOR: &str = "\u{001f}";
const ROW_SEPARATOR: &str = "\u{001e}";
const NULL_MARKER: &str = "__HARNESSKIT_SQLITE_NULL__";

pub struct SqliteConnection {
    db_path: PathBuf,
}

#[derive(Default)]
pub struct QueryResult {
    pub rows: Vec<Vec<Option<String>>>,
}

impl SqliteConnection {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let conn = Self {
            db_path: path.to_path_buf(),
        };
        conn.exec("PRAGMA user_version;")?;
        Ok(conn)
    }

    pub fn exec(&self, sql: &str) -> Result<()> {
        let output = self.run_sql(sql, false)?;
        if !output.status.success() {
            return Err(render_sqlite_error("sqlite exec failed", &output).into());
        }
        Ok(())
    }

    pub fn query(&self, sql: &str) -> Result<QueryResult> {
        let output = self.run_sql(sql, true)?;
        if !output.status.success() {
            return Err(render_sqlite_error("sqlite query failed", &output).into());
        }

        Ok(parse_query_output(&String::from_utf8_lossy(&output.stdout)))
    }

    fn command_base(&self) -> Command {
        let mut command = Command::new("sqlite3");
        command
            .arg(&self.db_path)
            .arg("-batch")
            .arg("-bail")
            .arg("-cmd")
            .arg(".timeout 5000");
        command
    }

    fn run_sql(&self, sql: &str, query_mode: bool) -> Result<std::process::Output> {
        let mut command = self.command_base();
        if query_mode {
            command
                .arg("-header")
                .arg("-separator")
                .arg(COLUMN_SEPARATOR)
                .arg("-newline")
                .arg(ROW_SEPARATOR)
                .arg("-nullvalue")
                .arg(NULL_MARKER);
        }

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                if err.kind() == ErrorKind::NotFound {
                    "sqlite3 CLI not found on PATH. HarnessKit alpha currently requires the system `sqlite3` command for index/query/check/context and other fact-store-backed commands. Install sqlite3 and retry, or run `harnesskit doctor` for environment diagnostics.".to_string()
                } else {
                    format!("failed to start sqlite3 CLI for {}: {}", self.db_path.display(), err)
                }
            })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(sql.as_bytes())?;
        }
        Ok(child.wait_with_output()?)
    }
}

fn parse_query_output(stdout: &str) -> QueryResult {
    let trimmed = stdout.trim_end_matches(ROW_SEPARATOR);
    if trimmed.is_empty() {
        return QueryResult::default();
    }

    let mut records = trimmed.split(ROW_SEPARATOR);
    let _columns = records.next();

    let rows = records
        .filter(|row| !row.is_empty())
        .map(|row| {
            row.split(COLUMN_SEPARATOR)
                .map(|cell| {
                    if cell == NULL_MARKER {
                        None
                    } else {
                        Some(cell.to_string())
                    }
                })
                .collect()
        })
        .collect();

    QueryResult { rows }
}

fn render_sqlite_error(prefix: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if !stderr.is_empty() {
        format!("{}: {}", prefix, stderr)
    } else if !stdout.is_empty() {
        format!("{}: {}", prefix, stdout)
    } else {
        prefix.to_string()
    }
}
